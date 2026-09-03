use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::equivalence::EquivalenceError;
use crate::compile::fragment_synth::candidate::{ExpandedPhysicalCandidate, RouteTarget};
use crate::compile::fragment_synth::certification::{
    CandidateCertificationError, CertifiedCandidate,
};
use crate::compile::fragment_synth::config::SearchConfig;
use crate::compile::fragment_synth::identity::{
    ConnectionId, ImplementationKey, InputMask, InstanceId, PhysicalEndpointId, PortId, RouteId,
    TimingArcId, TimingNodeId,
};
use crate::compile::fragment_synth::instance_graph::{
    DuplicateRequest, InstanceDriver, InstanceRole, PhysicalDriver,
};
use crate::compile::fragment_synth::manifest::Transition;
use crate::compile::fragment_synth::search::{
    CapWorkCounters, ProposalEvaluation, ProposalStream, ProposalTerminal,
};
use crate::compile::fragment_synth::seed::{
    compile_sparse_seed_variant_with_services, InstancePlacementOverride, SeedError, SeedInput,
    SeedRoutingFailure, SeedServices, SeedVariant,
};
use crate::compile::fragment_synth::timing_graph::{
    RealisedTimingGraph, TimingArc, TimingArcKind, TimingGraphError,
};
use crate::compile::fragment_synth::topology::OutputSpec;
use crate::compile::geometry::CellFacing;
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::topology::{GateKind, Library};
use crate::compile::Netlist;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FragmentId {
    pub instances: BTreeSet<InstanceId>,
    pub routes: BTreeSet<RouteId>,
    pub boundary_endpoints: BTreeSet<PhysicalEndpointId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FragmentChoice {
    pub fragment: FragmentId,
    pub hotspot: TimingArcId,
    pub instance: InstanceId,
    pub implementation: ImplementationKey,
    pub facing: CellFacing,
    pub dx: i32,
    pub dz: i32,
    pub shell_ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DuplicateChoice {
    pub fragment: FragmentId,
    pub hotspot: TimingArcId,
    pub request: DuplicateRequest,
}

pub(crate) struct FragmentProposalStream<'a> {
    input: SeedInput<'a>,
    services: SeedServices<'a>,
    variants: BTreeMap<Fingerprint, SeedVariant>,
    next_single_index: u64,
    next_duplicate_index: u64,
}

impl<'a> FragmentProposalStream<'a> {
    pub(crate) fn new(input: SeedInput<'a>, services: SeedServices<'a>) -> Self {
        Self {
            input,
            services,
            variants: BTreeMap::new(),
            next_single_index: 0,
            next_duplicate_index: 0,
        }
    }
}

impl ProposalStream<CertifiedCandidate> for FragmentProposalStream<'_> {
    fn next(
        &mut self,
        proposal_index: u64,
        incumbent: &CertifiedCandidate,
    ) -> Option<ProposalEvaluation<CertifiedCandidate>> {
        let mut variant = self
            .variants
            .get(&incumbent.metrics().candidate_fingerprint)
            .cloned()
            .unwrap_or_default();
        let duplicate = if proposal_index % 2 == 1 {
            duplication_choice(
                incumbent,
                self.input.lowered,
                self.services.search_config,
                &variant,
                self.next_duplicate_index,
            )
            .ok()
            .flatten()
        } else {
            None
        };
        let (fragment_fingerprint, choice_fingerprint, mut cap_work) =
            if let Some(choice) = duplicate {
                self.next_duplicate_index = self.next_duplicate_index.saturating_add(1);
                let fragment_fingerprint = choice.fragment.fingerprint();
                let choice_fingerprint = choice.fingerprint();
                variant.duplicates.push(choice.request);
                (
                    fragment_fingerprint,
                    choice_fingerprint,
                    CapWorkCounters::default(),
                )
            } else {
                let choice = match single_instance_choice(
                    incumbent,
                    self.input.lowered,
                    self.services.library,
                    self.services.search_config,
                    &variant,
                    self.next_single_index,
                ) {
                    Ok(Some(choice)) => choice,
                    Ok(None) => return None,
                    Err(_) => return None,
                };
                self.next_single_index = self.next_single_index.saturating_add(1);
                let fragment_fingerprint = choice.fragment.fingerprint();
                let choice_fingerprint = choice.fingerprint();
                let cap_work = CapWorkCounters {
                    backtracks: choice.shell_ordinal,
                    ..CapWorkCounters::default()
                };
                if choice.shell_ordinal
                    >= self
                        .services
                        .search_config
                        .max_fragment_backtracks_per_proposal
                {
                    return Some(ProposalEvaluation {
                        fragment_fingerprint,
                        choice_fingerprint,
                        terminal: ProposalTerminal::BacktrackCapExhausted,
                        cap_work,
                        certified: None,
                    });
                }
                variant
                    .implementations
                    .insert(choice.instance, choice.implementation);
                variant.placements.insert(
                    choice.instance,
                    InstancePlacementOverride {
                        facing: choice.facing,
                        dx: choice.dx,
                        dz: choice.dz,
                    },
                );
                (fragment_fingerprint, choice_fingerprint, cap_work)
            };
        match compile_sparse_seed_variant_with_services(self.input, self.services, &variant) {
            Ok(certified) => {
                self.variants
                    .insert(certified.metrics().candidate_fingerprint.clone(), variant);
                Some(ProposalEvaluation {
                    fragment_fingerprint,
                    choice_fingerprint,
                    terminal: ProposalTerminal::NoImprovement,
                    cap_work,
                    certified: Some(certified),
                })
            }
            Err(error) => {
                let terminal = terminal_for_seed_error(&error, &mut cap_work);
                Some(ProposalEvaluation {
                    fragment_fingerprint,
                    choice_fingerprint,
                    terminal,
                    cap_work,
                    certified: None,
                })
            }
        }
    }
}

fn terminal_for_seed_error(error: &SeedError, work: &mut CapWorkCounters) -> ProposalTerminal {
    match error {
        SeedError::Routing(SeedRoutingFailure {
            work_used: Some(work_used),
            ..
        }) => {
            work.router_expansions = *work_used;
            ProposalTerminal::RouterCapExhausted
        }
        SeedError::PlacementExhausted { .. } | SeedError::SeedExhausted { .. } => {
            ProposalTerminal::BacktrackCapExhausted
        }
        SeedError::Verification(_) => ProposalTerminal::VerificationFailed,
        SeedError::Certification(CandidateCertificationError::Equivalence(
            EquivalenceError::ProofExhausted { used, .. },
        )) => {
            work.proof_steps = *used;
            ProposalTerminal::ProofCapExhausted
        }
        SeedError::Certification(CandidateCertificationError::TransitionCapExceeded {
            count,
            ..
        }) => {
            work.certification_transitions = *count;
            ProposalTerminal::CertificationCapExhausted
        }
        SeedError::Certification(CandidateCertificationError::SimulatorEventCapExceeded {
            ..
        })
        | SeedError::Certification(CandidateCertificationError::TransitionDidNotSettle {
            ..
        }) => ProposalTerminal::CertificationCapExhausted,
        SeedError::Certification(_) => ProposalTerminal::VerificationFailed,
        SeedError::Routing(_)
        | SeedError::ChannelLayout(_)
        | SeedError::ProvenanceWidth { .. }
        | SeedError::InstanceGraph(_)
        | SeedError::Candidate(_)
        | SeedError::InvalidPins(_)
        | SeedError::PlacementCollision { .. }
        | SeedError::EmptyRoute
        | SeedError::Adapter(_)
        | SeedError::Emission(_)
        | SeedError::IdentityOverflow
        | SeedError::Incomplete(_) => ProposalTerminal::Refused,
    }
}

impl FragmentChoice {
    pub(crate) fn fingerprint(&self) -> Fingerprint {
        #[derive(Serialize)]
        struct ChoiceDescriptor<'a> {
            fragment: &'a FragmentId,
            hotspot: TimingArcId,
            instance: InstanceId,
            implementation: ImplementationKey,
            facing_index: u8,
            dx: i32,
            dz: i32,
            shell_ordinal: u64,
        }
        canonical_fingerprint(
            &serde_json::to_vec(&ChoiceDescriptor {
                fragment: &self.fragment,
                hotspot: self.hotspot,
                instance: self.instance,
                implementation: self.implementation,
                facing_index: self.facing.index(),
                dx: self.dx,
                dz: self.dz,
                shell_ordinal: self.shell_ordinal,
            })
            .expect("fragment choice must serialize canonically"),
        )
    }
}

impl DuplicateChoice {
    pub(crate) fn fingerprint(&self) -> Fingerprint {
        #[derive(Serialize)]
        struct ChoiceDescriptor<'a> {
            fragment: &'a FragmentId,
            hotspot: TimingArcId,
            request: &'a DuplicateRequest,
        }
        canonical_fingerprint(
            &serde_json::to_vec(&ChoiceDescriptor {
                fragment: &self.fragment,
                hotspot: self.hotspot,
                request: &self.request,
            })
            .expect("duplicate choice must serialize canonically"),
        )
    }
}

#[derive(Debug, Error)]
pub(crate) enum FragmentError {
    #[error(transparent)]
    Timing(#[from] TimingGraphError),
    #[error("hotspot arc {arc:?} names missing route {route:?}")]
    MissingRoute { arc: TimingArcId, route: RouteId },
    #[error("hotspot arc {arc:?} cannot be associated with a physical instance or route")]
    UnownedHotspot { arc: TimingArcId },
    #[error("dynamic transition has {actual} inputs, expected {expected}")]
    TransitionWidth { expected: usize, actual: usize },
    #[error("dynamic witness cannot resolve logical signal `{signal}`")]
    MissingSignal { signal: String },
    #[error("dynamic witness does not support gate {gate:?}")]
    UnsupportedGate { gate: GateKind },
    #[error("dynamic witness cannot resolve timing node {node:?}")]
    UnresolvedNode { node: TimingNodeId },
}

impl FragmentId {
    pub(crate) fn from_hotspot(
        candidate: &ExpandedPhysicalCandidate,
        hotspot: TimingArc,
    ) -> Result<Self, FragmentError> {
        let mut fragment = Self {
            instances: BTreeSet::new(),
            routes: BTreeSet::new(),
            boundary_endpoints: BTreeSet::new(),
        };

        if let TimingArcKind::Route { route, .. } = hotspot.kind {
            fragment.close_route(candidate, hotspot.id, route)?;
        } else {
            for node in [hotspot.from, hotspot.to] {
                if let Some(instance) = node_instance(node) {
                    fragment.instances.insert(instance);
                }
            }
            let owned = fragment.instances.clone();
            for route in candidate.routes.values() {
                if route_touches_instances(route, &owned) {
                    fragment.close_route(candidate, hotspot.id, route.id)?;
                }
            }
        }

        if fragment.instances.is_empty() && fragment.routes.is_empty() {
            return Err(FragmentError::UnownedHotspot { arc: hotspot.id });
        }
        Ok(fragment)
    }

    pub(crate) fn fingerprint(&self) -> Fingerprint {
        canonical_fingerprint(
            &serde_json::to_vec(self).expect("fragment identity must serialize canonically"),
        )
    }

    fn close_route(
        &mut self,
        candidate: &ExpandedPhysicalCandidate,
        arc: TimingArcId,
        route_id: RouteId,
    ) -> Result<(), FragmentError> {
        let route = candidate
            .routes
            .get(&route_id)
            .ok_or(FragmentError::MissingRoute {
                arc,
                route: route_id,
            })?;
        self.routes.insert(route_id);
        self.insert_endpoint(route.source);
        for branch in &route.branches {
            match branch.target {
                RouteTarget::Connection(connection) => {
                    self.instances.insert(connection_instance(connection));
                }
                RouteTarget::DeclaredOutput(port) => {
                    self.boundary_endpoints
                        .insert(PhysicalEndpointId::DeclaredOutput(port));
                }
            }
        }
        Ok(())
    }

    fn insert_endpoint(&mut self, endpoint: PhysicalEndpointId) {
        match endpoint {
            PhysicalEndpointId::PrimaryInput(_) | PhysicalEndpointId::DeclaredOutput(_) => {
                self.boundary_endpoints.insert(endpoint);
            }
            PhysicalEndpointId::PrimitiveOutput(primitive) => {
                self.instances.insert(primitive.instance);
            }
            PhysicalEndpointId::Landing(connection) => {
                self.instances.insert(connection_instance(connection));
            }
            PhysicalEndpointId::Junction(instance) => {
                self.instances.insert(instance);
            }
        }
    }
}

pub(crate) fn rank_hotspots(
    graph: &RealisedTimingGraph,
    dynamically_active: &BTreeSet<TimingArcId>,
) -> Result<Vec<TimingArcId>, TimingGraphError> {
    let timing = graph.analyse()?;
    let mut arcs = graph.arcs.keys().copied().collect::<Vec<_>>();
    arcs.sort_by_key(|arc| {
        (
            timing.slack.get(arc).copied().unwrap_or_default(),
            !dynamically_active.contains(arc),
            *arc,
        )
    });
    Ok(arcs)
}

pub(crate) fn active_arcs_for_transition(
    candidate: &ExpandedPhysicalCandidate,
    graph: &RealisedTimingGraph,
    lowered: &Netlist,
    transition: &Transition,
) -> Result<BTreeSet<TimingArcId>, FragmentError> {
    let from = logical_values(lowered, &transition.from)?;
    let to = logical_values(lowered, &transition.to)?;
    graph
        .arcs
        .values()
        .filter_map(|arc| {
            let before = node_value(candidate, lowered, arc.to, &from);
            let after = node_value(candidate, lowered, arc.to, &to);
            match (before, after) {
                (Ok(before), Ok(after)) => (before != after).then_some(Ok(arc.id)),
                (Err(error), _) | (_, Err(error)) => Some(Err(error)),
            }
        })
        .collect()
}

pub(crate) fn single_instance_choice(
    incumbent: &CertifiedCandidate,
    lowered: &Netlist,
    library: &Library,
    config: &SearchConfig,
    incumbent_variant: &SeedVariant,
    proposal_index: u64,
) -> Result<Option<FragmentChoice>, FragmentError> {
    let mut dynamically_active = BTreeSet::new();
    for &index in &incumbent.metrics().worst_transition_indices {
        if let Some(transition) = incumbent.manifest().transitions().get(index) {
            dynamically_active.extend(active_arcs_for_transition(
                incumbent.candidate(),
                incumbent.timing_graph(),
                lowered,
                transition,
            )?);
        }
    }
    let ranked = rank_hotspots(incumbent.timing_graph(), &dynamically_active)?;
    let mut target_seen = BTreeSet::new();
    let mut remaining = proposal_index;
    let radius = config
        .max_fragment_shell_radius
        .min(config.max_fragment_manhattan_radius);
    let offsets = horizontal_shell_offsets(radius);

    for hotspot in ranked {
        let arc = incumbent.timing_graph().arcs[&hotspot];
        let fragment = match FragmentId::from_hotspot(incumbent.candidate(), arc) {
            Ok(fragment) => fragment,
            Err(FragmentError::UnownedHotspot { .. }) => continue,
            Err(error) => return Err(error),
        };
        if fragment.boundary_endpoints.len() > usize::from(config.max_boundary_nets) {
            continue;
        }
        let Some(instance) = node_instance(arc.to)
            .or_else(|| node_instance(arc.from))
            .or_else(|| fragment.instances.first().copied())
        else {
            continue;
        };
        if !target_seen.insert(instance) {
            continue;
        }
        let Some(parent) = incumbent
            .candidate()
            .instances
            .instances
            .iter()
            .find(|candidate| {
                candidate.id == instance && candidate.role == InstanceRole::Canonical
            })
        else {
            continue;
        };
        let gate = &lowered.gates[usize::try_from(parent.logical_gate.0)
            .map_err(|_| FragmentError::UnownedHotspot { arc: hotspot })?];
        let implementations = implementation_choices(gate.kind, library);
        let parent_facing = instance_facing(incumbent.candidate(), parent)
            .ok_or(FragmentError::UnownedHotspot { arc: hotspot })?;
        let parent_offset = incumbent_variant
            .placements
            .get(&instance)
            .map(|placement| (placement.dx, placement.dz))
            .unwrap_or((0, 0));
        for implementation in implementations {
            for facing in [
                CellFacing::NORTH,
                CellFacing::EAST,
                CellFacing::SOUTH,
                CellFacing::WEST,
            ] {
                for (shell_ordinal, &(dx, dz)) in offsets.iter().enumerate() {
                    if implementation == parent.implementation
                        && facing == parent_facing
                        && (dx, dz) == parent_offset
                    {
                        continue;
                    }
                    if remaining == 0 {
                        return Ok(Some(FragmentChoice {
                            fragment,
                            hotspot,
                            instance,
                            implementation,
                            facing,
                            dx,
                            dz,
                            shell_ordinal: u64::try_from(shell_ordinal).unwrap_or(u64::MAX),
                        }));
                    }
                    remaining -= 1;
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn duplication_choice(
    incumbent: &CertifiedCandidate,
    lowered: &Netlist,
    config: &SearchConfig,
    incumbent_variant: &SeedVariant,
    proposal_index: u64,
) -> Result<Option<DuplicateChoice>, FragmentError> {
    let mut dynamically_active = BTreeSet::new();
    for &index in &incumbent.metrics().worst_transition_indices {
        if let Some(transition) = incumbent.manifest().transitions().get(index) {
            dynamically_active.extend(active_arcs_for_transition(
                incumbent.candidate(),
                incumbent.timing_graph(),
                lowered,
                transition,
            )?);
        }
    }
    let ranked = rank_hotspots(incumbent.timing_graph(), &dynamically_active)?;
    let mut canonical_seen = BTreeSet::new();
    let mut remaining = proposal_index;
    for hotspot in ranked {
        let arc = incumbent.timing_graph().arcs[&hotspot];
        let fragment = match FragmentId::from_hotspot(incumbent.candidate(), arc) {
            Ok(fragment) => fragment,
            Err(FragmentError::UnownedHotspot { .. }) => continue,
            Err(error) => return Err(error),
        };
        if fragment.boundary_endpoints.len() > usize::from(config.max_boundary_nets) {
            continue;
        }
        for &canonical in &fragment.instances {
            if !canonical_seen.insert(canonical) {
                continue;
            }
            let Some(instance) =
                incumbent
                    .candidate()
                    .instances
                    .instances
                    .iter()
                    .find(|instance| {
                        instance.id == canonical && instance.role == InstanceRole::Canonical
                    })
            else {
                continue;
            };
            if !matches!(instance.expanded.topology.output, OutputSpec::Primitive(_)) {
                continue;
            }
            let sinks = incumbent
                .candidate()
                .instances
                .assignments
                .iter()
                .filter(|assignment| driver_owner(&assignment.driver) == Some(canonical))
                .map(|assignment| assignment.sink)
                .collect::<Vec<_>>();
            if sinks.len() < 2 {
                continue;
            }
            let ordinal = incumbent_variant
                .duplicates
                .iter()
                .filter(|request| request.canonical == canonical)
                .map(|request| request.ordinal)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            for split in 1..sinks.len() {
                if remaining == 0 {
                    return Ok(Some(DuplicateChoice {
                        fragment,
                        hotspot,
                        request: DuplicateRequest {
                            canonical,
                            ordinal,
                            sinks: sinks[split..].iter().copied().collect(),
                        },
                    }));
                }
                remaining = remaining.saturating_sub(1);
            }
        }
    }
    Ok(None)
}

fn driver_owner(driver: &PhysicalDriver) -> Option<InstanceId> {
    match driver {
        PhysicalDriver::PrimaryInput(_) => None,
        PhysicalDriver::Instance(InstanceDriver::Primitive { logical_owner, .. })
        | PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
            Some(*logical_owner)
        }
    }
}

fn implementation_choices(kind: GateKind, library: &Library) -> Vec<ImplementationKey> {
    if let GateKind::Or(arity) = kind {
        let count = 1u64
            .checked_shl(u32::try_from(arity).unwrap_or(u32::MAX))
            .unwrap_or(0);
        return (0..count)
            .map(|bits| ImplementationKey::Merge {
                isolation_mask: InputMask::new(bits),
            })
            .collect();
    }
    (0..library.entries_for(kind).len())
        .filter_map(|ordinal| {
            library
                .entry_id_at(kind, ordinal)
                .map(ImplementationKey::Library)
        })
        .collect()
}

fn instance_facing(
    candidate: &ExpandedPhysicalCandidate,
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
) -> Option<CellFacing> {
    match &instance.expanded.topology.output {
        OutputSpec::Primitive(primitive) => candidate
            .placements
            .get(primitive)
            .map(|placement| placement.facing),
        OutputSpec::Junction { .. } => candidate
            .junctions
            .get(&instance.id)
            .map(|junction| junction.facing),
    }
}

fn horizontal_shell_offsets(max_radius: u32) -> Vec<(i32, i32)> {
    let mut offsets = vec![(0, 0)];
    for radius in 1..=max_radius {
        let radius = i32::try_from(radius).unwrap_or(i32::MAX);
        for dx in -radius..=radius {
            let dz = radius - dx.abs();
            offsets.push((dx, -dz));
            if dz != 0 {
                offsets.push((dx, dz));
            }
        }
    }
    offsets
}

fn logical_values(
    lowered: &Netlist,
    vector: &[bool],
) -> Result<BTreeMap<String, bool>, FragmentError> {
    if vector.len() != lowered.inputs.len() {
        return Err(FragmentError::TransitionWidth {
            expected: lowered.inputs.len(),
            actual: vector.len(),
        });
    }
    let mut values = lowered
        .inputs
        .iter()
        .cloned()
        .zip(vector.iter().copied())
        .collect::<BTreeMap<_, _>>();
    let order = lowered
        .topological_order()
        .ok_or(FragmentError::UnresolvedNode {
            node: TimingNodeId::InputBoundary(PortId(0)),
        })?;
    for gate_index in order {
        let gate = &lowered.gates[gate_index];
        let inputs = gate
            .inputs
            .iter()
            .map(|name| {
                values
                    .get(name)
                    .copied()
                    .ok_or_else(|| FragmentError::MissingSignal {
                        signal: name.clone(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let output = match gate.kind {
            GateKind::Nor(_) => !inputs.into_iter().any(|value| value),
            GateKind::Or(_) => inputs.into_iter().any(|value| value),
            GateKind::Buf => inputs.first().copied().unwrap_or(false),
            other => return Err(FragmentError::UnsupportedGate { gate: other }),
        };
        values.insert(gate.output.clone(), output);
    }
    Ok(values)
}

fn node_value(
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    node: TimingNodeId,
    values: &BTreeMap<String, bool>,
) -> Result<bool, FragmentError> {
    let signal = match node {
        TimingNodeId::InputBoundary(port) | TimingNodeId::PrimaryInput(port) => lowered
            .inputs
            .get(usize::try_from(port.0).unwrap_or(usize::MAX)),
        TimingNodeId::OutputLanding(port) | TimingNodeId::DeclaredOutput(port) => lowered
            .outputs
            .get(usize::try_from(port.0).unwrap_or(usize::MAX)),
        TimingNodeId::Landing(ConnectionId::External {
            instance,
            input_index,
        }) => candidate
            .instances
            .instances
            .iter()
            .find(|physical| physical.id == instance)
            .and_then(|physical| {
                lowered
                    .gates
                    .get(usize::try_from(physical.logical_gate.0).ok()?)
            })
            .and_then(|gate| gate.inputs.get(usize::from(input_index))),
        TimingNodeId::Landing(ConnectionId::Internal { instance, .. })
        | TimingNodeId::InstanceOutput(instance)
        | TimingNodeId::JunctionOutput(instance) => {
            instance_output_name(candidate, lowered, instance)
        }
        TimingNodeId::PrimitiveOutput(primitive) => {
            instance_output_name(candidate, lowered, primitive.instance)
        }
    }
    .ok_or(FragmentError::UnresolvedNode { node })?;
    values
        .get(signal)
        .copied()
        .ok_or_else(|| FragmentError::MissingSignal {
            signal: signal.clone(),
        })
}

fn instance_output_name<'a>(
    candidate: &ExpandedPhysicalCandidate,
    lowered: &'a Netlist,
    instance: InstanceId,
) -> Option<&'a String> {
    candidate
        .instances
        .instances
        .iter()
        .find(|physical| physical.id == instance)
        .and_then(|physical| {
            lowered
                .gates
                .get(usize::try_from(physical.logical_gate.0).ok()?)
        })
        .map(|gate| &gate.output)
}

fn node_instance(node: TimingNodeId) -> Option<InstanceId> {
    match node {
        TimingNodeId::Landing(connection) => Some(connection_instance(connection)),
        TimingNodeId::PrimitiveOutput(primitive) => Some(primitive.instance),
        TimingNodeId::InstanceOutput(instance) | TimingNodeId::JunctionOutput(instance) => {
            Some(instance)
        }
        TimingNodeId::InputBoundary(_)
        | TimingNodeId::PrimaryInput(_)
        | TimingNodeId::OutputLanding(_)
        | TimingNodeId::DeclaredOutput(_) => None,
    }
}

fn connection_instance(connection: ConnectionId) -> InstanceId {
    match connection {
        ConnectionId::External { instance, .. } | ConnectionId::Internal { instance, .. } => {
            instance
        }
    }
}

fn route_touches_instances(
    route: &crate::compile::routing::RealisedRouteTree,
    instances: &BTreeSet<InstanceId>,
) -> bool {
    endpoint_instance(route.source).is_some_and(|instance| instances.contains(&instance))
        || route.branches.iter().any(|branch| match branch.target {
            RouteTarget::Connection(connection) => {
                instances.contains(&connection_instance(connection))
            }
            RouteTarget::DeclaredOutput(_) => false,
        })
}

fn endpoint_instance(endpoint: PhysicalEndpointId) -> Option<InstanceId> {
    match endpoint {
        PhysicalEndpointId::PrimitiveOutput(primitive) => Some(primitive.instance),
        PhysicalEndpointId::Landing(connection) => Some(connection_instance(connection)),
        PhysicalEndpointId::Junction(instance) => Some(instance),
        PhysicalEndpointId::PrimaryInput(_) | PhysicalEndpointId::DeclaredOutput(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        active_arcs_for_transition, duplication_choice, rank_hotspots, single_instance_choice,
        FragmentId,
    };
    use crate::compile::equivalence::EquivalenceError;
    use crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate;
    use crate::compile::fragment_synth::certification::{
        CandidateCertificationError, CertifiedCandidate, CompleteCandidateCertifier,
        ExpandedCandidateCertifier,
    };
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::identity::{
        ConnectionId, InstanceId, ObservationId, PhysicalEndpointId, PortId, TimingArcId,
        TimingNodeId,
    };
    use crate::compile::fragment_synth::manifest::Transition;
    use crate::compile::fragment_synth::search::{
        run_budgeted_proposals, ProposalStream, ProposalTerminal, SynthesisBudget,
        SystemMonotonicClock,
    };
    use crate::compile::fragment_synth::seed::{
        compile_sparse_seed_with_services, SeedInput, SeedServices,
    };
    use crate::compile::fragment_synth::services::{
        DurableSeedEmitter, DurableSeedVerifier, SeedVerifier, TopologyAwareSeedPlacer,
    };
    use crate::compile::fragment_synth::timing_graph::{
        ExactDelay, RealisedTimingGraph, TimingArc, TimingArcKind,
    };
    use crate::compile::geometry::Anchor;
    use crate::compile::routing::{
        DurablePhysicalRouter, PhysicalRouter, RealisedRouteTree, RouteRequest, RouterFailure,
        RouterLimitKind,
    };
    use crate::compile::topology::Library;
    use crate::compile::verification::ExpandedPhysicalError;
    use crate::compile::{Gate, Netlist};

    #[test]
    fn active_tied_worst_arcs_rank_first_with_stable_static_fallback() {
        let input = TimingNodeId::PrimaryInput(PortId(0));
        let left = TimingNodeId::Landing(ConnectionId::External {
            instance: InstanceId(0),
            input_index: 0,
        });
        let right = TimingNodeId::Landing(ConnectionId::External {
            instance: InstanceId(1),
            input_index: 0,
        });
        let graph = RealisedTimingGraph::new(
            [input, left, right],
            [
                TimingArc::new(
                    TimingArcId(0),
                    input,
                    left,
                    TimingArcKind::InputBinding,
                    ExactDelay(4),
                ),
                TimingArc::new(
                    TimingArcId(1),
                    input,
                    right,
                    TimingArcKind::InputBinding,
                    ExactDelay(4),
                ),
            ],
        )
        .unwrap();

        assert_eq!(
            rank_hotspots(&graph, &BTreeSet::from([TimingArcId(1)])).unwrap(),
            [TimingArcId(1), TimingArcId(0)]
        );
        assert_eq!(
            rank_hotspots(&graph, &BTreeSet::new()).unwrap(),
            [TimingArcId(0), TimingArcId(1)]
        );
    }

    #[test]
    fn a_hot_route_branch_closes_over_the_whole_shared_fanout_tree() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![Gate::nor("left", &["a"]), Gate::nor("right", &["a"])],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();
        let route = certified
            .candidate()
            .routes
            .values()
            .find(|route| {
                route.source == PhysicalEndpointId::PrimaryInput(PortId(0))
                    && route.branches.len() == 2
            })
            .expect("the primary input must own one two-sink fanout tree");
        let hot_arc = certified
            .timing_graph()
            .arcs
            .values()
            .find(|arc| {
                matches!(
                    arc.kind,
                    TimingArcKind::Route { route: id, .. } if id == route.id
                )
            })
            .copied()
            .unwrap();

        let fragment = FragmentId::from_hotspot(certified.candidate(), hot_arc).unwrap();

        assert_eq!(fragment.routes, BTreeSet::from([route.id]));
        assert_eq!(
            fragment.instances,
            BTreeSet::from([InstanceId(0), InstanceId(1)])
        );
        assert_eq!(
            fragment.boundary_endpoints,
            BTreeSet::from([PhysicalEndpointId::PrimaryInput(PortId(0))])
        );
    }

    #[test]
    fn dynamic_witnesses_exclude_an_unsensitised_gate_output_path() {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a", "b"])],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();
        let transition = Transition {
            from: vec![true, false],
            to: vec![true, true],
        };

        let active = active_arcs_for_transition(
            certified.candidate(),
            certified.timing_graph(),
            &netlist,
            &transition,
        )
        .unwrap();
        let b_route = certified
            .timing_graph()
            .arcs
            .values()
            .find(|arc| {
                arc.from == TimingNodeId::PrimaryInput(PortId(1))
                    && matches!(arc.kind, TimingArcKind::Route { .. })
            })
            .unwrap();
        let primitive = certified
            .timing_graph()
            .arcs
            .values()
            .find(|arc| matches!(arc.kind, TimingArcKind::Primitive { .. }))
            .unwrap();

        assert!(active.contains(&b_route.id));
        assert!(!active.contains(&primitive.id));
    }

    #[test]
    fn single_instance_choices_are_stable_bounded_and_never_repeat_the_parent_choice() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let library = Library::default_library();
        let mut config = SearchConfig::checked_defaults();
        config.max_fragment_shell_radius = 1;
        config.max_fragment_manhattan_radius = 1;
        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();

        let incumbent_variant = super::SeedVariant::default();
        let first = single_instance_choice(
            &certified,
            &netlist,
            &library,
            &config,
            &incumbent_variant,
            0,
        )
        .unwrap()
        .unwrap();
        let repeated = single_instance_choice(
            &certified,
            &netlist,
            &library,
            &config,
            &incumbent_variant,
            0,
        )
        .unwrap()
        .unwrap();
        let parent = &certified.candidate().instances.instances[0];
        let parent_facing = certified
            .candidate()
            .placements
            .values()
            .next()
            .unwrap()
            .facing;

        assert_eq!(first.fingerprint(), repeated.fingerprint());
        assert_eq!(first.instance, InstanceId(0));
        assert!(first.dx.unsigned_abs() + first.dz.unsigned_abs() <= 1);
        assert_ne!(
            (first.implementation, first.facing, first.dx, first.dz),
            (parent.implementation, parent_facing, 0, 0)
        );
    }

    #[test]
    fn a_single_instance_transaction_returns_a_complete_candidate_without_mutating_parent_state() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let library = Library::default_library();
        let mut config = SearchConfig::checked_defaults();
        config.max_fragment_shell_radius = 1;
        config.max_fragment_manhattan_radius = 1;
        let services = SeedServices {
            library: &library,
            placer: &TopologyAwareSeedPlacer,
            router: &DurablePhysicalRouter,
            emitter: &DurableSeedEmitter,
            verifier: &DurableSeedVerifier,
            certifier: &CompleteCandidateCertifier,
            search_config: &config,
        };
        let input = SeedInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let parent = compile_sparse_seed_with_services(input, services).unwrap();
        let parent_fingerprint = parent.metrics().candidate_fingerprint.clone();
        let parent_quality = parent.metrics().quality;
        let mut proposals = super::FragmentProposalStream::new(input, services);

        let result = run_budgeted_proposals(
            parent,
            SynthesisBudget::Evaluations(1),
            &SystemMonotonicClock::start(),
            &mut proposals,
        );

        assert_eq!(result.trace.len(), 1);
        assert_eq!(result.trace[0].parent_fingerprint, parent_fingerprint);
        assert!(matches!(
            result.trace[0].terminal,
            ProposalTerminal::NoImprovement | ProposalTerminal::Accepted
        ));
        assert!(result.best.metrics().quality <= parent_quality);
    }

    #[test]
    fn a_later_transaction_accumulates_the_certified_incumbent_variant() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("mid", &["a"]), Gate::nor("y", &["mid"])],
        };
        let library = Library::default_library();
        let mut config = SearchConfig::checked_defaults();
        config.max_fragment_shell_radius = 1;
        config.max_fragment_manhattan_radius = 1;
        let services = SeedServices {
            library: &library,
            placer: &TopologyAwareSeedPlacer,
            router: &DurablePhysicalRouter,
            emitter: &DurableSeedEmitter,
            verifier: &DurableSeedVerifier,
            certifier: &CompleteCandidateCertifier,
            search_config: &config,
        };
        let input = SeedInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let parent = compile_sparse_seed_with_services(input, services).unwrap();
        let mut proposals = super::FragmentProposalStream::new(input, services);
        let first = proposals.next(0, &parent).unwrap().certified.unwrap();
        let first_variant = proposals.variants[&first.metrics().candidate_fingerprint].clone();
        let first_instance = *first_variant.placements.keys().next().unwrap();
        let second_index = (1..100)
            .find(|&index| {
                single_instance_choice(&first, &netlist, &library, &config, &first_variant, index)
                    .unwrap()
                    .is_some_and(|choice| choice.instance != first_instance)
            })
            .expect("the bounded stream must eventually visit the other instance");

        proposals.next_single_index = second_index;
        let second = proposals.next(2, &first).unwrap().certified.unwrap();
        let accumulated = &proposals.variants[&second.metrics().candidate_fingerprint];

        assert_eq!(accumulated.placements.len(), 2);
        assert!(accumulated.placements.contains_key(&first_instance));
    }

    #[test]
    fn duplicate_choices_partition_fanout_stably_and_run_as_certified_transactions() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                Gate::nor("shared", &["a"]),
                Gate::nor("left", &["shared"]),
                Gate::nor("right", &["shared"]),
            ],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let services = SeedServices {
            library: &library,
            placer: &TopologyAwareSeedPlacer,
            router: &DurablePhysicalRouter,
            emitter: &DurableSeedEmitter,
            verifier: &DurableSeedVerifier,
            certifier: &CompleteCandidateCertifier,
            search_config: &config,
        };
        let input = SeedInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let parent = compile_sparse_seed_with_services(input, services).unwrap();
        let variant = super::SeedVariant::default();
        let first = duplication_choice(&parent, &netlist, &config, &variant, 0)
            .unwrap()
            .unwrap();
        let repeated = duplication_choice(&parent, &netlist, &config, &variant, 0)
            .unwrap()
            .unwrap();

        assert_eq!(first.fingerprint(), repeated.fingerprint());
        assert_eq!(first.request.canonical, InstanceId(0));
        assert_eq!(first.request.ordinal, 1);
        assert!(!first.request.sinks.is_empty());

        let mut proposals = super::FragmentProposalStream::new(input, services);
        let certified = proposals.next(1, &parent).unwrap().certified.unwrap();
        let committed = &proposals.variants[&certified.metrics().candidate_fingerprint];

        assert_eq!(committed.duplicates, vec![first.request]);
        assert_eq!(certified.candidate().instances.instances.len(), 4);
        // The seed's channel plan already gives a two-sink fanout a straight
        // trunk with no extra repeater, so the duplicate can only match the
        // parent's settle time; the transaction must still certify and must
        // never make it worse.
        assert!(
            certified.metrics().quality.observed_settle <= parent.metrics().quality.observed_settle
        );

        let parent = compile_sparse_seed_with_services(input, services).unwrap();
        let parent_quality = parent.metrics().quality;
        let mut budgeted = super::FragmentProposalStream::new(input, services);
        let result = run_budgeted_proposals(
            parent,
            SynthesisBudget::Evaluations(2),
            &SystemMonotonicClock::start(),
            &mut budgeted,
        );
        assert!(!result.trace.is_empty());
        assert!(result.best.metrics().quality <= parent_quality);
    }

    #[test]
    fn compact_fanout_duplication_is_certified_but_not_an_improvement() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into(), "y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let services = SeedServices {
            library: &library,
            placer: &TopologyAwareSeedPlacer,
            router: &DurablePhysicalRouter,
            emitter: &DurableSeedEmitter,
            verifier: &DurableSeedVerifier,
            certifier: &CompleteCandidateCertifier,
            search_config: &config,
        };
        let input = SeedInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let parent = compile_sparse_seed_with_services(input, services).unwrap();
        let mut proposals = super::FragmentProposalStream::new(input, services);
        let duplicate = proposals.next(1, &parent).unwrap().certified.unwrap();

        assert_eq!(duplicate.candidate().instances.instances.len(), 2);
        assert!(duplicate.metrics().quality >= parent.metrics().quality);
    }

    struct CappedRouter;

    impl PhysicalRouter for CappedRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            Err(RouterFailure::RouterLimitExceeded {
                route: request.id,
                source: request.source.id,
                sink: request.sinks.as_slice()[0].id,
                kind: RouterLimitKind::NodeExpansions,
                limit: 0,
                work_used: 1,
            })
        }
    }

    struct RefusingVerifier;

    impl SeedVerifier for RefusingVerifier {
        fn verify(
            &self,
            _candidate: &ExpandedPhysicalCandidate,
            _emitted: &crate::compile::emission::EmittedWorld,
        ) -> Result<(), ExpandedPhysicalError> {
            Err(ExpandedPhysicalError::ObservationMismatch {
                observation: ObservationId::PrimaryInput(PortId(0)),
                at: Anchor { x: 0, y: 0, z: 0 },
            })
        }
    }

    struct ProofCappedCertifier;

    impl ExpandedCandidateCertifier for ProofCappedCertifier {
        fn certify(
            &self,
            _candidate: ExpandedPhysicalCandidate,
            _lowered: &Netlist,
            _library: &Library,
            _config: &crate::compile::fragment_synth::config::CertificationConfig,
        ) -> Result<CertifiedCandidate, CandidateCertificationError> {
            Err(EquivalenceError::ProofExhausted { used: 1, limit: 0 }.into())
        }
    }

    struct TransitionCappedCertifier;

    impl ExpandedCandidateCertifier for TransitionCappedCertifier {
        fn certify(
            &self,
            _candidate: ExpandedPhysicalCandidate,
            _lowered: &Netlist,
            _library: &Library,
            _config: &crate::compile::fragment_synth::config::CertificationConfig,
        ) -> Result<CertifiedCandidate, CandidateCertificationError> {
            Err(CandidateCertificationError::TransitionCapExceeded { count: 1, limit: 0 })
        }
    }

    fn assert_failed_transaction_is_atomic(
        router: &dyn PhysicalRouter,
        verifier: &dyn SeedVerifier,
        certifier: &dyn ExpandedCandidateCertifier,
        config: &SearchConfig,
        expected_terminal: ProposalTerminal,
    ) {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let library = Library::default_library();
        let input = SeedInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let parent_config = SearchConfig::checked_defaults();
        let parent = compile_sparse_seed_with_services(
            input,
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &parent_config,
            },
        )
        .unwrap();
        let parent_candidate = parent.metrics().candidate_fingerprint.clone();
        let parent_timing_graph = parent.metrics().realised_timing_graph_fingerprint.clone();
        let mut proposals = super::FragmentProposalStream::new(
            input,
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router,
                emitter: &DurableSeedEmitter,
                verifier,
                certifier,
                search_config: config,
            },
        );

        let result = run_budgeted_proposals(
            parent,
            SynthesisBudget::Evaluations(1),
            &SystemMonotonicClock::start(),
            &mut proposals,
        );

        assert_eq!(result.trace.len(), 1);
        assert_eq!(result.trace[0].terminal, expected_terminal);
        assert!(!result.trace[0].accepted);
        assert_eq!(result.trace[0].parent_fingerprint, parent_candidate);
        assert_eq!(
            result.best.metrics().candidate_fingerprint,
            parent_candidate
        );
        assert_eq!(
            result.best.metrics().realised_timing_graph_fingerprint,
            parent_timing_graph
        );
    }

    #[test]
    fn every_bounded_fragment_failure_keeps_the_parent_candidate_atomic() {
        let mut config = SearchConfig::checked_defaults();
        config.max_fragment_shell_radius = 1;
        config.max_fragment_manhattan_radius = 1;

        assert_failed_transaction_is_atomic(
            &CappedRouter,
            &DurableSeedVerifier,
            &CompleteCandidateCertifier,
            &config,
            ProposalTerminal::RouterCapExhausted,
        );
        assert_failed_transaction_is_atomic(
            &DurablePhysicalRouter,
            &RefusingVerifier,
            &CompleteCandidateCertifier,
            &config,
            ProposalTerminal::VerificationFailed,
        );
        assert_failed_transaction_is_atomic(
            &DurablePhysicalRouter,
            &DurableSeedVerifier,
            &ProofCappedCertifier,
            &config,
            ProposalTerminal::ProofCapExhausted,
        );
        assert_failed_transaction_is_atomic(
            &DurablePhysicalRouter,
            &DurableSeedVerifier,
            &TransitionCappedCertifier,
            &config,
            ProposalTerminal::CertificationCapExhausted,
        );

        config.max_seed_shell_radius = 0;
        assert_failed_transaction_is_atomic(
            &DurablePhysicalRouter,
            &DurableSeedVerifier,
            &CompleteCandidateCertifier,
            &config,
            ProposalTerminal::BacktrackCapExhausted,
        );

        config.max_seed_shell_radius = SearchConfig::checked_defaults().max_seed_shell_radius;
        config.max_fragment_backtracks_per_proposal = 0;
        assert_failed_transaction_is_atomic(
            &DurablePhysicalRouter,
            &DurableSeedVerifier,
            &CompleteCandidateCertifier,
            &config,
            ProposalTerminal::BacktrackCapExhausted,
        );
    }
}
