#![allow(dead_code)] // Task 9 is the first production caller of this Task-8 seam.

//! Independent deterministic sparse-seed construction.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::emission::EmissionError;
use crate::compile::fragment_synth::candidate::{
    endpoint_for_driver, BoundaryPlacement, CandidateError, ConnectionBinding,
    ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedJunction,
    VerifiedObservation,
};
use crate::compile::fragment_synth::certification::{
    CandidateCertificationError, CertifiedCandidate, ExpandedCandidateCertifier,
};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::identity::{
    ConnectionId, ImplementationKey, InstanceId, ObservationId, ObservationSite,
    PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::instance_graph::{
    DuplicateRequest, InstanceGraph, PhysicalDriver, PhysicalSink, SynthesisError,
};
use crate::compile::fragment_synth::placement::{
    analyse_instance_dag, LayoutOwner, LayoutRepair, RunwayDirection, SeedPlacementAnalysis,
    SeedPlacementError, SeedPlacementPlan, SeedPlacementRequest, SeedPlacer, SeparationAxis,
};
use crate::compile::fragment_synth::realise::{ExpandedAdapterError, ExpandedCandidateAdapter};
use crate::compile::fragment_synth::route_schedule::{
    RouteObligation, RouteSchedule, RouteScheduleError, TargetObligation,
};
use crate::compile::fragment_synth::services::{SeedEmitter, SeedVerifier};
use crate::compile::fragment_synth::terminal_geometry::{
    primitive_input_terminal, primitive_output_terminal, source_escape_corridor,
    PrimitiveTerminalError,
};
use crate::compile::fragment_synth::topology::{
    ConnectionSource, ConnectionTarget, ContributorSpec, OutputSpec,
};
use crate::compile::geometry::{self, Anchor, CellFacing};
use crate::compile::metrics::Fingerprint;
use crate::compile::physical::{self, PortKind};
use crate::compile::planner::{PortPlacements, PortRole};
use crate::compile::routing::{
    DelayedComponent, DelayedOwner, NonEmptyRouteSinks, PhysicalReservationKind,
    PhysicalReservationOwner, PhysicalReservations, PhysicalRouter, RouteEndpoint, RouteGuidance,
    RouteRequest, RouteSink, RouterFailure, RouterLimitKind, RouterRefusalCategory,
    TerminalContract, TerminalRequirement,
};
use crate::compile::topology::{Library, Primitive};
use crate::compile::verification::ExpandedPhysicalError;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};

const ORIGIN_WORLD_MARGIN: i32 = 16;
const MAX_LAYOUT_REPAIR_ATTEMPTS: u64 = 16;
const MIN_ROUTE_PRECEDENCE_REPAIRS: u64 = 16;
const MAX_ROUTE_PRECEDENCE_REPAIRS: u64 = 128;

#[derive(Clone, Copy)]
pub(crate) struct SeedInput<'a> {
    pub lowered: &'a Netlist,
    pub source_provenance: Option<&'a [usize]>,
    pub pins: Option<&'a PortPlacements>,
}

#[derive(Clone, Copy)]
pub(crate) struct SeedServices<'a> {
    pub library: &'a Library,
    pub placer: &'a dyn SeedPlacer,
    pub router: &'a dyn PhysicalRouter,
    pub emitter: &'a dyn SeedEmitter,
    pub verifier: &'a dyn SeedVerifier,
    pub certifier: &'a dyn ExpandedCandidateCertifier,
    pub search_config: &'a SearchConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InstancePlacementOverride {
    pub facing: CellFacing,
    pub dx: i32,
    pub dz: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SeedVariant {
    pub implementations: BTreeMap<InstanceId, ImplementationKey>,
    pub placements: BTreeMap<InstanceId, InstancePlacementOverride>,
    pub duplicates: Vec<DuplicateRequest>,
}

#[derive(Debug, Error)]
pub(crate) enum SeedError {
    #[error("source provenance has {actual} entries for {expected} lowered gates")]
    ProvenanceWidth { expected: usize, actual: usize },
    #[error("instance graph construction failed: {0}")]
    InstanceGraph(#[from] SynthesisError),
    #[error("candidate construction failed: {0}")]
    Candidate(#[from] CandidateError),
    #[error("invalid pinned IO: {0}")]
    InvalidPins(#[source] crate::compile::planner::PlannerError),
    #[error("physical placement at {at:?} overlaps another seed component")]
    PlacementCollision { at: Anchor },
    #[error("seed placement planning failed: {0}")]
    PlacementPlan(#[from] SeedPlacementError),
    #[error(
        "seed placement exhausted at instance {instance:?}, primitive {primitive:?}, radius {radius}"
    )]
    PlacementExhausted {
        instance: InstanceId,
        primitive: PrimitiveId,
        radius: u32,
    },
    #[error("typed route construction failed: {0}")]
    Routing(#[source] SeedRoutingFailure),
    #[error("route schedule construction failed: {0}")]
    RouteSchedule(#[from] RouteScheduleError),
    #[error("repairable seed attempt failed: {0}")]
    Repairable(#[source] SeedRepairRefusal),
    #[error("seed repair exhausted after {attempts_used} attempts: {final_refusal}")]
    SeedExhausted {
        attempts_used: u64,
        final_refusal: SeedRepairRefusal,
    },
    #[error("typed route sink set was unexpectedly empty")]
    EmptyRoute,
    #[error("expanded candidate adaptation failed: {0}")]
    Adapter(#[from] ExpandedAdapterError),
    #[error("durable emission failed: {0}")]
    Emission(#[from] EmissionError),
    #[error("durable physical verification failed: {0}")]
    Verification(#[from] ExpandedPhysicalError),
    #[error("complete candidate certification failed: {0}")]
    Certification(#[from] CandidateCertificationError),
    #[error("typed identity width exceeded")]
    IdentityOverflow,
    #[error("seed topology is internally incomplete: {0}")]
    Incomplete(&'static str),
    #[error("physical input terminal is invalid: {0}")]
    PrimitiveTerminal(#[from] PrimitiveTerminalError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeedRoutingFailure {
    pub scheduled_index: usize,
    pub route: RouteId,
    pub source: PhysicalEndpointId,
    pub sink: RoutedSinkId,
    pub sink_endpoint: PhysicalEndpointId,
    pub fanout: usize,
    pub category: RouterRefusalCategory,
    pub limit_kind: Option<RouterLimitKind>,
    pub limit: Option<u64>,
    pub work_used: Option<u64>,
    pub plan_fingerprint: Fingerprint,
    pub source_at: Anchor,
    pub source_exit: Facing,
    pub precedence_blocker: Option<PhysicalEndpointId>,
    pub source_escape_obstructed: bool,
    pub sink_at: Anchor,
    pub sink_entry: Option<Facing>,
}

impl std::fmt::Display for SeedRoutingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "scheduled route {} ({:?}) from {:?} at {:?} exiting {:?} failed at {:?} at {:?} entering {:?} as {:?}",
            self.scheduled_index,
            self.route,
            self.source,
            self.source_at,
            self.source_exit,
            self.sink,
            self.sink_at,
            self.sink_entry,
            self.category
        )
    }
}

impl std::error::Error for SeedRoutingFailure {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SeedRepairRefusal {
    Routing(SeedRoutingFailure),
    CrossRouteConnectivity {
        first: RouteId,
        second: RouteId,
        first_source: PhysicalEndpointId,
        second_source: PhysicalEndpointId,
        at: Anchor,
        guarded_source: PhysicalEndpointId,
    },
    CrossRouteCoupling {
        source_route: RouteId,
        foreign: RouteId,
        source_endpoint: PhysicalEndpointId,
        foreign_endpoint: PhysicalEndpointId,
        at: Anchor,
        guarded_source: PhysicalEndpointId,
    },
}

impl std::fmt::Display for SeedRepairRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Routing(failure) => failure.fmt(formatter),
            Self::CrossRouteConnectivity {
                first,
                second,
                at,
                guarded_source,
                ..
            } => write!(
                formatter,
                "routes {first:?} and {second:?} connect at {at:?}; guard {guarded_source:?}"
            ),
            Self::CrossRouteCoupling {
                source_route,
                foreign,
                at,
                guarded_source,
                ..
            } => write!(
                formatter,
                "route {source_route:?} couples to {foreign:?} at {at:?}; guard {guarded_source:?}"
            ),
        }
    }
}

impl std::error::Error for SeedRepairRefusal {}

pub(crate) struct SparseSeedBuilder;

pub(crate) fn compile_sparse_seed_with_services(
    input: SeedInput<'_>,
    services: SeedServices<'_>,
) -> Result<CertifiedCandidate, SeedError> {
    SparseSeedBuilder::build(input, services)
}

pub(crate) fn compile_sparse_seed_variant_with_services(
    input: SeedInput<'_>,
    services: SeedServices<'_>,
    variant: &SeedVariant,
) -> Result<CertifiedCandidate, SeedError> {
    SparseSeedBuilder::build_variant(input, services, variant)
}

#[derive(Debug, Clone, Copy)]
struct SourceGeometry {
    route_anchor: Anchor,
    allowed_exit: Facing,
}

#[derive(Debug, Clone, Copy)]
struct TargetGeometry {
    terminal: Anchor,
    allowed_entry: Facing,
    support: Anchor,
    requirement: TerminalRequirement,
}

#[derive(Debug, Clone, Copy, Default)]
struct PlanTranslation {
    dx: i32,
    dz: i32,
}

impl PlanTranslation {
    fn for_unpinned(plan: &SeedPlacementPlan, has_pins: bool) -> Self {
        if has_pins {
            return Self::default();
        }
        let anchors = plan
            .instances
            .values()
            .map(|pose| pose.preferred_origin)
            .chain(plan.automatic_inputs.values().copied())
            .chain(plan.automatic_outputs.values().copied())
            .chain(plan.owner_offsets.iter().filter_map(|(owner, offset)| {
                let instance = match owner {
                    LayoutOwner::Primitive(primitive) => primitive.instance,
                    LayoutOwner::Junction(instance) => *instance,
                    LayoutOwner::Boundary(_) | LayoutOwner::Instance(_) => return None,
                };
                plan.instances.get(&instance).map(|pose| Anchor {
                    x: pose.preferred_origin.x.saturating_add(offset.x),
                    y: pose.preferred_origin.y.saturating_add(offset.y),
                    z: pose.preferred_origin.z.saturating_add(offset.z),
                })
            }))
            .collect::<Vec<_>>();
        let min_x = anchors.iter().map(|anchor| anchor.x).min().unwrap_or(0);
        let min_z = anchors.iter().map(|anchor| anchor.z).min().unwrap_or(0);
        Self {
            dx: ORIGIN_WORLD_MARGIN.saturating_sub(min_x).max(0),
            dz: ORIGIN_WORLD_MARGIN.saturating_sub(min_z).max(0),
        }
    }

    fn apply(self, anchor: Anchor) -> Anchor {
        Anchor {
            x: anchor.x.saturating_add(self.dx),
            z: anchor.z.saturating_add(self.dz),
            ..anchor
        }
    }
}

#[derive(Debug)]
struct PlacementSearch {
    max_radius: u32,
    max_backtracks: u64,
    backtracks_used: u64,
}

impl PlacementSearch {
    fn new(config: &SearchConfig) -> Self {
        Self {
            max_radius: config.max_seed_shell_radius,
            max_backtracks: config.max_seed_backtracks,
            backtracks_used: 0,
        }
    }

    fn reject_choice(
        &mut self,
        instance: InstanceId,
        primitive: PrimitiveId,
    ) -> Result<(), SeedError> {
        self.backtracks_used = self.backtracks_used.saturating_add(1);
        if self.backtracks_used >= self.max_backtracks {
            return Err(SeedError::PlacementExhausted {
                instance,
                primitive,
                radius: self.max_radius,
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SeedRepairBudget {
    layout_attempts_used: u64,
    layout_attempt_limit: u64,
    route_precedence_repairs_used: u64,
    route_precedence_repair_limit: u64,
}

impl SeedRepairBudget {
    fn new(layout_attempt_limit: u64, route_precedence_repair_limit: u64) -> Self {
        Self {
            layout_attempts_used: 1,
            layout_attempt_limit,
            route_precedence_repairs_used: 0,
            route_precedence_repair_limit,
        }
    }

    fn try_charge(&mut self, repair: &LayoutRepair) -> bool {
        if matches!(repair, LayoutRepair::RouteBefore { .. }) {
            if self.route_precedence_repairs_used >= self.route_precedence_repair_limit {
                return false;
            }
            self.route_precedence_repairs_used =
                self.route_precedence_repairs_used.saturating_add(1);
            true
        } else {
            if self.layout_attempts_used >= self.layout_attempt_limit {
                return false;
            }
            self.layout_attempts_used = self.layout_attempts_used.saturating_add(1);
            true
        }
    }
}

#[derive(Debug, Clone)]
enum PendingTarget {
    Connection(ConnectionId, TargetGeometry),
    DeclaredOutput(PortId, TargetGeometry),
}

impl PendingTarget {
    fn key(&self) -> (u8, u32, u16) {
        match self {
            Self::Connection(
                ConnectionId::External {
                    instance,
                    input_index,
                },
                _,
            ) => (0, instance.0, *input_index),
            Self::Connection(
                ConnectionId::Internal {
                    instance,
                    edge_index,
                },
                _,
            ) => (1, instance.0, *edge_index),
            Self::DeclaredOutput(port, _) => (2, port.0, 0),
        }
    }

    fn geometry(&self) -> TargetGeometry {
        match self {
            Self::Connection(_, geometry) | Self::DeclaredOutput(_, geometry) => *geometry,
        }
    }

    fn endpoint(&self) -> PhysicalEndpointId {
        match self {
            Self::Connection(connection, _) => PhysicalEndpointId::Landing(*connection),
            Self::DeclaredOutput(port, _) => PhysicalEndpointId::DeclaredOutput(*port),
        }
    }
}

impl SparseSeedBuilder {
    pub(crate) fn build(
        input: SeedInput<'_>,
        services: SeedServices<'_>,
    ) -> Result<CertifiedCandidate, SeedError> {
        Self::build_variant(input, services, &SeedVariant::default())
    }

    fn build_variant(
        input: SeedInput<'_>,
        services: SeedServices<'_>,
        variant: &SeedVariant,
    ) -> Result<CertifiedCandidate, SeedError> {
        if let Some(provenance) = input.source_provenance {
            if provenance.len() != input.lowered.gates.len() {
                return Err(SeedError::ProvenanceWidth {
                    expected: input.lowered.gates.len(),
                    actual: provenance.len(),
                });
            }
        }
        let instances = InstanceGraph::with_variants(
            input.lowered,
            services.library,
            &variant.implementations,
            &variant.duplicates,
        )?;
        if let Some(first) = instances
            .instances
            .iter()
            .flat_map(|instance| {
                instance
                    .expanded
                    .topology
                    .primitives
                    .iter()
                    .map(move |primitive| (instance.id, primitive.id))
            })
            .next()
        {
            if services.search_config.max_seed_shell_radius == 0
                || services.search_config.max_seed_backtracks == 0
            {
                return Err(SeedError::PlacementExhausted {
                    instance: first.0,
                    primitive: first.1,
                    radius: services.search_config.max_seed_shell_radius,
                });
            }
        }

        let mut repairs = BTreeSet::new();
        let mut attempts_used = 0u64;
        let attempt_limit = services
            .search_config
            .max_seed_backtracks
            .min(MAX_LAYOUT_REPAIR_ATTEMPTS);
        let route_precedence_repair_limit = u64::try_from(instances.instances.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(2)
            .clamp(MIN_ROUTE_PRECEDENCE_REPAIRS, MAX_ROUTE_PRECEDENCE_REPAIRS);
        let mut repair_budget = SeedRepairBudget::new(attempt_limit, route_precedence_repair_limit);
        loop {
            attempts_used = attempts_used.saturating_add(1);
            match Self::build_attempt(
                input,
                services,
                variant,
                instances.clone(),
                &repairs.iter().copied().collect::<Vec<_>>(),
            ) {
                Ok(certified) => return Ok(certified),
                Err(SeedError::Repairable(refusal)) => {
                    let repair = next_layout_repair(&refusal, &repairs)?;
                    if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                        eprintln!(
                            "seed attempt {attempts_used}: refusal={refusal:?}; repair={repair:?}"
                        );
                    }
                    if repairs.contains(&repair) || !repair_budget.try_charge(&repair) {
                        return Err(SeedError::SeedExhausted {
                            attempts_used,
                            final_refusal: refusal,
                        });
                    }
                    repairs.insert(repair);
                }
                Err(error) => {
                    if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                        eprintln!(
                            "seed attempt {attempts_used}: terminal={error:?}; repairs={repairs:?}"
                        );
                    }
                    return Err(error);
                }
            }
        }
    }

    fn build_attempt(
        input: SeedInput<'_>,
        services: SeedServices<'_>,
        variant: &SeedVariant,
        instances: InstanceGraph,
        repairs: &[LayoutRepair],
    ) -> Result<CertifiedCandidate, SeedError> {
        let mut candidate =
            ExpandedPhysicalCandidate::empty(instances, input.pins.cloned().unwrap_or_default());
        candidate.bind_pin_contracts(input.lowered)?;
        crate::compile::planner::validate_port_placements(input.lowered, &candidate.pins)
            .map_err(SeedError::InvalidPins)?;
        let placement_analysis = analyse_instance_dag(&candidate.instances)
            .map_err(|_| SeedError::Incomplete("seed placement analysis"))?;
        let placement_plan = services.placer.plan_with_repairs(
            SeedPlacementRequest {
                graph: &candidate.instances,
                analysis: &placement_analysis,
                pins: &candidate.pin_contracts,
            },
            repairs,
        )?;
        let plan_translation =
            PlanTranslation::for_unpinned(&placement_plan, !candidate.pin_contracts.is_empty());

        let mut occupied = BTreeSet::new();
        let mut sources = BTreeMap::new();
        let mut targets = BTreeMap::new();
        place_boundaries(
            &mut candidate,
            input.lowered,
            &placement_plan,
            plan_translation,
            &mut occupied,
            &mut sources,
            &mut targets,
        )?;
        place_instances(
            &mut candidate,
            input.lowered,
            services.search_config,
            &placement_plan,
            plan_translation,
            &variant.placements,
            &mut occupied,
            &mut sources,
            &mut targets,
        )?;

        let mut reservations = reservations_for_components(&candidate)?;
        reserve_route_endpoints(&mut reservations, &candidate, &sources, &targets)?;
        if let Err(error) = route_all(
            &mut candidate,
            services.router,
            services.search_config,
            &placement_analysis,
            &placement_plan,
            plan_translation,
            &sources,
            &targets,
            repairs,
            &mut reservations,
        ) {
            return match error {
                SeedError::Routing(failure) => {
                    Err(SeedError::Repairable(SeedRepairRefusal::Routing(failure)))
                }
                other => Err(other),
            };
        }
        if let Some(failure) =
            route_ownership_failure(&candidate, &sources, &placement_plan.fingerprint)?
        {
            return Err(SeedError::Repairable(SeedRepairRefusal::Routing(failure)));
        }
        candidate.validate_shape()?;
        candidate.validate_physical_ownership()?;

        let adapter = ExpandedCandidateAdapter::new(&candidate)?;
        let size = adapter.deterministic_world_size()?;
        let emitted = services.emitter.emit(&adapter, size)?;
        match services.verifier.verify(&candidate, &emitted) {
            Ok(()) => {}
            Err(ExpandedPhysicalError::CrossRouteConnectivity { first, second, at }) => {
                if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                    eprintln!(
                        "cross-route connectivity: first={first:?}/{:?} second={second:?}/{:?} at={at:?} owner={:?}",
                        candidate.routes.get(&first).map(|tree| tree.source),
                        candidate.routes.get(&second).map(|tree| tree.source),
                        emitted.owner_at(at),
                    );
                }
                return Err(SeedError::Repairable(cross_route_connectivity_refusal(
                    &candidate, first, second, at, repairs,
                )?));
            }
            Err(ExpandedPhysicalError::CrossRouteCoupling {
                source_route,
                foreign,
                at,
            }) => {
                let (source_endpoint, foreign_endpoint, guarded_source) = cross_route_sources(
                    &candidate,
                    source_route,
                    foreign,
                    &candidate.pin_contracts,
                    repairs,
                )?;
                return Err(SeedError::Repairable(
                    SeedRepairRefusal::CrossRouteCoupling {
                        source_route,
                        foreign,
                        source_endpoint,
                        foreign_endpoint,
                        at,
                        guarded_source,
                    },
                ));
            }
            Err(error) => return Err(SeedError::Verification(error)),
        }

        let certification = CertificationConfig::from_search(services.search_config);
        services
            .certifier
            .certify(candidate, input.lowered, services.library, &certification)
            .map_err(SeedError::from)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeedPhysicalOwner {
    Primitive(PrimitiveId),
    Boundary(PhysicalEndpointId),
    Route(RouteId),
    Junction(InstanceId),
}

fn claim_seed_owner(
    ledger: &mut BTreeMap<Anchor, SeedPhysicalOwner>,
    at: Anchor,
    owner: SeedPhysicalOwner,
) -> Option<(SeedPhysicalOwner, SeedPhysicalOwner)> {
    match ledger.insert(at, owner) {
        Some(first) if first != owner => Some((first, owner)),
        _ => None,
    }
}

fn route_ownership_failure(
    candidate: &ExpandedPhysicalCandidate,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    plan_fingerprint: &Fingerprint,
) -> Result<Option<SeedRoutingFailure>, SeedError> {
    let mut ledger = BTreeMap::<Anchor, SeedPhysicalOwner>::new();
    let mut conflict = None;
    let mut claim = |at: Anchor, owner: SeedPhysicalOwner| {
        if let Some((first, second)) = claim_seed_owner(&mut ledger, at, owner) {
            let route = match (first, owner) {
                (SeedPhysicalOwner::Route(route), _) | (_, SeedPhysicalOwner::Route(route)) => {
                    Some(route)
                }
                _ => None,
            };
            if conflict.is_none() {
                if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                    eprintln!(
                        "physical ownership conflict: at={at:?} first={first:?} second={second:?}"
                    );
                }
                conflict = route.map(|route| (route, at));
            }
        }
    };
    for (&id, placement) in &candidate.placements {
        for block in &placement.blocks {
            claim(block.at, SeedPhysicalOwner::Primitive(id));
        }
    }
    for (&endpoint, boundary) in &candidate.boundaries {
        for block in &boundary.blocks {
            claim(block.at, SeedPhysicalOwner::Boundary(endpoint));
        }
    }
    for (&route, tree) in &candidate.routes {
        for block in tree.cells.iter().chain(&tree.floors) {
            claim(block.at, SeedPhysicalOwner::Route(route));
        }
    }
    for (&instance, junction) in &candidate.junctions {
        for block in &junction.cells {
            claim(block.at, SeedPhysicalOwner::Junction(instance));
        }
    }
    let Some((route, at)) = conflict else {
        return Ok(None);
    };
    let tree = candidate
        .routes
        .get(&route)
        .ok_or(SeedError::Incomplete("ownership-conflict route"))?;
    let above = Anchor { y: at.y + 1, ..at };
    let branch = tree
        .branches
        .iter()
        .find(|branch| branch.path.contains(&at) || branch.path.contains(&above))
        .or_else(|| tree.branches.first())
        .ok_or(SeedError::EmptyRoute)?;
    let sink_endpoint = match branch.target {
        crate::compile::routing::RouteTarget::Connection(connection) => {
            PhysicalEndpointId::Landing(connection)
        }
        crate::compile::routing::RouteTarget::DeclaredOutput(port) => {
            PhysicalEndpointId::DeclaredOutput(port)
        }
    };
    let source_at = sources
        .get(&tree.source)
        .map(|geometry| geometry.route_anchor)
        .ok_or(SeedError::Incomplete("ownership-conflict source geometry"))?;
    let source_exit = sources
        .get(&tree.source)
        .map(|geometry| geometry.allowed_exit)
        .ok_or(SeedError::Incomplete("ownership-conflict source geometry"))?;
    let sink_entry = branch
        .path
        .iter()
        .rev()
        .copied()
        .find(|at| *at != branch.terminal.at)
        .and_then(|approach| horizontal_direction_between(branch.terminal.at, approach));
    Ok(Some(SeedRoutingFailure {
        scheduled_index: usize::try_from(route.0).map_err(|_| SeedError::IdentityOverflow)?,
        route,
        source: tree.source,
        sink: branch.sink,
        sink_endpoint,
        fanout: tree.branches.len(),
        category: RouterRefusalCategory::PhysicalInvariant,
        limit_kind: None,
        limit: None,
        work_used: None,
        plan_fingerprint: plan_fingerprint.clone(),
        source_at,
        source_exit,
        precedence_blocker: None,
        source_escape_obstructed: false,
        sink_at: branch.terminal.at,
        sink_entry,
    }))
}

fn horizontal_direction_between(from: Anchor, to: Anchor) -> Option<Facing> {
    match (to.x - from.x, to.y - from.y, to.z - from.z) {
        (0, 0, -1) => Some(Facing::North),
        (0, 0, 1) => Some(Facing::South),
        (1, 0, 0) => Some(Facing::East),
        (-1, 0, 0) => Some(Facing::West),
        _ => None,
    }
}

fn next_layout_repair(
    refusal: &SeedRepairRefusal,
    repairs: &BTreeSet<LayoutRepair>,
) -> Result<LayoutRepair, SeedError> {
    let repair = match refusal {
        SeedRepairRefusal::CrossRouteConnectivity {
            first_source,
            second_source,
            guarded_source,
            ..
        } => {
            let guard = LayoutRepair::ExclusiveGuardedTrack {
                source: *guarded_source,
            };
            if repairs.contains(&guard) {
                next_owner_separation(
                    repairs,
                    layout_owner_for_endpoint(*first_source),
                    layout_owner_for_endpoint(*second_source),
                    SeparationAxis::Lateral,
                )
            } else {
                guard
            }
        }
        SeedRepairRefusal::CrossRouteCoupling {
            source_endpoint,
            foreign_endpoint,
            guarded_source,
            ..
        } => {
            let guard = LayoutRepair::ExclusiveGuardedTrack {
                source: *guarded_source,
            };
            if repairs.contains(&guard) {
                next_owner_separation(
                    repairs,
                    layout_owner_for_endpoint(*source_endpoint),
                    layout_owner_for_endpoint(*foreign_endpoint),
                    SeparationAxis::Lateral,
                )
            } else {
                guard
            }
        }
        SeedRepairRefusal::Routing(failure) => {
            let preparation = LayoutRepair::ExclusiveGuardedTrack {
                source: failure.source,
            };
            let promotion = LayoutRepair::EarlyTreeSinkAndEscape {
                source: failure.source,
                sink: failure.sink_endpoint,
            };
            if let Some(blocker) = failure.precedence_blocker {
                let precedence = LayoutRepair::RouteBefore {
                    source: failure.source,
                    blocker,
                };
                if !repairs.contains(&precedence)
                    && !route_precedence_would_cycle(repairs, failure.source, blocker)
                {
                    return Ok(precedence);
                }
            }
            if failure.fanout > 1 && !repairs.contains(&preparation) {
                preparation
            } else if failure.fanout > 1 && !repairs.contains(&promotion) {
                promotion
            } else {
                let source_owner = layout_owner_for_endpoint(failure.source);
                let sink_owner = layout_owner_for_endpoint(failure.sink_endpoint);
                let axis = if failure.category == RouterRefusalCategory::NoLocalRoute
                    && failure.precedence_blocker.is_none()
                {
                    runway_direction(failure.source_exit)
                        .map(SeparationAxis::Runway)
                        .unwrap_or(SeparationAxis::Lateral)
                } else {
                    SeparationAxis::Lateral
                };
                next_owner_separation(repairs, source_owner, sink_owner, axis)
            }
        }
    };
    Ok(repair)
}

fn runway_direction(facing: Facing) -> Option<RunwayDirection> {
    match facing {
        Facing::North => Some(RunwayDirection::North),
        Facing::South => Some(RunwayDirection::South),
        Facing::East => Some(RunwayDirection::East),
        Facing::West => Some(RunwayDirection::West),
        Facing::Up | Facing::Down => None,
    }
}

fn route_precedence_would_cycle(
    repairs: &BTreeSet<LayoutRepair>,
    source: PhysicalEndpointId,
    blocker: PhysicalEndpointId,
) -> bool {
    let mut pending = vec![blocker];
    let mut visited = BTreeSet::new();
    while let Some(endpoint) = pending.pop() {
        if endpoint == source {
            return true;
        }
        if !visited.insert(endpoint) {
            continue;
        }
        pending.extend(repairs.iter().filter_map(|repair| match repair {
            LayoutRepair::RouteBefore {
                source: edge_source,
                blocker: edge_blocker,
            } if *edge_source == endpoint => Some(*edge_blocker),
            _ => None,
        }));
    }
    false
}

fn next_owner_separation(
    repairs: &BTreeSet<LayoutRepair>,
    source_owner: LayoutOwner,
    sink_owner: LayoutOwner,
    axis: SeparationAxis,
) -> LayoutRepair {
    let ordinal = repairs
        .iter()
        .filter_map(|repair| match repair {
            LayoutRepair::SeparateOwners {
                source_owner: candidate_source,
                sink_owner: candidate_sink,
                axis: candidate_axis,
                ordinal,
            } if *candidate_source == source_owner
                && *candidate_sink == sink_owner
                && *candidate_axis == axis =>
            {
                Some(*ordinal)
            }
            _ => None,
        })
        .max()
        .map_or(0, |ordinal| ordinal.saturating_add(1));
    LayoutRepair::SeparateOwners {
        source_owner,
        sink_owner,
        axis,
        ordinal,
    }
}

fn layout_owner_for_endpoint(endpoint: PhysicalEndpointId) -> LayoutOwner {
    match endpoint {
        PhysicalEndpointId::PrimaryInput(_) | PhysicalEndpointId::DeclaredOutput(_) => {
            LayoutOwner::Boundary(endpoint)
        }
        PhysicalEndpointId::PrimitiveOutput(primitive) => LayoutOwner::Instance(primitive.instance),
        PhysicalEndpointId::Junction(instance) => LayoutOwner::Instance(instance),
        PhysicalEndpointId::Landing(connection) => LayoutOwner::Instance(match connection {
            ConnectionId::External { instance, .. } | ConnectionId::Internal { instance, .. } => {
                instance
            }
        }),
    }
}

fn cross_route_connectivity_refusal(
    candidate: &ExpandedPhysicalCandidate,
    first: RouteId,
    second: RouteId,
    at: Anchor,
    repairs: &[LayoutRepair],
) -> Result<SeedRepairRefusal, SeedError> {
    let (first_source, second_source, guarded_source) =
        cross_route_sources(candidate, first, second, &candidate.pin_contracts, repairs)?;
    Ok(SeedRepairRefusal::CrossRouteConnectivity {
        first,
        second,
        first_source,
        second_source,
        at,
        guarded_source,
    })
}

fn cross_route_sources(
    candidate: &ExpandedPhysicalCandidate,
    first: RouteId,
    second: RouteId,
    pins: &BTreeMap<PhysicalEndpointId, crate::compile::planner::PortPin>,
    repairs: &[LayoutRepair],
) -> Result<(PhysicalEndpointId, PhysicalEndpointId, PhysicalEndpointId), SeedError> {
    let first_source = candidate
        .routes
        .get(&first)
        .map(|tree| tree.source)
        .ok_or(SeedError::Incomplete("cross-route first source"))?;
    let second_source = candidate
        .routes
        .get(&second)
        .map(|tree| tree.source)
        .ok_or(SeedError::Incomplete("cross-route second source"))?;
    let mut routes = [first, second];
    routes.sort();
    for route in routes.into_iter().rev() {
        let source = candidate
            .routes
            .get(&route)
            .map(|tree| tree.source)
            .ok_or(SeedError::Incomplete("cross-route source"))?;
        if !pins.contains_key(&source)
            && !repairs.contains(&LayoutRepair::ExclusiveGuardedTrack { source })
        {
            return Ok((first_source, second_source, source));
        }
    }
    for route in routes.into_iter().rev() {
        let source = candidate
            .routes
            .get(&route)
            .map(|tree| tree.source)
            .ok_or(SeedError::Incomplete("cross-route source"))?;
        if !pins.contains_key(&source) {
            return Ok((first_source, second_source, source));
        }
    }
    Err(SeedError::Incomplete("movable cross-route source"))
}

fn reserve_route_endpoints(
    reservations: &mut PhysicalReservations,
    candidate: &ExpandedPhysicalCandidate,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    for (&endpoint, source) in sources {
        if reservations.get(&source.route_anchor).is_none() {
            reservations.reserve(
                source.route_anchor,
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    for (&sink, target) in targets {
        if reservations.get(&target.terminal).is_none() {
            let endpoint = match sink {
                PhysicalSink::InstanceInput {
                    instance,
                    input_index,
                } => PhysicalEndpointId::Landing(ConnectionId::External {
                    instance,
                    input_index,
                }),
                PhysicalSink::DeclaredOutput(port) => PhysicalEndpointId::DeclaredOutput(port),
            };
            reservations.reserve(
                target.terminal,
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    let endpoints = sources
        .values()
        .map(|source| source.route_anchor)
        .chain(targets.values().map(|target| target.terminal))
        .collect::<BTreeSet<_>>();
    for &endpoint in candidate.boundaries.keys() {
        match endpoint {
            PhysicalEndpointId::PrimaryInput(_) => {
                let source = sources
                    .get(&endpoint)
                    .ok_or(SeedError::Incomplete("boundary source geometry"))?;
                let body = step(source.route_anchor, source.allowed_exit.opposite());
                reserve_boundary_terminal_sides(
                    reservations,
                    endpoint,
                    body,
                    source.route_anchor,
                    &endpoints,
                );
            }
            PhysicalEndpointId::DeclaredOutput(port) => {
                let target = targets
                    .get(&PhysicalSink::DeclaredOutput(port))
                    .ok_or(SeedError::Incomplete("boundary target geometry"))?;
                reserve_boundary_terminal_sides(
                    reservations,
                    endpoint,
                    target.support,
                    target.terminal,
                    &endpoints,
                );
            }
            _ => return Err(SeedError::Incomplete("boundary endpoint identity")),
        }
    }
    for (&primitive, placement) in &candidate.placements {
        let Some(specification) = candidate
            .instances
            .instances
            .iter()
            .flat_map(|instance| &instance.expanded.topology.primitives)
            .find(|specification| specification.id == primitive)
        else {
            continue;
        };
        if specification.primitive != Primitive::Torch {
            continue;
        }
        let variant = &physical::variants(specification.primitive)[usize::from(placement.variant)];
        let support = translate(
            placement.anchor,
            variant.port(PortKind::TorchInput).position,
        );
        for direction in geometry::input_directions(placement.facing) {
            let socket = step(support, direction);
            if !endpoints.contains(&socket) && reservations.get(&socket).is_none() {
                reservations.reserve(
                    socket,
                    PhysicalReservationOwner::KeepOut(
                        primitive.instance.0 ^ u32::from(primitive.node.0),
                    ),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }
    for junction in candidate.junctions.values() {
        for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let neighbour = step(junction.at, direction);
            if !endpoints.contains(&neighbour) && reservations.get(&neighbour).is_none() {
                reservations.reserve(
                    neighbour,
                    PhysicalReservationOwner::KeepOut(junction.id.0),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }
    Ok(())
}

fn reserve_boundary_terminal_sides(
    reservations: &mut PhysicalReservations,
    endpoint: PhysicalEndpointId,
    body: Anchor,
    handover: Anchor,
    protected: &BTreeSet<Anchor>,
) {
    for direction in [
        Facing::North,
        Facing::South,
        Facing::East,
        Facing::West,
        Facing::Up,
        Facing::Down,
    ] {
        let side = step(body, direction);
        if side == handover || protected.contains(&side) || reservations.get(&side).is_some() {
            continue;
        }
        reservations.reserve(
            side,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );
    }
}

fn place_boundaries(
    candidate: &mut ExpandedPhysicalCandidate,
    netlist: &Netlist,
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    for (index, name) in netlist.inputs.iter().enumerate() {
        let port = PortId(u32::try_from(index).map_err(|_| SeedError::IdentityOverflow)?);
        let endpoint = PhysicalEndpointId::PrimaryInput(port);
        let pin = candidate.pin_contracts.get(&endpoint).copied();
        let (blocks, delayed, observation_at, route_anchor, allowed_exit) = match pin {
            Some(pin) => {
                let handover = pin.handover(PortRole::Input);
                let net = pin.net_cell(PortRole::Input);
                let blocks = vec![
                    PlacedBlock {
                        at: Anchor {
                            y: handover.y - 1,
                            ..handover
                        },
                        state: compile::stone(),
                    },
                    PlacedBlock {
                        at: handover,
                        state: compile::repeater(pin.toward),
                    },
                ];
                (
                    blocks,
                    Some(DelayedComponent {
                        at: handover,
                        owner: DelayedOwner::InputBinding(port),
                    }),
                    pin.at,
                    net,
                    pin.toward,
                )
            }
            None => {
                let home = plan
                    .automatic_inputs
                    .get(&port)
                    .copied()
                    .ok_or(SeedError::Incomplete("automatic input placement"))?;
                let toward = automatic_boundary_direction(candidate);
                let home = plan_translation.apply(home);
                let root = step(home, toward);
                (
                    vec![
                        PlacedBlock {
                            at: Anchor {
                                y: home.y - 1,
                                ..home
                            },
                            state: compile::stone(),
                        },
                        PlacedBlock {
                            at: home,
                            state: compile::lever(false),
                        },
                    ],
                    None,
                    home,
                    root,
                    toward,
                )
            }
        };
        claim_blocks(occupied, &blocks)?;
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed,
                blocks: blocks.clone(),
            },
        );
        candidate.observations.insert(
            ObservationId::PrimaryInput(port),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::PrimaryInput(port),
                    at: observation_at,
                    logical_owner: None,
                    display_label: Some(name.clone()),
                },
                state: block_state_at(&blocks, observation_at),
            },
        );
        sources.insert(
            endpoint,
            SourceGeometry {
                route_anchor,
                allowed_exit,
            },
        );
    }

    for (index, name) in netlist.outputs.iter().enumerate() {
        let port = PortId(u32::try_from(index).map_err(|_| SeedError::IdentityOverflow)?);
        let endpoint = PhysicalEndpointId::DeclaredOutput(port);
        let pin = candidate.pin_contracts.get(&endpoint).copied();
        let (blocks, observation_at, geometry) = match pin {
            Some(pin) => {
                let terminal = pin.handover(PortRole::Output);
                (
                    Vec::new(),
                    pin.at,
                    TargetGeometry {
                        terminal,
                        allowed_entry: pin.toward.opposite(),
                        support: pin.at,
                        requirement: TerminalRequirement::Exact(
                            crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                        ),
                    },
                )
            }
            None => {
                let lamp = plan
                    .automatic_outputs
                    .get(&port)
                    .copied()
                    .ok_or(SeedError::Incomplete("automatic output placement"))?;
                let toward = automatic_boundary_direction(candidate);
                let lamp = plan_translation.apply(lamp);
                let terminal = step(lamp, toward.opposite());
                let blocks = vec![PlacedBlock {
                    at: lamp,
                    state: compile::lamp(),
                }];
                (
                    blocks,
                    lamp,
                    TargetGeometry {
                        terminal,
                        allowed_entry: toward.opposite(),
                        support: lamp,
                        requirement: TerminalRequirement::Exact(
                            crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                        ),
                    },
                )
            }
        };
        claim_blocks(occupied, &blocks)?;
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: None,
                blocks: blocks.clone(),
            },
        );
        candidate.observations.insert(
            ObservationId::DeclaredOutput(port),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::DeclaredOutput(port),
                    at: observation_at,
                    logical_owner: candidate
                        .instances
                        .assignments
                        .iter()
                        .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                        .and_then(|assignment| match &assignment.driver {
                            PhysicalDriver::PrimaryInput(_) => None,
                            PhysicalDriver::Instance(driver) => Some(match driver {
                                crate::compile::fragment_synth::instance_graph::InstanceDriver::Primitive { logical_owner, .. }
                                | crate::compile::fragment_synth::instance_graph::InstanceDriver::Junction { logical_owner, .. } => *logical_owner,
                            }),
                        }),
                    display_label: Some(name.clone()),
                },
                state: block_state_at(&blocks, observation_at),
            },
        );
        targets.insert(PhysicalSink::DeclaredOutput(port), geometry);
    }
    Ok(())
}

fn automatic_boundary_direction(candidate: &ExpandedPhysicalCandidate) -> Facing {
    let inputs = candidate
        .pin_contracts
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)).then_some(*pin)
        })
        .collect::<Vec<_>>();
    let outputs = candidate
        .pin_contracts
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)).then_some(*pin)
        })
        .collect::<Vec<_>>();
    if !inputs.is_empty() && !outputs.is_empty() {
        let from = median_anchor(inputs.iter().map(|pin| pin.net_cell(PortRole::Input)));
        let to = median_anchor(outputs.iter().map(|pin| pin.net_cell(PortRole::Output)));
        dominant_horizontal_direction(from, to)
    } else if !inputs.is_empty() {
        majority_direction(inputs.iter().map(|pin| pin.toward))
    } else if !outputs.is_empty() {
        majority_direction(outputs.iter().map(|pin| pin.toward)).opposite()
    } else {
        Facing::East
    }
}

fn median_anchor(anchors: impl Iterator<Item = Anchor>) -> Anchor {
    let anchors = anchors.collect::<Vec<_>>();
    let mut xs = anchors.iter().map(|anchor| anchor.x).collect::<Vec<_>>();
    let mut ys = anchors.iter().map(|anchor| anchor.y).collect::<Vec<_>>();
    let mut zs = anchors.iter().map(|anchor| anchor.z).collect::<Vec<_>>();
    xs.sort();
    ys.sort();
    zs.sort();
    Anchor {
        x: xs[xs.len() / 2],
        y: ys[ys.len() / 2],
        z: zs[zs.len() / 2],
    }
}

fn majority_direction(directions: impl Iterator<Item = Facing>) -> Facing {
    let order = [Facing::North, Facing::East, Facing::South, Facing::West];
    let directions = directions.collect::<Vec<_>>();
    order
        .into_iter()
        .max_by_key(|candidate| {
            (
                directions
                    .iter()
                    .filter(|direction| *direction == candidate)
                    .count(),
                std::cmp::Reverse(
                    order
                        .iter()
                        .position(|direction| direction == candidate)
                        .unwrap(),
                ),
            )
        })
        .unwrap_or(Facing::East)
}

fn dominant_horizontal_direction(from: Anchor, to: Anchor) -> Facing {
    let dx = i64::from(to.x) - i64::from(from.x);
    let dz = i64::from(to.z) - i64::from(from.z);
    if dz.abs() >= dx.abs() && dz != 0 {
        if dz < 0 {
            Facing::North
        } else {
            Facing::South
        }
    } else if dx < 0 {
        Facing::West
    } else {
        Facing::East
    }
}

fn place_instances(
    candidate: &mut ExpandedPhysicalCandidate,
    netlist: &Netlist,
    search_config: &SearchConfig,
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
    placement_overrides: &BTreeMap<InstanceId, InstancePlacementOverride>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let order = topological_instance_order(&candidate.instances);
    let mut placement_search = PlacementSearch::new(search_config);

    for instance_id in order {
        let instance = candidate
            .instances
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .ok_or(SeedError::Incomplete("topological instance"))?
            .clone();
        let planned = plan
            .instances
            .get(&instance.id)
            .copied()
            .ok_or(SeedError::Incomplete("planned instance pose"))?;
        let placement_override = placement_overrides.get(&instance.id).copied();
        let base = apply_plan_owner_offset(
            plan_translation.apply(Anchor {
                x: planned
                    .preferred_origin
                    .x
                    .saturating_add(placement_override.map_or(0, |choice| choice.dx)),
                z: planned
                    .preferred_origin
                    .z
                    .saturating_add(placement_override.map_or(0, |choice| choice.dz)),
                ..planned.preferred_origin
            }),
            plan,
            LayoutOwner::Instance(instance.id),
        );
        let gate = &netlist.gates
            [usize::try_from(instance.logical_gate.0).map_err(|_| SeedError::IdentityOverflow)?];

        match &instance.expanded.topology.output {
            OutputSpec::Junction { contributors, .. } => {
                let facing = placement_override
                    .map(|choice| choice.facing)
                    .unwrap_or(planned.facing);
                place_junction_instance(
                    candidate,
                    &instance,
                    gate,
                    contributors,
                    apply_plan_owner_offset(base, plan, LayoutOwner::Junction(instance.id)),
                    facing,
                    occupied,
                    sources,
                    targets,
                )?;
            }
            OutputSpec::Primitive(output) => {
                let facing = placement_override
                    .map(|choice| choice.facing)
                    .unwrap_or(planned.facing);
                let positions = topology_primitive_positions(&instance);
                for specification in &instance.expanded.topology.primitives {
                    let local = positions[&specification.id];
                    let (dx, dy, dz) = geometry::rotate((local.x, local.y, local.z), facing);
                    let anchor = apply_plan_owner_offset(
                        Anchor {
                            x: base.x.saturating_add(dx),
                            y: base.y.saturating_add(dy),
                            z: base.z.saturating_add(dz),
                        },
                        plan,
                        LayoutOwner::Primitive(specification.id),
                    );
                    place_primitive_searched(
                        candidate,
                        specification.id,
                        specification.primitive,
                        facing,
                        anchor,
                        Some(instance.id),
                        instance.id,
                        &mut placement_search,
                        occupied,
                        sources,
                    )?;
                }
                assign_primitive_targets(candidate, &instance, targets)?;
                let observation = candidate
                    .observations
                    .get(&ObservationId::PrimitiveOutput(*output))
                    .cloned()
                    .ok_or(SeedError::Incomplete("instance output primitive"))?;
                candidate.observations.insert(
                    ObservationId::InstanceOutput(instance.id),
                    VerifiedObservation {
                        site: ObservationSite {
                            id: ObservationId::InstanceOutput(instance.id),
                            at: observation.site.at,
                            logical_owner: Some(instance.id),
                            display_label: Some(gate.output.clone()),
                        },
                        state: observation.state,
                    },
                );
            }
        }
    }
    Ok(())
}

fn apply_plan_owner_offset(anchor: Anchor, plan: &SeedPlacementPlan, owner: LayoutOwner) -> Anchor {
    let offset = plan
        .owner_offsets
        .get(&owner)
        .copied()
        .unwrap_or(Anchor { x: 0, y: 0, z: 0 });
    Anchor {
        x: anchor.x.saturating_add(offset.x),
        y: anchor.y.saturating_add(offset.y),
        z: anchor.z.saturating_add(offset.z),
    }
}

fn topology_primitive_positions(
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
) -> BTreeMap<PrimitiveId, Position> {
    let topology = &instance.expanded.topology;
    let mut levels = BTreeMap::<PrimitiveId, i32>::new();
    while levels.len() < topology.primitives.len() {
        let before = levels.len();
        for primitive in &topology.primitives {
            if levels.contains_key(&primitive.id) {
                continue;
            }
            let incoming = topology
                .connections
                .iter()
                .filter(|edge| edge.target == ConnectionTarget::Primitive(primitive.id))
                .filter_map(|edge| match edge.source {
                    ConnectionSource::Primitive(source) => Some(source),
                    ConnectionSource::ExternalInput { .. } => None,
                })
                .collect::<Vec<_>>();
            if incoming.iter().all(|source| levels.contains_key(source)) {
                levels.insert(
                    primitive.id,
                    incoming
                        .iter()
                        .map(|source| levels[source] + 1)
                        .max()
                        .unwrap_or(0),
                );
            }
        }
        if levels.len() == before {
            break;
        }
    }
    let mut per_level = BTreeMap::<i32, i32>::new();
    topology
        .primitives
        .iter()
        .map(|primitive| {
            let level = levels.get(&primitive.id).copied().unwrap_or(0);
            let lane = per_level.entry(level).or_insert(0);
            let position = Position::new(level * 4, 0, *lane * 4);
            *lane += 1;
            (primitive.id, position)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn place_junction_instance(
    candidate: &mut ExpandedPhysicalCandidate,
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
    gate: &crate::compile::Gate,
    contributors: &[ContributorSpec],
    at: Anchor,
    facing: CellFacing,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let cells = vec![
        PlacedBlock {
            at: Anchor { y: at.y - 1, ..at },
            state: compile::stone(),
        },
        PlacedBlock {
            at,
            state: compile::dust(),
        },
    ];
    claim_blocks(occupied, &cells)?;
    candidate.junctions.insert(
        instance.id,
        RealisedJunction {
            id: instance.id,
            at,
            facing,
            contributors: contributors.iter().map(contributor_endpoint).collect(),
            cells: cells.clone(),
        },
    );
    let junction_state = block_state_at(&cells, at);
    for id in [
        ObservationId::JunctionOutput(instance.id),
        ObservationId::InstanceOutput(instance.id),
    ] {
        candidate.observations.insert(
            id,
            VerifiedObservation {
                site: ObservationSite {
                    id,
                    at,
                    logical_owner: Some(instance.id),
                    display_label: (id == ObservationId::InstanceOutput(instance.id))
                        .then(|| gate.output.clone()),
                },
                state: junction_state.clone(),
            },
        );
    }
    sources.insert(
        PhysicalEndpointId::Junction(instance.id),
        SourceGeometry {
            route_anchor: step(at, geometry::output_direction(facing)),
            allowed_exit: geometry::output_direction(facing),
        },
    );

    let directions = geometry::input_directions(facing);
    for (input_index, contributor) in contributors.iter().enumerate() {
        let direction = directions[input_index];
        match *contributor {
            ContributorSpec::Landing(connection) => {
                targets.insert(
                    PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index: connection_input_index(connection),
                    },
                    TargetGeometry {
                        terminal: step(at, direction),
                        allowed_entry: direction,
                        support: at,
                        requirement: TerminalRequirement::Exact(
                            crate::compile::routing::RouteTerminalKind::BareMergeDust,
                        ),
                    },
                );
            }
            ContributorSpec::Primitive(primitive) => {
                let primitive_at = step(at, direction);
                let facing = repeater_facing_with_front(direction.opposite())?;
                place_primitive(
                    candidate,
                    primitive,
                    Primitive::Repeater,
                    facing,
                    primitive_at,
                    Some(instance.id),
                    occupied,
                    sources,
                )?;
                let input_index = instance
                    .expanded
                    .topology
                    .connections
                    .iter()
                    .find(|connection| connection.target == ConnectionTarget::Primitive(primitive))
                    .and_then(|connection| match connection.id {
                        ConnectionId::External { input_index, .. } => Some(input_index),
                        ConnectionId::Internal { .. } => None,
                    })
                    .ok_or(SeedError::Incomplete("isolating repeater input"))?;
                let rear = primitive_input_geometry(candidate, primitive, 0)?;
                targets.insert(
                    PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index,
                    },
                    rear,
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn place_primitive(
    candidate: &mut ExpandedPhysicalCandidate,
    id: PrimitiveId,
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
    logical_owner: Option<InstanceId>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
) -> Result<(), SeedError> {
    let blocks = primitive_blocks(primitive, facing, anchor)?;
    commit_primitive(
        candidate,
        id,
        primitive,
        facing,
        anchor,
        logical_owner,
        occupied,
        sources,
        blocks,
    )
}

#[allow(clippy::too_many_arguments)]
fn place_primitive_searched(
    candidate: &mut ExpandedPhysicalCandidate,
    id: PrimitiveId,
    primitive: Primitive,
    facing: CellFacing,
    preferred: Anchor,
    logical_owner: Option<InstanceId>,
    instance: InstanceId,
    search: &mut PlacementSearch,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
) -> Result<(), SeedError> {
    let (anchor, blocks) =
        find_primitive_placement(primitive, facing, preferred, instance, id, search, occupied)?;
    commit_primitive(
        candidate,
        id,
        primitive,
        facing,
        anchor,
        logical_owner,
        occupied,
        sources,
        blocks,
    )
}

#[allow(clippy::too_many_arguments)]
fn find_primitive_placement(
    primitive: Primitive,
    facing: CellFacing,
    preferred: Anchor,
    instance: InstanceId,
    id: PrimitiveId,
    search: &mut PlacementSearch,
    occupied: &BTreeSet<Anchor>,
) -> Result<(Anchor, Vec<PlacedBlock>), SeedError> {
    for anchor in horizontal_manhattan_shells(preferred, search.max_radius) {
        let blocks = primitive_blocks(primitive, facing, anchor)?;
        if blocks.iter().all(|block| !occupied.contains(&block.at)) {
            return Ok((anchor, blocks));
        }
        search.reject_choice(instance, id)?;
    }
    Err(SeedError::PlacementExhausted {
        instance,
        primitive: id,
        radius: search.max_radius,
    })
}

fn primitive_blocks(
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
) -> Result<Vec<PlacedBlock>, SeedError> {
    let variants = physical::variants(primitive);
    let variant = variants
        .get(usize::from(facing.index()))
        .ok_or(SeedError::Incomplete("physical primitive variant"))?;
    Ok(variant
        .blocks
        .iter()
        .map(|block| PlacedBlock {
            at: translate(anchor, block.position),
            state: state_for_local(block.kind, block.facing, block.face),
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn commit_primitive(
    candidate: &mut ExpandedPhysicalCandidate,
    id: PrimitiveId,
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
    logical_owner: Option<InstanceId>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    blocks: Vec<PlacedBlock>,
) -> Result<(), SeedError> {
    claim_blocks(occupied, &blocks)?;
    let delayed = blocks
        .iter()
        .find(|block| {
            matches!(
                block.state.kind,
                BlockKind::WallTorch | BlockKind::Torch | BlockKind::Repeater
            )
        })
        .map(|block| DelayedComponent {
            at: block.at,
            owner: DelayedOwner::Primitive(id),
        });
    candidate.placements.insert(
        id,
        PrimitivePlacement {
            id,
            variant: u16::from(facing.index()),
            facing,
            anchor,
            delayed,
            blocks: blocks.clone(),
        },
    );

    let output = primitive_output_terminal(primitive, facing, anchor)?;
    let output_at = output.support;
    candidate.observations.insert(
        ObservationId::PrimitiveOutput(id),
        VerifiedObservation {
            site: ObservationSite {
                id: ObservationId::PrimitiveOutput(id),
                at: output_at,
                logical_owner,
                display_label: None,
            },
            state: block_state_at(&blocks, output_at),
        },
    );
    sources.insert(
        PhysicalEndpointId::PrimitiveOutput(id),
        SourceGeometry {
            route_anchor: output.route_anchor,
            allowed_exit: output.allowed_exit,
        },
    );
    Ok(())
}

fn horizontal_manhattan_shells(origin: Anchor, max_radius: u32) -> Vec<Anchor> {
    let mut anchors = vec![origin];
    for radius in 1..=max_radius {
        let radius = i32::try_from(radius).unwrap_or(i32::MAX);
        for dx in -radius..=radius {
            let dz = radius - dx.abs();
            anchors.push(Anchor {
                x: origin.x.saturating_add(dx),
                y: origin.y,
                z: origin.z.saturating_sub(dz),
            });
            if dz != 0 {
                anchors.push(Anchor {
                    x: origin.x.saturating_add(dx),
                    y: origin.y,
                    z: origin.z.saturating_add(dz),
                });
            }
        }
    }
    anchors
}

fn assign_primitive_targets(
    candidate: &ExpandedPhysicalCandidate,
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let mut ordinal_by_primitive = BTreeMap::<PrimitiveId, usize>::new();
    for connection in &instance.expanded.topology.connections {
        let ConnectionTarget::Primitive(primitive) = connection.target else {
            continue;
        };
        let ordinal = ordinal_by_primitive.entry(primitive).or_default();
        let geometry = primitive_input_geometry(candidate, primitive, *ordinal)?;
        *ordinal += 1;
        match connection.id {
            ConnectionId::External { input_index, .. } => {
                targets.insert(
                    PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index,
                    },
                    geometry,
                );
            }
            ConnectionId::Internal { .. } => {}
        }
    }
    Ok(())
}

fn primitive_input_geometry(
    candidate: &ExpandedPhysicalCandidate,
    primitive: PrimitiveId,
    ordinal: usize,
) -> Result<TargetGeometry, SeedError> {
    let placement = candidate
        .placements
        .get(&primitive)
        .ok_or(SeedError::Incomplete("primitive placement"))?;
    let specification = candidate
        .instances
        .instances
        .iter()
        .flat_map(|instance| &instance.expanded.topology.primitives)
        .find(|specification| specification.id == primitive)
        .ok_or(SeedError::Incomplete("primitive specification"))?;
    let input = primitive_input_terminal(
        specification.primitive,
        placement.facing,
        placement.anchor,
        ordinal,
    )?;
    let requirement = match specification.primitive {
        Primitive::Torch => TerminalRequirement::Repeater,
        Primitive::Repeater => TerminalRequirement::DirectedDust,
        _ => return Err(SeedError::Incomplete("unsupported primitive input")),
    };
    Ok(TargetGeometry {
        terminal: input.terminal,
        allowed_entry: input.allowed_entry,
        support: input.support,
        requirement,
    })
}

fn route_source_instance(source: PhysicalEndpointId) -> Option<InstanceId> {
    match source {
        PhysicalEndpointId::PrimitiveOutput(primitive) => Some(primitive.instance),
        PhysicalEndpointId::Junction(instance) => Some(instance),
        PhysicalEndpointId::PrimaryInput(_)
        | PhysicalEndpointId::DeclaredOutput(_)
        | PhysicalEndpointId::Landing(_) => None,
    }
}

fn route_target_instance(target: &PendingTarget) -> Option<InstanceId> {
    match target {
        PendingTarget::Connection(ConnectionId::External { instance, .. }, _)
        | PendingTarget::Connection(ConnectionId::Internal { instance, .. }, _) => Some(*instance),
        PendingTarget::DeclaredOutput(_, _) => None,
    }
}

fn route_source_level(source: PhysicalEndpointId, analysis: &SeedPlacementAnalysis) -> u64 {
    route_source_instance(source)
        .and_then(|instance| analysis.nodes.get(&instance))
        .map(|facts| facts.forward_level)
        .unwrap_or(0)
}

fn route_target_level(
    target: &PendingTarget,
    analysis: &SeedPlacementAnalysis,
    output_level: u64,
) -> u64 {
    route_target_instance(target)
        .and_then(|instance| analysis.nodes.get(&instance))
        .map(|facts| facts.forward_level)
        .unwrap_or(output_level)
}

fn input_boundary_slack(instance: InstanceId, analysis: &SeedPlacementAnalysis) -> u64 {
    analysis.nodes.get(&instance).map_or(0, |facts| {
        analysis
            .critical_delay_ticks
            .saturating_sub(facts.tail_ticks)
    })
}

fn output_boundary_slack(instance: InstanceId, analysis: &SeedPlacementAnalysis) -> u64 {
    analysis.nodes.get(&instance).map_or(0, |facts| {
        analysis
            .critical_delay_ticks
            .saturating_sub(facts.head_ticks)
    })
}

fn route_target_slack(
    source: PhysicalEndpointId,
    target: &PendingTarget,
    analysis: &SeedPlacementAnalysis,
) -> u64 {
    let source_instance = route_source_instance(source);
    let target_instance = route_target_instance(target);
    match (source_instance, target_instance) {
        (Some(source), Some(target)) if source == target => 0,
        (Some(source), Some(target)) => analysis
            .edges
            .iter()
            .find(|edge| edge.source == source && edge.sink == target)
            .map(|edge| edge.structural_slack_ticks)
            .unwrap_or(0),
        (None, Some(target)) => input_boundary_slack(target, analysis),
        (Some(source), None) => output_boundary_slack(source, analysis),
        (None, None) => 0,
    }
}

fn route_guidance_for_source(
    graph: &InstanceGraph,
    source: PhysicalEndpointId,
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
) -> Option<RouteGuidance> {
    let signal = match source {
        PhysicalEndpointId::PrimaryInput(port) => {
            crate::compile::fragment_synth::instance_graph::LogicalSignalId::PrimaryInput(port)
        }
        _ => {
            graph
                .assignments
                .iter()
                .find(|assignment| endpoint_for_driver(&assignment.driver) == Some(source))?
                .signal
        }
    };
    Some(RouteGuidance {
        origin: plan_translation.apply(plan.frame.origin),
        lateral: plan.frame.lateral,
        track: *plan.signal_tracks.get(&signal)?,
        half_width: 2,
        penalty_per_block: 2,
    })
}

fn route_all(
    candidate: &mut ExpandedPhysicalCandidate,
    router: &dyn PhysicalRouter,
    config: &SearchConfig,
    analysis: &SeedPlacementAnalysis,
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &BTreeMap<PhysicalSink, TargetGeometry>,
    repairs: &[LayoutRepair],
    reservations: &mut PhysicalReservations,
) -> Result<(), SeedError> {
    let mut grouped = BTreeMap::<PhysicalEndpointId, Vec<PendingTarget>>::new();
    for instance in &candidate.instances.instances {
        for connection in &instance.expanded.topology.connections {
            let source = match connection.source {
                crate::compile::fragment_synth::topology::ConnectionSource::Primitive(id) => {
                    PhysicalEndpointId::PrimitiveOutput(id)
                }
                crate::compile::fragment_synth::topology::ConnectionSource::ExternalInput {
                    input_index,
                } => candidate
                    .instances
                    .assignments
                    .iter()
                    .find(|assignment| {
                        assignment.sink
                            == PhysicalSink::InstanceInput {
                                instance: instance.id,
                                input_index,
                            }
                    })
                    .and_then(|assignment| endpoint_for_driver(&assignment.driver))
                    .ok_or(SeedError::Incomplete("external connection source"))?,
            };
            let geometry = match connection.target {
                ConnectionTarget::Primitive(primitive) => {
                    let ordinal = instance
                        .expanded
                        .topology
                        .connections
                        .iter()
                        .filter(|candidate| {
                            candidate.target == ConnectionTarget::Primitive(primitive)
                        })
                        .take_while(|candidate| candidate.id != connection.id)
                        .count();
                    primitive_input_geometry(candidate, primitive, ordinal)?
                }
                ConnectionTarget::Junction(_) => targets
                    .get(&PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index: connection_input_index(connection.id),
                    })
                    .copied()
                    .ok_or(SeedError::Incomplete("junction target"))?,
            };
            grouped
                .entry(source)
                .or_default()
                .push(PendingTarget::Connection(connection.id, geometry));
        }
    }
    for assignment in &candidate.instances.assignments {
        let PhysicalSink::DeclaredOutput(port) = assignment.sink else {
            continue;
        };
        let source = endpoint_for_driver(&assignment.driver)
            .ok_or(SeedError::Incomplete("declared output source"))?;
        let geometry = targets
            .get(&PhysicalSink::DeclaredOutput(port))
            .copied()
            .ok_or(SeedError::Incomplete("declared output target"))?;
        grouped
            .entry(source)
            .or_default()
            .push(PendingTarget::DeclaredOutput(port, geometry));
    }
    for pending in grouped.values().flatten() {
        let geometry = pending.geometry();
        if reservations.get(&geometry.terminal).is_none() {
            reservations.reserve(
                geometry.terminal,
                PhysicalReservationOwner::Endpoint(pending.endpoint()),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    let mut reserved_sink_approaches = BTreeMap::new();
    for (&source, pending_targets) in &grouped {
        for pending in pending_targets {
            let geometry = pending.geometry();
            let endpoint = pending.endpoint();
            let guards = reserve_scheduled_sink_approach(
                reservations,
                endpoint,
                geometry.terminal,
                geometry.allowed_entry,
                target_is_promoted(repairs, source, endpoint),
            );
            if !guards.is_empty() {
                reserved_sink_approaches.insert(endpoint, guards);
            }
        }
    }
    let scheduled_sources = grouped.keys().copied().collect::<BTreeSet<_>>();
    let reserved_source_escapes =
        reserve_scheduled_source_escapes(reservations, sources, &scheduled_sources)?;

    let output_level = analysis
        .nodes
        .values()
        .map(|facts| facts.forward_level)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let obligations = grouped
        .into_iter()
        .map(|(source, pending)| {
            let source_level = route_source_level(source, analysis);
            let targets = pending
                .into_iter()
                .map(|target| {
                    let target_level = route_target_level(&target, analysis, output_level);
                    TargetObligation {
                        promoted: target_is_promoted(repairs, source, target.endpoint()),
                        structural_slack_ticks: route_target_slack(source, &target, analysis),
                        forward_distance: target_level.saturating_sub(source_level),
                        key: target.key(),
                        target,
                    }
                })
                .collect::<Vec<_>>();
            let structural_slack_ticks = targets
                .iter()
                .map(|target| target.structural_slack_ticks)
                .min()
                .unwrap_or(0);
            let level_span = targets
                .iter()
                .map(|target| target.forward_distance)
                .max()
                .unwrap_or(0);
            RouteObligation {
                source,
                must_precede: repairs
                    .iter()
                    .filter_map(|repair| match repair {
                        LayoutRepair::RouteBefore {
                            source: repaired_source,
                            blocker,
                        } if *repaired_source == source => Some(*blocker),
                        _ => None,
                    })
                    .collect(),
                boundary_escape: matches!(source, PhysicalEndpointId::PrimaryInput(_)),
                structural_slack_ticks,
                fanout: targets.len(),
                level_span,
                targets,
            }
        })
        .collect();
    let schedule = RouteSchedule::build(obligations)?;
    let protected = sources
        .values()
        .map(|source| source.route_anchor)
        .chain(reserved_source_escapes.values().flatten().copied())
        .chain(
            schedule
                .routes
                .iter()
                .flat_map(|route| route.targets.iter().map(PendingTarget::geometry))
                .map(|geometry| geometry.terminal),
        )
        .collect::<BTreeSet<_>>();

    for (route_index, scheduled_route) in schedule.routes.into_iter().enumerate() {
        let source_id = scheduled_route.source;
        let route = RouteId(u32::try_from(route_index).map_err(|_| SeedError::IdentityOverflow)?);
        let source = *sources
            .get(&source_id)
            .ok_or(SeedError::Incomplete("route source geometry"))?;
        let guidance =
            route_guidance_for_source(&candidate.instances, source_id, plan, plan_translation);
        let pending = scheduled_route.targets;
        let sinks = pending
            .iter()
            .enumerate()
            .map(|(ordinal, target)| {
                let id = RoutedSinkId {
                    route,
                    ordinal: u16::try_from(ordinal).map_err(|_| SeedError::IdentityOverflow)?,
                };
                let geometry = target.geometry();
                let (endpoint, route_target) = match target {
                    PendingTarget::Connection(connection, _) => (
                        PhysicalEndpointId::Landing(*connection),
                        crate::compile::routing::RouteTarget::Connection(*connection),
                    ),
                    PendingTarget::DeclaredOutput(port, _) => (
                        PhysicalEndpointId::DeclaredOutput(*port),
                        crate::compile::routing::RouteTarget::DeclaredOutput(*port),
                    ),
                };
                Ok(RouteSink {
                    id,
                    endpoint,
                    anchor: geometry.terminal,
                    allowed_entry: geometry.allowed_entry,
                    terminal: TerminalContract::Sink {
                        target: route_target,
                        support: geometry.support,
                        requirement: geometry.requirement,
                    },
                })
            })
            .collect::<Result<Vec<_>, SeedError>>()?;
        let sinks = NonEmptyRouteSinks::new(sinks).map_err(|_| SeedError::EmptyRoute)?;
        let mut attempt_reservations = reservations.clone();
        for target in &pending {
            let endpoint = target.endpoint();
            let Some(guards) = reserved_sink_approaches.get(&endpoint) else {
                continue;
            };
            for &guard in guards {
                if !attempt_reservations.release_endpoint_keep_out(guard, endpoint) {
                    return Err(SeedError::Incomplete("sink approach reservation"));
                }
            }
        }
        if let Some(source_escapes) = reserved_source_escapes.get(&source_id) {
            for &source_escape in source_escapes {
                if !attempt_reservations.release_endpoint_keep_out(source_escape, source_id) {
                    return Err(SeedError::Incomplete("source escape reservation"));
                }
            }
        }
        if matches!(source_id, PhysicalEndpointId::Junction(_))
            && !attempt_reservations.promote_endpoint_conductor(
                source.route_anchor,
                source_id,
                route,
                compile::repeater(source.allowed_exit),
            )
        {
            return Err(SeedError::Incomplete("junction source refresh reservation"));
        }
        if matches!(source_id, PhysicalEndpointId::Junction(_)) {
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                if direction == source.allowed_exit || direction == source.allowed_exit.opposite() {
                    continue;
                }
                let side = step(source.route_anchor, direction);
                if attempt_reservations.get(&side).is_none() {
                    attempt_reservations.reserve(
                        side,
                        PhysicalReservationOwner::KeepOut(route.0),
                        PhysicalReservationKind::KeepOut,
                    );
                }
            }
        }
        let mut tree = match router.route_guided(
            RouteRequest {
                id: route,
                source: RouteEndpoint {
                    id: source_id,
                    anchor: source.route_anchor,
                    allowed_exit: source.allowed_exit,
                    terminal: TerminalContract::Source {
                        signal_strength: MAX_SIGNAL_STRENGTH,
                    },
                },
                sinks: &sinks,
                reservations: &attempt_reservations,
                limits: config.router_limits,
            },
            guidance,
        ) {
            Ok(tree) => tree,
            Err(failure) => {
                let source_escape_blocking_route = source_escape_blocking_route(
                    &attempt_reservations,
                    route,
                    source.route_anchor,
                    source.allowed_exit,
                );
                let source_escape_obstructed = source_escape_blocking_route.is_some();
                let failed_sink = explicit_failure_sink(&sinks, &failure)
                    .and_then(|id| sinks.as_slice().iter().find(|sink| sink.id == id));
                let sink_approach_blocking_route = failed_sink.and_then(|sink| {
                    sink_approach_blocking_route(
                        &attempt_reservations,
                        route,
                        source.route_anchor,
                        sink.anchor,
                        sink.allowed_entry,
                    )
                    .or_else(|| {
                        matches!(failure, RouterFailure::NoLocalRoute { .. })
                            .then(|| {
                                nearby_sink_blocking_route(
                                    &attempt_reservations,
                                    route,
                                    sink.anchor,
                                    sink.allowed_entry,
                                )
                            })
                            .flatten()
                    })
                });
                let precedence_blocker = source_escape_blocking_route
                    .or(sink_approach_blocking_route)
                    .and_then(|blocker| candidate.routes.get(&blocker).map(|tree| tree.source));
                return Err(SeedError::Routing(seed_routing_failure(
                    route_index,
                    route,
                    source_id,
                    source.route_anchor,
                    source.allowed_exit,
                    precedence_blocker,
                    source_escape_obstructed,
                    &sinks,
                    &failure,
                    &plan.fingerprint,
                )));
            }
        };
        if let Some((_, sink)) = route_self_overlap(&tree) {
            let failure = RouterFailure::Refused {
                route,
                source: source_id,
                sink: Some(sink),
                category: RouterRefusalCategory::PhysicalInvariant,
            };
            return Err(SeedError::Routing(seed_routing_failure(
                route_index,
                route,
                source_id,
                source.route_anchor,
                source.allowed_exit,
                None,
                false,
                &sinks,
                &failure,
                &plan.fingerprint,
            )));
        }
        refresh_exact_route_delays(&mut tree);

        for (target, branch) in pending.iter().zip(&tree.branches) {
            if let PendingTarget::Connection(connection, _) = target {
                candidate.connections.insert(
                    *connection,
                    ConnectionBinding {
                        id: *connection,
                        source: source_id,
                        landing: PhysicalEndpointId::Landing(*connection),
                        route,
                        sink: branch.sink,
                    },
                );
            }
        }
        reserve_route(&mut attempt_reservations, &tree, &protected);
        *reservations = attempt_reservations;
        candidate.routes.insert(route, tree);
    }
    Ok(())
}

fn target_is_promoted(
    repairs: &[LayoutRepair],
    source: PhysicalEndpointId,
    sink: PhysicalEndpointId,
) -> bool {
    repairs.contains(&LayoutRepair::ExclusiveGuardedTrack { source })
        || repairs.contains(&LayoutRepair::EarlyTreeSinkAndEscape { source, sink })
}

fn route_self_overlap(
    tree: &crate::compile::routing::RealisedRouteTree,
) -> Option<(Anchor, RoutedSinkId)> {
    let floors = tree
        .floors
        .iter()
        .map(|block| block.at)
        .collect::<BTreeSet<_>>();
    let at = tree
        .cells
        .iter()
        .map(|block| block.at)
        .find(|at| floors.contains(at))?;
    let above = Anchor { y: at.y + 1, ..at };
    let sink = tree
        .branches
        .iter()
        .find(|branch| branch.path.contains(&at) || branch.path.contains(&above))
        .or_else(|| tree.branches.first())?
        .sink;
    if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
        let branches = tree
            .branches
            .iter()
            .filter(|branch| branch.path.contains(&at) || branch.path.contains(&above))
            .map(|branch| branch.sink)
            .collect::<Vec<_>>();
        eprintln!(
            "route self overlap: route={:?} at={at:?} cell={:?} floor={:?} branches={branches:?} selected_sink={sink:?}",
            tree.id,
            tree.cells.iter().find(|block| block.at == at),
            tree.floors.iter().find(|block| block.at == at),
        );
    }
    Some((at, sink))
}

fn explicit_failure_sink(
    sinks: &NonEmptyRouteSinks,
    failure: &RouterFailure,
) -> Option<RoutedSinkId> {
    match failure {
        RouterFailure::RouterLimitExceeded { sink, .. }
        | RouterFailure::NoLocalRoute { sink, .. }
        | RouterFailure::RingClosure { sink, .. } => Some(*sink),
        RouterFailure::InvalidRequest { sink, .. } | RouterFailure::Refused { sink, .. } => *sink,
        RouterFailure::WrongRepeaterAxis { connection, .. } => sinks
            .as_slice()
            .iter()
            .find(|sink| {
                sink.terminal.target()
                    == Some(crate::compile::routing::RouteTarget::Connection(
                        *connection,
                    ))
            })
            .map(|sink| sink.id),
    }
}

fn seed_routing_failure(
    scheduled_index: usize,
    route: RouteId,
    source: PhysicalEndpointId,
    source_at: Anchor,
    source_exit: Facing,
    precedence_blocker: Option<PhysicalEndpointId>,
    source_escape_obstructed: bool,
    sinks: &NonEmptyRouteSinks,
    failure: &RouterFailure,
    plan_fingerprint: &Fingerprint,
) -> SeedRoutingFailure {
    let explicit_sink = explicit_failure_sink(sinks, failure);
    let fallback = &sinks.as_slice()[0];
    let sink = explicit_sink.unwrap_or(fallback.id);
    let sink_at = sinks
        .as_slice()
        .iter()
        .find(|candidate| candidate.id == sink)
        .map(|candidate| candidate.anchor)
        .unwrap_or(fallback.anchor);
    let sink_endpoint = sinks
        .as_slice()
        .iter()
        .find(|candidate| candidate.id == sink)
        .map(|candidate| candidate.endpoint)
        .unwrap_or(fallback.endpoint);
    let sink_entry = sinks
        .as_slice()
        .iter()
        .find(|candidate| candidate.id == sink)
        .map(|candidate| candidate.allowed_entry);
    let (limit_kind, limit, work_used) = match failure {
        RouterFailure::RouterLimitExceeded {
            kind,
            limit,
            work_used,
            ..
        } => (Some(*kind), Some(*limit), Some(*work_used)),
        _ => (None, None, None),
    };
    SeedRoutingFailure {
        scheduled_index,
        route,
        source,
        sink,
        sink_endpoint,
        fanout: sinks.as_slice().len(),
        category: failure.category(),
        limit_kind,
        limit,
        work_used,
        plan_fingerprint: plan_fingerprint.clone(),
        source_at,
        source_exit,
        precedence_blocker,
        source_escape_obstructed,
        sink_at,
        sink_entry,
    }
}

fn source_escape_blocking_route(
    reservations: &PhysicalReservations,
    current: RouteId,
    source_at: Anchor,
    allowed_exit: Facing,
) -> Option<RouteId> {
    fn foreign_route_owner(
        reservation: &crate::compile::routing::PhysicalReservation,
        current: RouteId,
    ) -> Option<RouteId> {
        match reservation.owner {
            PhysicalReservationOwner::Route(route)
            | PhysicalReservationOwner::RouteStair(route)
                if route != current =>
            {
                Some(route)
            }
            _ => None,
        }
    }

    let exit = step(source_at, allowed_exit);
    if let Some(blocker) = reservations
        .get(&exit)
        .and_then(|reservation| foreign_route_owner(reservation, current))
    {
        return Some(blocker);
    }

    let below = Anchor {
        y: exit.y - 1,
        ..exit
    };
    if let Some(blocker) = reservations.get(&below).and_then(|reservation| {
        matches!(
            reservation.kind,
            PhysicalReservationKind::Conductor(_) | PhysicalReservationKind::MandatoryAir
        )
        .then(|| foreign_route_owner(reservation, current))
        .flatten()
    }) {
        return Some(blocker);
    }

    for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let neighbour = step(exit, direction);
        for at in [
            neighbour,
            Anchor {
                y: neighbour.y + 1,
                ..neighbour
            },
            Anchor {
                y: neighbour.y - 1,
                ..neighbour
            },
        ] {
            let Some(reservation) = reservations.get(&at) else {
                continue;
            };
            if !matches!(reservation.kind, PhysicalReservationKind::Conductor(_)) {
                continue;
            }
            if let Some(blocker) = foreign_route_owner(reservation, current) {
                return Some(blocker);
            }
        }
    }
    None
}

fn sink_approach_blocking_route(
    reservations: &PhysicalReservations,
    current: RouteId,
    source_at: Anchor,
    terminal: Anchor,
    allowed_entry: Facing,
) -> Option<RouteId> {
    let foreign_route_owner =
        |reservation: &crate::compile::routing::PhysicalReservation| match reservation.owner {
            PhysicalReservationOwner::Route(route)
            | PhysicalReservationOwner::RouteStair(route)
                if route != current =>
            {
                Some(route)
            }
            _ => None,
        };
    let approach = step(terminal, allowed_entry);
    if approach != source_at {
        if let Some(blocker) = reservations.get(&approach).and_then(foreign_route_owner) {
            return Some(blocker);
        }
    }
    let below = Anchor {
        y: approach.y - 1,
        ..approach
    };
    if let Some(blocker) = reservations.get(&below).and_then(|reservation| {
        matches!(
            reservation.kind,
            PhysicalReservationKind::Conductor(_) | PhysicalReservationKind::MandatoryAir
        )
        .then(|| foreign_route_owner(reservation))
        .flatten()
    }) {
        return Some(blocker);
    }
    for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let neighbour = step(approach, direction);
        for at in [
            neighbour,
            Anchor {
                y: neighbour.y + 1,
                ..neighbour
            },
            Anchor {
                y: neighbour.y - 1,
                ..neighbour
            },
        ] {
            if at == source_at || at == terminal {
                continue;
            }
            let Some(reservation) = reservations.get(&at) else {
                continue;
            };
            if !matches!(reservation.kind, PhysicalReservationKind::Conductor(_)) {
                continue;
            }
            if let Some(blocker) = foreign_route_owner(reservation) {
                return Some(blocker);
            }
        }
    }
    None
}

fn nearby_sink_blocking_route(
    reservations: &PhysicalReservations,
    current: RouteId,
    terminal: Anchor,
    allowed_entry: Facing,
) -> Option<RouteId> {
    let approach = step(terminal, allowed_entry);
    for distance in 2_i32..=3 {
        for dx in -distance..=distance {
            let dz = distance - dx.abs();
            for signed_dz in if dz == 0 { vec![0] } else { vec![-dz, dz] } {
                for dy in -1..=2 {
                    let at = Anchor {
                        x: approach.x.saturating_add(dx),
                        y: approach.y.saturating_add(dy),
                        z: approach.z.saturating_add(signed_dz),
                    };
                    let Some(reservation) = reservations.get(&at) else {
                        continue;
                    };
                    if !matches!(reservation.kind, PhysicalReservationKind::Conductor(_)) {
                        continue;
                    }
                    if let PhysicalReservationOwner::Route(route) = reservation.owner {
                        if route != current {
                            return Some(route);
                        }
                    }
                }
            }
        }
    }
    None
}

fn reserve_source_escape_footprint(
    reservations: &mut PhysicalReservations,
    source: PhysicalEndpointId,
    source_at: Anchor,
    allowed_exit: Facing,
) -> Vec<Anchor> {
    let (core, halo) = source_escape_footprint(source_at, allowed_exit);
    core.into_iter()
        .chain(halo)
        .filter(|at| {
            if reservations.get(at).is_some() {
                return false;
            }
            reservations.reserve(
                *at,
                PhysicalReservationOwner::Endpoint(source),
                PhysicalReservationKind::KeepOut,
            );
            true
        })
        .collect()
}

fn reserve_scheduled_source_escapes(
    reservations: &mut PhysicalReservations,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    scheduled: &BTreeSet<PhysicalEndpointId>,
) -> Result<BTreeMap<PhysicalEndpointId, Vec<Anchor>>, SeedError> {
    let footprints = scheduled
        .iter()
        .copied()
        .map(|endpoint| {
            let source = sources
                .get(&endpoint)
                .ok_or(SeedError::Incomplete("scheduled source geometry"))?;
            Ok((
                endpoint,
                source_escape_footprint(source.route_anchor, source.allowed_exit),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, SeedError>>()?;
    let mut guarded = BTreeMap::<PhysicalEndpointId, Vec<Anchor>>::new();
    for halo_phase in [false, true] {
        for (&endpoint, (core, halo)) in &footprints {
            let cells = if halo_phase { halo } else { core };
            for &at in cells {
                if let Some(reservation) = reservations.get(&at) {
                    if reservation.owner != PhysicalReservationOwner::Endpoint(endpoint)
                        && std::env::var_os("REDA_TRACE_SOURCE_GUARDS").is_some()
                    {
                        eprintln!(
                            "source guard conflict: endpoint={endpoint:?} phase={} at={at:?} reservation={:?}",
                            if halo_phase { "halo" } else { "core" },
                            reservation,
                        );
                    }
                    continue;
                }
                reservations.reserve(
                    at,
                    PhysicalReservationOwner::Endpoint(endpoint),
                    PhysicalReservationKind::KeepOut,
                );
                guarded.entry(endpoint).or_default().push(at);
            }
        }
    }
    for cells in guarded.values_mut() {
        cells.sort();
    }
    Ok(guarded)
}

fn source_escape_footprint(
    source_at: Anchor,
    allowed_exit: Facing,
) -> (BTreeSet<Anchor>, BTreeSet<Anchor>) {
    let [exit, runway, mouth] = source_escape_corridor(source_at, allowed_exit);
    let core = BTreeSet::from([
        exit,
        Anchor {
            y: exit.y - 1,
            ..exit
        },
        runway,
        Anchor {
            y: runway.y - 1,
            ..runway
        },
        mouth,
        Anchor {
            y: mouth.y - 1,
            ..mouth
        },
    ]);
    let mut halo = BTreeSet::new();
    for center in [exit, runway, mouth] {
        for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let neighbour = step(center, direction);
            for dy in [-1, 0, 1] {
                halo.insert(Anchor {
                    y: neighbour.y + dy,
                    ..neighbour
                });
            }
        }
    }
    halo.retain(|at| !core.contains(at));
    (core, halo)
}

fn reserve_sink_approach_footprint(
    reservations: &mut PhysicalReservations,
    endpoint: PhysicalEndpointId,
    terminal: Anchor,
    allowed_entry: Facing,
) -> Vec<Anchor> {
    let approach = step(terminal, allowed_entry);
    let mut footprint = BTreeSet::from([
        approach,
        Anchor {
            y: approach.y - 1,
            ..approach
        },
    ]);
    for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let neighbour = step(approach, direction);
        footprint.insert(neighbour);
        footprint.insert(Anchor {
            y: neighbour.y + 1,
            ..neighbour
        });
        footprint.insert(Anchor {
            y: neighbour.y - 1,
            ..neighbour
        });
    }
    footprint
        .into_iter()
        .filter(|at| *at != terminal)
        .filter(|at| {
            if reservations.get(at).is_some() {
                return false;
            }
            reservations.reserve(
                *at,
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            );
            true
        })
        .collect()
}

fn reserve_sink_runway_footprint(
    reservations: &mut PhysicalReservations,
    endpoint: PhysicalEndpointId,
    terminal: Anchor,
    allowed_entry: Facing,
) -> Vec<Anchor> {
    let approach = step(terminal, allowed_entry);
    let runway = step(approach, allowed_entry);
    [
        approach,
        Anchor {
            y: approach.y - 1,
            ..approach
        },
        runway,
        Anchor {
            y: runway.y - 1,
            ..runway
        },
    ]
    .into_iter()
    .filter(|at| {
        if reservations.get(at).is_some() {
            return false;
        }
        reservations.reserve(
            *at,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );
        true
    })
    .collect()
}

fn reserve_exclusive_sink_approach_footprint(
    reservations: &mut PhysicalReservations,
    endpoint: PhysicalEndpointId,
    terminal: Anchor,
    allowed_entry: Facing,
) -> Vec<Anchor> {
    let mut guarded =
        reserve_sink_approach_footprint(reservations, endpoint, terminal, allowed_entry);
    let approach = step(terminal, allowed_entry);
    for dx in -2_i32..=2 {
        for dz in -2_i32..=2 {
            if dx.abs() + dz.abs() != 2 {
                continue;
            }
            for dy in -1_i32..=2 {
                let at = Anchor {
                    x: approach.x.saturating_add(dx),
                    y: approach.y.saturating_add(dy),
                    z: approach.z.saturating_add(dz),
                };
                if at == terminal || reservations.get(&at).is_some() {
                    continue;
                }
                reservations.reserve(
                    at,
                    PhysicalReservationOwner::Endpoint(endpoint),
                    PhysicalReservationKind::KeepOut,
                );
                guarded.push(at);
            }
        }
    }
    guarded.sort();
    guarded
}

fn reserve_scheduled_sink_approach(
    reservations: &mut PhysicalReservations,
    endpoint: PhysicalEndpointId,
    terminal: Anchor,
    allowed_entry: Facing,
    exclusive: bool,
) -> Vec<Anchor> {
    if exclusive {
        reserve_exclusive_sink_approach_footprint(reservations, endpoint, terminal, allowed_entry)
    } else {
        reserve_sink_runway_footprint(reservations, endpoint, terminal, allowed_entry)
    }
}

fn refresh_exact_route_delays(tree: &mut crate::compile::routing::RealisedRouteTree) {
    let repeaters = tree
        .cells
        .iter()
        .filter(|block| block.state.kind == BlockKind::Repeater)
        .map(|block| block.at)
        .collect::<BTreeSet<_>>();
    for branch in &mut tree.branches {
        branch.terminal.repeaters = branch
            .path
            .iter()
            .filter(|at| repeaters.contains(at))
            .filter(|at| {
                !(**at == branch.terminal.at
                    && branch.terminal.kind
                        == crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater)
            })
            .count() as u64;
    }
}

fn reservations_for_components(
    candidate: &ExpandedPhysicalCandidate,
) -> Result<PhysicalReservations, SeedError> {
    let mut reservations = PhysicalReservations::new();
    let mut ordinal = 0u32;
    for block in candidate
        .placements
        .values()
        .flat_map(|placement| &placement.blocks)
        .chain(
            candidate
                .boundaries
                .values()
                .flat_map(|boundary| &boundary.blocks),
        )
        .chain(
            candidate
                .junctions
                .values()
                .flat_map(|junction| &junction.cells),
        )
    {
        if reservations.get(&block.at).is_some() {
            return Err(SeedError::PlacementCollision { at: block.at });
        }
        reservations.reserve(
            block.at,
            PhysicalReservationOwner::KeepOut(ordinal),
            PhysicalReservationKind::KeepOut,
        );
        ordinal = ordinal.saturating_add(1);
    }
    Ok(reservations)
}

fn reserve_route(
    reservations: &mut PhysicalReservations,
    tree: &crate::compile::routing::RealisedRouteTree,
    protected: &BTreeSet<Anchor>,
) {
    for block in &tree.cells {
        reservations.reserve(
            block.at,
            PhysicalReservationOwner::Route(tree.id),
            PhysicalReservationKind::Conductor(block.state.clone()),
        );
    }
    for block in &tree.floors {
        reservations.reserve(
            block.at,
            PhysicalReservationOwner::RouteStair(tree.id),
            PhysicalReservationKind::Floor(block.state.clone()),
        );
    }
    for block in &tree.cells {
        for halo in route_conductor_clearance(block.at) {
            if protected.contains(&halo) || reservations.get(&halo).is_some() {
                continue;
            }
            reservations.reserve(
                halo,
                PhysicalReservationOwner::KeepOut(tree.id.0),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
}

fn route_conductor_clearance(at: Anchor) -> BTreeSet<Anchor> {
    let mut clearance = BTreeSet::new();
    for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let neighbour = step(at, direction);
        for dy in -1..=1 {
            clearance.insert(Anchor {
                y: neighbour.y.saturating_add(dy),
                ..neighbour
            });
        }
    }
    clearance
}

fn topological_instance_order(graph: &InstanceGraph) -> Vec<InstanceId> {
    let ids = graph
        .instances
        .iter()
        .map(|instance| instance.id)
        .collect::<BTreeSet<_>>();
    let mut predecessors = ids
        .iter()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for assignment in &graph.assignments {
        let PhysicalSink::InstanceInput { instance, .. } = assignment.sink else {
            continue;
        };
        if let PhysicalDriver::Instance(driver) = &assignment.driver {
            let owner = match driver {
                crate::compile::fragment_synth::instance_graph::InstanceDriver::Primitive {
                    logical_owner,
                    ..
                }
                | crate::compile::fragment_synth::instance_graph::InstanceDriver::Junction {
                    logical_owner,
                    ..
                } => *logical_owner,
            };
            if owner != instance {
                predecessors.entry(instance).or_default().insert(owner);
            }
        }
    }
    let mut remaining = ids;
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let next = remaining.iter().copied().find(|id| {
            predecessors[id]
                .iter()
                .all(|before| !remaining.contains(before))
        });
        let Some(next) = next else {
            ordered.extend(remaining);
            break;
        };
        remaining.remove(&next);
        ordered.push(next);
    }
    ordered
}

fn repeater_facing_with_front(direction: Facing) -> Result<CellFacing, SeedError> {
    (0..4u8)
        .filter_map(CellFacing::from_index)
        .find(|facing| {
            physical::variants(Primitive::Repeater)[usize::from(facing.index())]
                .port(PortKind::RepeaterFront)
                .direction
                == direction
        })
        .ok_or(SeedError::Incomplete("repeater front direction"))
}

fn contributor_endpoint(contributor: &ContributorSpec) -> PhysicalEndpointId {
    match *contributor {
        ContributorSpec::Landing(connection) => PhysicalEndpointId::Landing(connection),
        ContributorSpec::Primitive(primitive) => PhysicalEndpointId::PrimitiveOutput(primitive),
    }
}

fn connection_input_index(connection: ConnectionId) -> u16 {
    match connection {
        ConnectionId::External { input_index, .. } => input_index,
        ConnectionId::Internal { edge_index, .. } => edge_index,
    }
}

fn claim_blocks(occupied: &mut BTreeSet<Anchor>, blocks: &[PlacedBlock]) -> Result<(), SeedError> {
    for block in blocks {
        if occupied.contains(&block.at) {
            return Err(SeedError::PlacementCollision { at: block.at });
        }
    }
    occupied.extend(blocks.iter().map(|block| block.at));
    Ok(())
}

fn block_state_at(blocks: &[PlacedBlock], at: Anchor) -> BlockState {
    blocks
        .iter()
        .find(|block| block.at == at)
        .map(|block| block.state.clone())
        .unwrap_or_else(BlockState::air)
}

fn translate(anchor: Anchor, local: Position) -> Anchor {
    Anchor {
        x: anchor.x + local.x,
        y: anchor.y + local.y,
        z: anchor.z + local.z,
    }
}

fn state_for_local(
    kind: BlockKind,
    facing: Option<Facing>,
    face: Option<crate::redstone::world::block::Face>,
) -> BlockState {
    let mut state = match kind {
        BlockKind::Solid => compile::stone(),
        BlockKind::Repeater => {
            let mut state = BlockState::air();
            state.kind = BlockKind::Repeater;
            state.name = "minecraft:repeater".to_string();
            state.delay = 1;
            state.lit = true;
            state
        }
        BlockKind::WallTorch => {
            let mut state = BlockState::air();
            state.kind = BlockKind::WallTorch;
            state.name = "minecraft:redstone_wall_torch".to_string();
            state.lit = true;
            state
        }
        BlockKind::Lever => compile::lever(false),
        BlockKind::Lamp => compile::lamp(),
        _ => {
            let mut state = BlockState::air();
            state.kind = kind;
            state
        }
    };
    state.facing = facing;
    state.face = face;
    state
}

fn step(at: Anchor, direction: Facing) -> Anchor {
    match direction {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

fn step_many(at: Anchor, direction: Facing, distance: i32) -> Anchor {
    match direction {
        Facing::North => Anchor {
            z: at.z.saturating_sub(distance),
            ..at
        },
        Facing::South => Anchor {
            z: at.z.saturating_add(distance),
            ..at
        },
        Facing::East => Anchor {
            x: at.x.saturating_add(distance),
            ..at
        },
        Facing::West => Anchor {
            x: at.x.saturating_sub(distance),
            ..at
        },
        Facing::Up => Anchor {
            y: at.y.saturating_add(distance),
            ..at
        },
        Facing::Down => Anchor {
            y: at.y.saturating_sub(distance),
            ..at
        },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::emission::{EmittedWorld, PhysicalCandidateView};
    use crate::compile::fragment_synth::benchmark::legacy_benchmark_evaluator;
    use crate::compile::fragment_synth::certification::CompleteCandidateCertifier;
    use crate::compile::fragment_synth::legacy_adapter::{LegacyCandidateAdapter, LegacyOracle};
    use crate::compile::fragment_synth::placement::{
        PreferredInstancePose, SeedPlacementError, SeedPlacementPlan, SeedPlacementRequest,
        SeedPlacer, TopologyAwareSeedPlacer,
    };
    use crate::compile::fragment_synth::services::{DurableSeedEmitter, DurableSeedVerifier};
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::routing::{DurablePhysicalRouter, RealisedRouteTree};
    use crate::compile::Gate;

    #[test]
    fn unpinned_plan_translation_covers_primitive_owner_offsets() {
        let instance = InstanceId(0);
        let primitive = PrimitiveId {
            instance,
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        };
        let plan = SeedPlacementPlan {
            frame: crate::compile::fragment_synth::placement::PlacementFrame {
                forward: Facing::East,
                lateral: Facing::South,
                origin: Anchor { x: 0, y: 1, z: 0 },
            },
            signal_tracks: BTreeMap::new(),
            instances: BTreeMap::from([(
                instance,
                PreferredInstancePose {
                    preferred_origin: Anchor { x: 0, y: 1, z: 0 },
                    facing: CellFacing::NORTH,
                },
            )]),
            automatic_inputs: BTreeMap::new(),
            automatic_outputs: BTreeMap::new(),
            owner_offsets: BTreeMap::from([(
                LayoutOwner::Primitive(primitive),
                Anchor {
                    x: -24,
                    y: 0,
                    z: -18,
                },
            )]),
            fingerprint: canonical_fingerprint(b"translated-owner-offset"),
        };

        let translation = PlanTranslation::for_unpinned(&plan, false);

        assert_eq!(translation.dx, ORIGIN_WORLD_MARGIN + 24);
        assert_eq!(translation.dz, ORIGIN_WORLD_MARGIN + 18);
        assert_eq!(
            translation.apply(Anchor {
                x: -24,
                y: 1,
                z: -18,
            }),
            Anchor {
                x: ORIGIN_WORLD_MARGIN,
                y: 1,
                z: ORIGIN_WORLD_MARGIN,
            }
        );
    }

    #[test]
    fn route_guidance_uses_the_planned_signal_track_after_world_translation() {
        let netlist = not_netlist();
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &BTreeMap::new(),
            })
            .unwrap();
        let translation = PlanTranslation { dx: 11, dz: 17 };

        let guidance = route_guidance_for_source(
            &graph,
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            &plan,
            translation,
        )
        .unwrap();

        assert_eq!(guidance.origin, translation.apply(plan.frame.origin));
        assert_eq!(guidance.lateral, plan.frame.lateral);
        assert_eq!(
            guidance.track,
            plan.signal_tracks
                [&crate::compile::fragment_synth::instance_graph::LogicalSignalId::PrimaryInput(
                    PortId(0)
                )]
        );
    }

    #[test]
    fn route_guidance_recovers_the_logical_track_for_an_instance_driver() {
        let netlist = not_netlist();
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &BTreeMap::new(),
            })
            .unwrap();
        let output_signal =
            crate::compile::fragment_synth::instance_graph::LogicalSignalId::GateOutput(
                crate::compile::fragment_synth::identity::GateIndex(0),
            );
        let source = graph
            .assignments
            .iter()
            .find(|assignment| assignment.signal == output_signal)
            .and_then(|assignment| endpoint_for_driver(&assignment.driver))
            .unwrap();

        let guidance =
            route_guidance_for_source(&graph, source, &plan, PlanTranslation { dx: 0, dz: 0 })
                .unwrap();

        assert_eq!(guidance.track, plan.signal_tracks[&output_signal]);
    }

    #[test]
    fn repair_owner_mapping_keeps_cell_topology_atomic() {
        let instance = InstanceId(7);
        let primitive = PrimitiveId {
            instance,
            node: crate::compile::fragment_synth::identity::TopologyNodeId(3),
        };
        let landing = PhysicalEndpointId::Landing(ConnectionId::External {
            instance,
            input_index: 1,
        });

        assert_eq!(
            layout_owner_for_endpoint(PhysicalEndpointId::PrimitiveOutput(primitive)),
            LayoutOwner::Instance(instance),
        );
        assert_eq!(
            layout_owner_for_endpoint(PhysicalEndpointId::Junction(instance)),
            LayoutOwner::Instance(instance),
        );
        assert_eq!(
            layout_owner_for_endpoint(landing),
            LayoutOwner::Instance(instance),
        );
        assert_eq!(
            layout_owner_for_endpoint(PhysicalEndpointId::PrimaryInput(PortId(2))),
            LayoutOwner::Boundary(PhysicalEndpointId::PrimaryInput(PortId(2))),
        );
    }

    #[test]
    fn precedence_repair_rejects_direct_and_indirect_cycles() {
        let first = PhysicalEndpointId::PrimaryInput(PortId(0));
        let second = PhysicalEndpointId::PrimaryInput(PortId(1));
        let third = PhysicalEndpointId::PrimaryInput(PortId(2));
        let repairs = BTreeSet::from([
            LayoutRepair::RouteBefore {
                source: first,
                blocker: second,
            },
            LayoutRepair::RouteBefore {
                source: second,
                blocker: third,
            },
        ]);

        assert!(route_precedence_would_cycle(&repairs, second, first));
        assert!(route_precedence_would_cycle(&repairs, third, first));
        assert!(!route_precedence_would_cycle(&repairs, first, third));
        assert!(route_precedence_would_cycle(&repairs, first, first));
    }

    #[test]
    fn schedule_learning_has_a_separate_bounded_budget_from_layout_repairs() {
        let source = PhysicalEndpointId::PrimaryInput(PortId(0));
        let blocker = PhysicalEndpointId::PrimaryInput(PortId(1));
        let precedence = LayoutRepair::RouteBefore { source, blocker };
        let layout = LayoutRepair::ExclusiveGuardedTrack { source };
        let mut budget = SeedRepairBudget::new(2, 2);

        assert!(budget.try_charge(&precedence));
        assert!(budget.try_charge(&precedence));
        assert!(!budget.try_charge(&precedence));
        assert!(budget.try_charge(&layout));
        assert!(!budget.try_charge(&layout));
    }

    #[test]
    fn ownership_ledger_ignores_repeated_claims_from_the_same_owner() {
        let at = Anchor { x: 7, y: 2, z: 9 };
        let route = SeedPhysicalOwner::Route(RouteId(3));
        let primitive = SeedPhysicalOwner::Primitive(PrimitiveId {
            instance: InstanceId(4),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let mut ledger = BTreeMap::new();

        assert_eq!(claim_seed_owner(&mut ledger, at, route), None);
        assert_eq!(claim_seed_owner(&mut ledger, at, route), None);
        assert_eq!(
            claim_seed_owner(&mut ledger, at, primitive),
            Some((route, primitive))
        );
    }

    #[test]
    fn cross_route_refusal_names_the_driver_cells_instead_of_arbitrary_sinks() {
        let first = RouteId(9);
        let second = RouteId(16);
        let first_source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(4),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let second_source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(17),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let candidate = ExpandedPhysicalCandidate {
            instances: InstanceGraph {
                instances: Vec::new(),
                assignments: Vec::new(),
                primary_inputs: Vec::new(),
                declared_outputs: Vec::new(),
            },
            placements: BTreeMap::new(),
            boundaries: BTreeMap::new(),
            connections: BTreeMap::new(),
            routes: BTreeMap::from([
                (
                    first,
                    RealisedRouteTree {
                        id: first,
                        source: first_source,
                        cells: Vec::new(),
                        floors: Vec::new(),
                        branches: Vec::new(),
                    },
                ),
                (
                    second,
                    RealisedRouteTree {
                        id: second,
                        source: second_source,
                        cells: Vec::new(),
                        floors: Vec::new(),
                        branches: Vec::new(),
                    },
                ),
            ]),
            junctions: BTreeMap::new(),
            observations: BTreeMap::new(),
            pins: PortPlacements::default(),
            pin_contracts: BTreeMap::new(),
            pin_name_bindings: BTreeMap::new(),
        };

        assert_eq!(
            cross_route_connectivity_refusal(
                &candidate,
                first,
                second,
                Anchor { x: 53, y: 1, z: 56 },
                &[],
            )
            .unwrap(),
            SeedRepairRefusal::CrossRouteConnectivity {
                first,
                second,
                first_source,
                second_source,
                at: Anchor { x: 53, y: 1, z: 56 },
                guarded_source: second_source,
            }
        );
    }

    #[test]
    fn routing_repairs_precede_then_prepare_the_whole_fanout_before_one_sink() {
        let source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(5),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let sink_endpoint = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(7),
            input_index: 1,
        });
        let blocker = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(6),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let failure = SeedRoutingFailure {
            scheduled_index: 3,
            route: RouteId(3),
            source,
            sink: RoutedSinkId {
                route: RouteId(3),
                ordinal: 0,
            },
            sink_endpoint,
            fanout: 6,
            category: RouterRefusalCategory::NoLocalRoute,
            limit_kind: None,
            limit: None,
            work_used: None,
            plan_fingerprint: canonical_fingerprint(b"fanout-repair-policy"),
            source_at: Anchor { x: 31, y: 1, z: 46 },
            source_exit: Facing::East,
            precedence_blocker: Some(blocker),
            source_escape_obstructed: false,
            sink_at: Anchor { x: 36, y: 1, z: 17 },
            sink_entry: Some(Facing::South),
        };
        let refusal = SeedRepairRefusal::Routing(failure);

        let promotion = LayoutRepair::EarlyTreeSinkAndEscape {
            source,
            sink: sink_endpoint,
        };
        let preparation = LayoutRepair::ExclusiveGuardedTrack { source };
        let precedence = LayoutRepair::RouteBefore { source, blocker };
        assert_eq!(
            next_layout_repair(&refusal, &BTreeSet::new()).unwrap(),
            precedence,
        );
        assert_eq!(
            next_layout_repair(&refusal, &BTreeSet::from([precedence])).unwrap(),
            preparation,
        );
        assert_eq!(
            next_layout_repair(&refusal, &BTreeSet::from([precedence, preparation])).unwrap(),
            promotion,
        );

        let SeedRepairRefusal::Routing(mut unblocked) = refusal.clone() else {
            unreachable!()
        };
        unblocked.fanout = 1;
        unblocked.precedence_blocker = None;
        assert_eq!(
            next_layout_repair(&SeedRepairRefusal::Routing(unblocked), &BTreeSet::new(),).unwrap(),
            LayoutRepair::SeparateOwners {
                source_owner: LayoutOwner::Instance(InstanceId(5)),
                sink_owner: LayoutOwner::Instance(InstanceId(7)),
                axis: SeparationAxis::Runway(RunwayDirection::East),
                ordinal: 0,
            },
        );
        assert_eq!(
            next_layout_repair(
                &refusal,
                &BTreeSet::from([precedence, preparation, promotion]),
            )
            .unwrap(),
            LayoutRepair::SeparateOwners {
                source_owner: LayoutOwner::Instance(InstanceId(5)),
                sink_owner: LayoutOwner::Instance(InstanceId(7)),
                axis: SeparationAxis::Lateral,
                ordinal: 0,
            },
        );
    }

    #[test]
    fn source_wide_fanout_preparation_promotes_every_sink_of_that_source() {
        let source = PhysicalEndpointId::PrimaryInput(PortId(0));
        let other_source = PhysicalEndpointId::PrimaryInput(PortId(1));
        let first_sink = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(2),
            input_index: 0,
        });
        let second_sink = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(3),
            input_index: 1,
        });
        let repairs = [LayoutRepair::ExclusiveGuardedTrack { source }];

        assert!(target_is_promoted(&repairs, source, first_sink));
        assert!(target_is_promoted(&repairs, source, second_sink));
        assert!(!target_is_promoted(&repairs, other_source, first_sink));
    }

    #[test]
    fn placement_search_visits_stable_manhattan_shells_and_skips_collisions() {
        let preferred = Anchor { x: 8, y: 4, z: 9 };
        let shell = horizontal_manhattan_shells(preferred, 1);
        assert_eq!(
            shell,
            vec![
                preferred,
                Anchor { x: 7, y: 4, z: 9 },
                Anchor { x: 8, y: 4, z: 8 },
                Anchor { x: 8, y: 4, z: 10 },
                Anchor { x: 9, y: 4, z: 9 },
            ]
        );

        let occupied = primitive_blocks(Primitive::Torch, CellFacing::EAST, preferred)
            .unwrap()
            .into_iter()
            .map(|block| block.at)
            .collect::<BTreeSet<_>>();
        let mut search = PlacementSearch {
            max_radius: 4,
            max_backtracks: 100,
            backtracks_used: 0,
        };
        let (selected, blocks) = find_primitive_placement(
            Primitive::Torch,
            CellFacing::EAST,
            preferred,
            InstanceId(3),
            PrimitiveId {
                instance: InstanceId(3),
                node: crate::compile::fragment_synth::identity::TopologyNodeId(2),
            },
            &mut search,
            &occupied,
        )
        .unwrap();

        assert_ne!(selected, preferred);
        assert!(search.backtracks_used > 0);
        assert!(blocks.iter().all(|block| !occupied.contains(&block.at)));
    }

    #[test]
    fn placement_search_honours_the_backtrack_cap() {
        let preferred = Anchor { x: 8, y: 4, z: 9 };
        let occupied = primitive_blocks(Primitive::Torch, CellFacing::EAST, preferred)
            .unwrap()
            .into_iter()
            .map(|block| block.at)
            .collect::<BTreeSet<_>>();
        let primitive = PrimitiveId {
            instance: InstanceId(3),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(2),
        };
        let mut search = PlacementSearch {
            max_radius: 4,
            max_backtracks: 1,
            backtracks_used: 0,
        };

        let error = find_primitive_placement(
            Primitive::Torch,
            CellFacing::EAST,
            preferred,
            InstanceId(3),
            primitive,
            &mut search,
            &occupied,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SeedError::PlacementExhausted {
                instance: InstanceId(3),
                primitive: actual,
                radius: 4,
            } if actual == primitive
        ));
    }

    fn not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![crate::compile::Gate {
                name: "not".to_string(),
                inputs: vec!["a".to_string()],
                output: "y".to_string(),
                kind: crate::compile::topology::GateKind::Nor(1),
            }],
        }
    }

    fn one_typed_sink(route: RouteId) -> NonEmptyRouteSinks {
        NonEmptyRouteSinks::new(vec![RouteSink {
            id: RoutedSinkId { route, ordinal: 0 },
            endpoint: PhysicalEndpointId::DeclaredOutput(PortId(0)),
            anchor: Anchor { x: 9, y: 2, z: 7 },
            allowed_entry: Facing::West,
            terminal: TerminalContract::Sink {
                target: crate::compile::routing::RouteTarget::DeclaredOutput(PortId(0)),
                support: Anchor { x: 10, y: 2, z: 7 },
                requirement: TerminalRequirement::Exact(
                    crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                ),
            },
        }])
        .unwrap()
    }

    #[test]
    fn boundary_route_slack_counts_instance_delay_once() {
        let instance = InstanceId(0);
        let analysis = SeedPlacementAnalysis {
            order: vec![instance],
            nodes: BTreeMap::from([(
                instance,
                crate::compile::fragment_synth::placement::NodeFacts {
                    predecessors: Vec::new(),
                    successors: Vec::new(),
                    forward_level: 0,
                    reverse_level: 0,
                    head_ticks: 4,
                    tail_ticks: 8,
                },
            )]),
            edges: Vec::new(),
            critical_delay_ticks: 10,
        };
        let geometry = TargetGeometry {
            terminal: Anchor { x: 4, y: 1, z: 4 },
            allowed_entry: Facing::West,
            support: Anchor { x: 5, y: 1, z: 4 },
            requirement: TerminalRequirement::DirectedDust,
        };
        let input_target = PendingTarget::Connection(
            ConnectionId::External {
                instance,
                input_index: 0,
            },
            geometry,
        );
        let output_target = PendingTarget::DeclaredOutput(PortId(0), geometry);
        let output_source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance,
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });

        assert_eq!(
            route_target_slack(
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                &input_target,
                &analysis,
            ),
            2,
        );
        assert_eq!(
            route_target_slack(output_source, &output_target, &analysis),
            6,
        );
    }

    #[test]
    fn router_limit_failure_keeps_exact_schedule_geometry_and_cap_work() {
        let route = RouteId(3);
        let source = PhysicalEndpointId::PrimaryInput(PortId(2));
        let source_at = Anchor { x: 2, y: 2, z: 7 };
        let sinks = one_typed_sink(route);
        let plan_fingerprint = canonical_fingerprint(b"typed-limit-plan");
        let failure = RouterFailure::RouterLimitExceeded {
            route,
            source,
            sink: RoutedSinkId { route, ordinal: 0 },
            kind: crate::compile::routing::RouterLimitKind::QueueEntries,
            limit: 262_144,
            work_used: 262_145,
        };

        let evidence = seed_routing_failure(
            7,
            route,
            source,
            source_at,
            Facing::East,
            None,
            false,
            &sinks,
            &failure,
            &plan_fingerprint,
        );

        assert_eq!(evidence.scheduled_index, 7);
        assert_eq!(evidence.route, route);
        assert_eq!(evidence.source, source);
        assert_eq!(evidence.sink, RoutedSinkId { route, ordinal: 0 });
        assert_eq!(
            evidence.category,
            crate::compile::routing::RouterRefusalCategory::InvalidRequest
        );
        assert_eq!(evidence.limit, Some(262_144));
        assert_eq!(evidence.work_used, Some(262_145));
        assert_eq!(
            evidence.limit_kind,
            Some(crate::compile::routing::RouterLimitKind::QueueEntries)
        );
        assert_eq!(evidence.plan_fingerprint, plan_fingerprint);
        assert_eq!(evidence.source_at, source_at);
        assert_eq!(evidence.source_exit, Facing::East);
        assert_eq!(evidence.sink_at, Anchor { x: 9, y: 2, z: 7 });
    }

    #[test]
    fn source_escape_blocker_is_found_in_the_allowed_exit_halo() {
        let current = RouteId(9);
        let blocker = RouteId(2);
        let source_at = Anchor { x: 24, y: 1, z: 35 };
        let exit = step(source_at, Facing::East);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            exit,
            PhysicalReservationOwner::KeepOut(blocker.0),
            PhysicalReservationKind::KeepOut,
        );
        reservations.reserve_conductor(step(exit, Facing::North), blocker, compile::dust());

        assert_eq!(
            source_escape_blocking_route(&reservations, current, source_at, Facing::East),
            Some(blocker),
        );
    }

    #[test]
    fn route_clearance_blocks_dust_that_could_climb_from_an_adjacent_layer() {
        let conductor = Anchor { x: 28, y: 2, z: 34 };

        let clearance = route_conductor_clearance(conductor);

        assert!(clearance.contains(&Anchor { x: 29, y: 1, z: 34 }));
        assert!(clearance.contains(&Anchor { x: 29, y: 2, z: 34 }));
        assert!(clearance.contains(&Anchor { x: 29, y: 3, z: 34 }));
        assert!(!clearance.contains(&Anchor { x: 28, y: 1, z: 34 }));
    }

    #[test]
    fn sink_approach_blocker_is_found_in_the_strict_goal_halo() {
        let current = RouteId(22);
        let blocker = RouteId(14);
        let source_at = Anchor { x: 31, y: 1, z: 58 };
        let terminal = Anchor { x: 36, y: 1, z: 32 };
        let approach = step(terminal, Facing::North);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve_conductor(
            Anchor {
                y: approach.y + 1,
                z: approach.z - 1,
                ..approach
            },
            blocker,
            compile::dust(),
        );

        assert_eq!(
            sink_approach_blocking_route(
                &reservations,
                current,
                source_at,
                terminal,
                Facing::North,
            ),
            Some(blocker),
        );
    }

    #[test]
    fn no_local_sink_finds_the_nearest_foreign_route_outside_its_strict_halo() {
        let current = RouteId(3);
        let blocker = RouteId(2);
        let terminal = Anchor { x: 59, y: 1, z: 38 };
        let approach = step(terminal, Facing::West);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve_conductor(
            Anchor {
                z: approach.z - 2,
                ..approach
            },
            blocker,
            compile::dust(),
        );

        assert_eq!(
            sink_approach_blocking_route(
                &reservations,
                current,
                Anchor { x: 18, y: 1, z: 39 },
                terminal,
                Facing::West,
            ),
            None,
        );
        assert_eq!(
            nearby_sink_blocking_route(&reservations, current, terminal, Facing::West),
            Some(blocker),
        );
    }

    #[test]
    fn reserved_source_escape_covers_the_strict_exit_clearance() {
        let source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(5),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let source_at = Anchor { x: 24, y: 1, z: 35 };
        let exit = step(source_at, Facing::East);
        let observed_blocker_at = step(exit, Facing::North);
        let runway = step(exit, Facing::East);
        let mouth = step(runway, Facing::East);
        let runway_side_halo = step(runway, Facing::North);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            source_at,
            PhysicalReservationOwner::Endpoint(source),
            PhysicalReservationKind::KeepOut,
        );

        let guarded =
            reserve_source_escape_footprint(&mut reservations, source, source_at, Facing::East);

        assert!(guarded.contains(&exit));
        assert!(guarded.contains(&observed_blocker_at));
        assert!(guarded.contains(&runway));
        assert!(guarded.contains(&mouth));
        assert!(guarded.contains(&runway_side_halo));
        let (core, _) = source_escape_footprint(source_at, Facing::East);
        assert!(core.contains(&mouth));
        assert_eq!(
            reservations
                .get(&observed_blocker_at)
                .map(|claim| claim.owner),
            Some(PhysicalReservationOwner::Endpoint(source)),
        );
        assert!(!guarded.contains(&step(source_at, Facing::West)));
    }

    #[test]
    fn every_source_core_is_reserved_before_any_other_source_halo() {
        let first = PhysicalEndpointId::PrimaryInput(PortId(0));
        let second = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(2),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let sources = BTreeMap::from([
            (
                first,
                SourceGeometry {
                    route_anchor: Anchor { x: 0, y: 1, z: 0 },
                    allowed_exit: Facing::East,
                },
            ),
            (
                second,
                SourceGeometry {
                    route_anchor: Anchor { x: 1, y: 1, z: 1 },
                    allowed_exit: Facing::East,
                },
            ),
        ]);
        let scheduled = BTreeSet::from([first, second]);
        let second_exit = Anchor { x: 2, y: 1, z: 1 };
        let mut reservations = PhysicalReservations::new();

        let guarded =
            reserve_scheduled_source_escapes(&mut reservations, &sources, &scheduled).unwrap();

        assert!(guarded[&second].contains(&second_exit));
        assert!(!guarded[&first].contains(&second_exit));
        assert_eq!(
            reservations.get(&second_exit).map(|claim| claim.owner),
            Some(PhysicalReservationOwner::Endpoint(second)),
        );
    }

    #[test]
    fn reserved_sink_approach_covers_the_strict_goal_clearance() {
        let endpoint = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(7),
            input_index: 0,
        });
        let terminal = Anchor {
            x: 134,
            y: 1,
            z: 118,
        };
        let approach = step(terminal, Facing::West);
        let halo_blocker = Anchor {
            y: approach.y + 1,
            z: approach.z - 1,
            ..approach
        };
        let outer_cell = Anchor {
            z: approach.z - 2,
            ..approach
        };
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            terminal,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );

        let guarded =
            reserve_sink_approach_footprint(&mut reservations, endpoint, terminal, Facing::West);

        assert!(guarded.contains(&approach));
        assert!(guarded.contains(&halo_blocker));
        assert!(!guarded.contains(&outer_cell));
        assert_eq!(
            reservations.get(&halo_blocker).map(|claim| claim.owner),
            Some(PhysicalReservationOwner::Endpoint(endpoint)),
        );
        assert!(!guarded.contains(&terminal));
    }

    #[test]
    fn reserved_sink_outer_ring_leaves_an_elevated_crossing_open() {
        let endpoint = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(38),
            input_index: 2,
        });
        let terminal = Anchor { x: 62, y: 1, z: 31 };
        let approach = step(terminal, Facing::West);
        let outer_same_level = Anchor {
            z: approach.z - 2,
            ..approach
        };
        let elevated_crossing = Anchor {
            y: approach.y + 1,
            ..outer_same_level
        };
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            terminal,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );

        let guarded =
            reserve_sink_approach_footprint(&mut reservations, endpoint, terminal, Facing::West);

        assert!(!guarded.contains(&outer_same_level));
        assert!(!guarded.contains(&elevated_crossing));
        assert!(reservations.get(&elevated_crossing).is_none());
    }

    #[test]
    fn exclusive_sink_track_reserves_the_outer_elevated_crossing() {
        let endpoint = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(38),
            input_index: 2,
        });
        let terminal = Anchor { x: 62, y: 1, z: 31 };
        let approach = step(terminal, Facing::West);
        let outer_same_level = Anchor {
            z: approach.z - 2,
            ..approach
        };
        let elevated_crossing = Anchor {
            y: approach.y + 1,
            ..outer_same_level
        };
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            terminal,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );

        let guarded = reserve_exclusive_sink_approach_footprint(
            &mut reservations,
            endpoint,
            terminal,
            Facing::West,
        );

        assert!(guarded.contains(&outer_same_level));
        assert!(guarded.contains(&elevated_crossing));
        assert_eq!(
            reservations
                .get(&elevated_crossing)
                .map(|claim| claim.owner),
            Some(PhysicalReservationOwner::Endpoint(endpoint)),
        );
    }

    #[test]
    fn ordinary_scheduled_sink_reserves_its_runway_without_the_exclusive_outer_ring() {
        let endpoint = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(41),
            input_index: 0,
        });
        let terminal = Anchor { x: 70, y: 1, z: 40 };
        let approach = step(terminal, Facing::West);
        let runway = step(approach, Facing::West);
        let side_halo = Anchor {
            z: approach.z - 1,
            ..approach
        };
        let outer_elevated = Anchor {
            x: approach.x,
            y: approach.y + 1,
            z: approach.z - 2,
        };
        let mut reservations = PhysicalReservations::new();

        let guarded = reserve_scheduled_sink_approach(
            &mut reservations,
            endpoint,
            terminal,
            Facing::West,
            false,
        );

        assert!(guarded.contains(&approach));
        assert!(guarded.contains(&runway));
        assert!(!guarded.contains(&side_halo));
        assert!(!guarded.contains(&outer_elevated));
    }

    #[test]
    fn boundary_terminal_reserves_every_face_except_its_named_handover() {
        let endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
        let body = Anchor { x: 16, y: 1, z: 16 };
        let handover = step(body, Facing::East);
        let mut reservations = PhysicalReservations::new();
        let protected = BTreeSet::from([handover]);

        reserve_boundary_terminal_sides(&mut reservations, endpoint, body, handover, &protected);

        assert!(reservations.get(&handover).is_none());
        for direction in [
            Facing::North,
            Facing::South,
            Facing::West,
            Facing::Up,
            Facing::Down,
        ] {
            assert_eq!(
                reservations
                    .get(&step(body, direction))
                    .map(|claim| (claim.owner, &claim.kind,)),
                Some((
                    PhysicalReservationOwner::Endpoint(endpoint),
                    &PhysicalReservationKind::KeepOut,
                )),
            );
        }
    }

    #[test]
    fn ring_closure_failure_has_physical_category_without_cap_work() {
        let route = RouteId(4);
        let source = PhysicalEndpointId::Junction(InstanceId(6));
        let source_at = Anchor { x: 3, y: 1, z: 5 };
        let sinks = one_typed_sink(route);
        let plan_fingerprint = canonical_fingerprint(b"typed-ring-plan");
        let failure = RouterFailure::RingClosure {
            route,
            source,
            sink: RoutedSinkId { route, ordinal: 0 },
            repeater: Anchor { x: 7, y: 1, z: 5 },
            charged: vec![Anchor { x: 8, y: 1, z: 5 }],
        };

        let evidence = seed_routing_failure(
            2,
            route,
            source,
            source_at,
            Facing::East,
            None,
            false,
            &sinks,
            &failure,
            &plan_fingerprint,
        );

        assert_eq!(
            evidence.category,
            crate::compile::routing::RouterRefusalCategory::PhysicalInvariant
        );
        assert_eq!(evidence.limit, None);
        assert_eq!(evidence.work_used, None);
        assert_eq!(evidence.sink_at, Anchor { x: 9, y: 2, z: 7 });
        assert_eq!(evidence.plan_fingerprint, plan_fingerprint);
    }

    struct LiteralSeedPlacer {
        calls: Cell<u32>,
    }

    impl SeedPlacer for LiteralSeedPlacer {
        fn plan(
            &self,
            _request: SeedPlacementRequest<'_>,
        ) -> Result<SeedPlacementPlan, SeedPlacementError> {
            self.calls.set(self.calls.get() + 1);
            Ok(SeedPlacementPlan {
                frame: crate::compile::fragment_synth::placement::PlacementFrame {
                    forward: Facing::East,
                    lateral: Facing::South,
                    origin: Anchor { x: 0, y: 1, z: 0 },
                },
                signal_tracks: BTreeMap::new(),
                instances: BTreeMap::from([(
                    InstanceId(0),
                    PreferredInstancePose {
                        preferred_origin: Anchor { x: 91, y: 7, z: 83 },
                        facing: CellFacing::NORTH,
                    },
                )]),
                automatic_inputs: BTreeMap::from([(PortId(0), Anchor { x: 71, y: 7, z: 83 })]),
                automatic_outputs: BTreeMap::from([(
                    PortId(0),
                    Anchor {
                        x: 111,
                        y: 7,
                        z: 83,
                    },
                )]),
                owner_offsets: BTreeMap::new(),
                fingerprint: canonical_fingerprint(b"literal-seed-plan"),
            })
        }
    }

    #[test]
    fn injected_seed_plan_controls_materialised_pose_and_automatic_boundaries_once() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let placer = LiteralSeedPlacer {
            calls: Cell::new(0),
        };

        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &placer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();

        let candidate = certified.candidate();
        assert_eq!(placer.calls.get(), 1);
        let views = candidate.compatibility_views(&netlist).unwrap();
        assert_eq!(
            views.input_positions,
            BTreeMap::from([("a".to_string(), (71, 7, 83))])
        );
        assert_eq!(
            views.output_positions,
            BTreeMap::from([("y".to_string(), (111, 7, 83))])
        );
        let primitive = PrimitiveId {
            instance: InstanceId(0),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        };
        assert_eq!(candidate.placements[&primitive].facing, CellFacing::NORTH);
        assert_eq!(
            candidate.placements[&primitive].anchor,
            Anchor { x: 91, y: 7, z: 83 }
        );
    }

    fn build(netlist: &Netlist) -> Result<CertifiedCandidate, SeedError> {
        build_with_pins(netlist, None)
    }

    fn build_with_pins(
        netlist: &Netlist,
        pins: Option<&PortPlacements>,
    ) -> Result<CertifiedCandidate, SeedError> {
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        compile_sparse_seed_with_services(
            SeedInput {
                lowered: netlist,
                source_provenance: None,
                pins,
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
    }

    #[test]
    fn one_instance_variant_rebuilds_and_certifies_the_requested_facing_and_offset() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let baseline = build(&netlist).unwrap();
        let variant = compile_sparse_seed_variant_with_services(
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
            &SeedVariant {
                placements: BTreeMap::from([(
                    InstanceId(0),
                    InstancePlacementOverride {
                        facing: CellFacing::NORTH,
                        dx: 4,
                        dz: -3,
                    },
                )]),
                ..SeedVariant::default()
            },
        )
        .unwrap();
        let primitive = PrimitiveId {
            instance: InstanceId(0),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        };
        let original = &baseline.candidate().placements[&primitive];
        let changed = &variant.candidate().placements[&primitive];

        assert_eq!(changed.facing, CellFacing::NORTH);
        assert_eq!(changed.anchor.x, original.anchor.x + 4);
        assert_eq!(changed.anchor.z, original.anchor.z - 3);
        assert_ne!(
            variant.metrics().candidate_fingerprint,
            baseline.metrics().candidate_fingerprint
        );
    }

    #[test]
    fn a_combinational_duplicate_is_independently_placed_routed_and_certified() {
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
        let duplicate_sink =
            crate::compile::fragment_synth::instance_graph::PhysicalSink::InstanceInput {
                instance: InstanceId(2),
                input_index: 0,
            };
        let certified = compile_sparse_seed_variant_with_services(
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
            &SeedVariant {
                duplicates: vec![
                    crate::compile::fragment_synth::instance_graph::DuplicateRequest {
                        canonical: InstanceId(0),
                        ordinal: 1,
                        sinks: BTreeSet::from([duplicate_sink]),
                    },
                ],
                ..SeedVariant::default()
            },
        )
        .unwrap();

        assert_eq!(certified.candidate().instances.instances.len(), 4);
        assert!(certified
            .candidate()
            .instances
            .instances
            .iter()
            .any(|instance| {
                instance.role
                    == crate::compile::fragment_synth::instance_graph::InstanceRole::Duplicate {
                        ordinal: 1,
                    }
            }));
        certified.candidate().validate_shape().unwrap();
    }

    #[derive(Default)]
    struct CountingRouter {
        calls: Cell<u32>,
    }

    impl PhysicalRouter for CountingRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.calls.set(self.calls.get() + 1);
            DurablePhysicalRouter.route(request)
        }
    }

    #[derive(Default)]
    struct CountingEmitter {
        calls: Cell<u32>,
    }

    impl SeedEmitter for CountingEmitter {
        fn emit(
            &self,
            candidate: &dyn PhysicalCandidateView,
            size: (i32, i32, i32),
        ) -> Result<EmittedWorld, EmissionError> {
            self.calls.set(self.calls.get() + 1);
            DurableSeedEmitter.emit(candidate, size)
        }
    }

    #[derive(Default)]
    struct CountingVerifier {
        calls: Cell<u32>,
    }

    impl SeedVerifier for CountingVerifier {
        fn verify(
            &self,
            candidate: &ExpandedPhysicalCandidate,
            emitted: &EmittedWorld,
        ) -> Result<(), ExpandedPhysicalError> {
            self.calls.set(self.calls.get() + 1);
            DurableSeedVerifier.verify(candidate, emitted)
        }
    }

    #[derive(Default)]
    struct CountingCertifier {
        calls: Cell<u32>,
    }

    impl ExpandedCandidateCertifier for CountingCertifier {
        fn certify(
            &self,
            candidate: ExpandedPhysicalCandidate,
            lowered: &Netlist,
            library: &Library,
            config: &CertificationConfig,
        ) -> Result<CertifiedCandidate, CandidateCertificationError> {
            self.calls.set(self.calls.get() + 1);
            CompleteCandidateCertifier.certify(candidate, lowered, library, config)
        }
    }

    #[derive(Default)]
    struct CountingLegacyOracle {
        calls: Cell<u32>,
        compile_legacy_calls: Cell<u32>,
        compile_planned_calls: Cell<u32>,
        compile_grown_calls: Cell<u32>,
        seed_from_legacy_calls: Cell<u32>,
        plan_from_netlist_calls: Cell<u32>,
    }

    impl LegacyOracle for CountingLegacyOracle {
        fn compile_legacy(
            &self,
            netlist: &Netlist,
        ) -> Result<crate::compile::CompiledCircuit, crate::compile::CompileError> {
            self.calls.set(self.calls.get() + 1);
            self.compile_legacy_calls
                .set(self.compile_legacy_calls.get() + 1);
            crate::compile::compile_legacy(netlist)
        }
    }

    fn gate(
        name: &str,
        inputs: &[&str],
        output: &str,
        kind: crate::compile::topology::GateKind,
    ) -> crate::compile::Gate {
        crate::compile::Gate {
            name: name.to_string(),
            inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
            output: output.to_string(),
            kind,
        }
    }

    #[test]
    fn not_seed_is_fully_certified_and_deterministic() {
        let netlist = not_netlist();
        let first = build(&netlist).unwrap();
        let second = build(&netlist).unwrap();

        assert_eq!(
            first.metrics().candidate_fingerprint,
            second.metrics().candidate_fingerprint
        );
        assert_eq!(
            first.metrics().emitted_world_fingerprint,
            second.metrics().emitted_world_fingerprint
        );
    }

    #[test]
    fn production_seed_uses_every_durable_service_and_no_legacy_entrypoint() {
        let (netlist, _) = build_and4_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
        let emitter = CountingEmitter::default();
        let verifier = CountingVerifier::default();
        let certifier = CountingCertifier::default();
        let legacy = CountingLegacyOracle::default();

        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &router,
                emitter: &emitter,
                verifier: &verifier,
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap();

        assert_eq!(
            certified.candidate().instances.instances.len(),
            netlist.gates.len()
        );
        assert!(router.calls.get() > 0);
        assert!(emitter.calls.get() > 0);
        assert!(verifier.calls.get() > 0);
        assert!(certifier.calls.get() > 0);
        assert_eq!(legacy.calls.get(), 0);
        assert_eq!(legacy.compile_legacy_calls.get(), 0);
        assert_eq!(legacy.compile_planned_calls.get(), 0);
        assert_eq!(legacy.compile_grown_calls.get(), 0);
        assert_eq!(legacy.seed_from_legacy_calls.get(), 0);
        assert_eq!(legacy.plan_from_netlist_calls.get(), 0);

        LegacyCandidateAdapter::adapt_from_oracle(&netlist, &legacy).unwrap();
        assert_eq!(legacy.calls.get(), 1);
        assert_eq!(legacy.compile_legacy_calls.get(), 1);
        assert_eq!(legacy.compile_planned_calls.get(), 0);
        assert_eq!(legacy.compile_grown_calls.get(), 0);
        assert_eq!(legacy.seed_from_legacy_calls.get(), 0);
        assert_eq!(legacy.plan_from_netlist_calls.get(), 0);
    }

    #[test]
    fn and4_seed_is_fully_certified_and_deterministic() {
        let (netlist, _) = build_and4_netlist();
        let first = build(&netlist).unwrap();
        let second = build(&netlist).unwrap();

        assert_eq!(
            first.candidate().instances.instances.len(),
            netlist.gates.len()
        );
        assert_eq!(
            first.metrics().candidate_fingerprint,
            second.metrics().candidate_fingerprint
        );
        assert_eq!(
            first.metrics().emitted_world_fingerprint,
            second.metrics().emitted_world_fingerprint
        );
    }

    #[test]
    fn segment_a_seed_routes_and_certifies_without_closing_a_refresh_ring() {
        let evaluator = legacy_benchmark_evaluator().unwrap();
        let fixture = evaluator.fixture("segment_a").unwrap();

        build_with_pins(fixture.lowered_netlist(), Some(fixture.placements()))
            .expect("segment_a has legal routing space and must certify at the seed budget");
    }

    #[test]
    fn pinned_and4_preserves_caller_cells_and_handover_directions() {
        let (netlist, _) = build_and4_netlist();
        let input_at = Anchor { x: 21, y: 1, z: 62 };
        let output_at = Anchor { x: 53, y: 1, z: 10 };
        let output_name = netlist.outputs[0].clone();
        let mut pins = PortPlacements::default();
        pins.pin("a", input_at, Facing::North)
            .pin(output_name.clone(), output_at, Facing::North);

        let certified = build_with_pins(&netlist, Some(&pins)).unwrap();
        let repeated = build_with_pins(&netlist, Some(&pins)).unwrap();
        let candidate = certified.candidate();
        assert_eq!(
            certified.metrics().candidate_fingerprint,
            repeated.metrics().candidate_fingerprint
        );
        assert_eq!(
            certified.metrics().emitted_world_fingerprint,
            repeated.metrics().emitted_world_fingerprint
        );
        assert_eq!(candidate.pins.get("a").unwrap().at, input_at);
        assert_eq!(candidate.pins.get(&output_name).unwrap().at, output_at);
        assert_eq!(
            candidate.observations[&ObservationId::PrimaryInput(PortId(0))]
                .state
                .kind,
            BlockKind::Air
        );
        assert_eq!(
            candidate.observations[&ObservationId::DeclaredOutput(PortId(0))]
                .state
                .kind,
            BlockKind::Air
        );
    }

    #[test]
    fn fanout_buf_and_merge_seed_shapes_are_fully_certified() {
        use crate::compile::topology::GateKind;

        let fixtures = [
            Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into(), "z".into()],
                gates: vec![
                    gate("source", &["a"], "n", GateKind::Nor(1)),
                    gate("left", &["n"], "y", GateKind::Nor(1)),
                    gate("right", &["n"], "z", GateKind::Nor(1)),
                ],
            },
            Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("buf", &["a"], "y", GateKind::Buf)],
            },
            Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("bare", &["a", "b"], "y", GateKind::Or(2))],
            },
            Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into(), "z".into()],
                gates: vec![
                    gate("mixed", &["a", "b"], "y", GateKind::Or(2)),
                    gate("fanout", &["a"], "z", GateKind::Nor(1)),
                ],
            },
            Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into(), "u".into(), "v".into()],
                gates: vec![
                    gate("isolated", &["a", "b"], "y", GateKind::Or(2)),
                    gate("left", &["a"], "u", GateKind::Nor(1)),
                    gate("right", &["b"], "v", GateKind::Nor(1)),
                ],
            },
        ];

        for (fixture_index, fixture) in fixtures.into_iter().enumerate() {
            let certified = build(&fixture)
                .unwrap_or_else(|error| panic!("fixture {fixture_index} failed: {error:?}"));
            let repeated = build(&fixture)
                .unwrap_or_else(|error| panic!("fixture {fixture_index} repeat failed: {error:?}"));
            assert_eq!(
                certified.candidate().instances.instances.len(),
                fixture.gates.len()
            );
            assert_eq!(
                certified.metrics().candidate_fingerprint,
                repeated.metrics().candidate_fingerprint
            );
            assert_eq!(
                certified.metrics().emitted_world_fingerprint,
                repeated.metrics().emitted_world_fingerprint
            );
        }
    }

    #[test]
    fn stateful_topology_is_rejected_before_any_physical_service() {
        let netlist = Netlist {
            inputs: vec!["d".into(), "clk".into()],
            outputs: vec!["q".into()],
            gates: vec![gate(
                "ff",
                &["d", "clk"],
                "q",
                crate::compile::topology::GateKind::DffPosedge,
            )],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
        let emitter = CountingEmitter::default();
        let verifier = CountingVerifier::default();
        let certifier = CountingCertifier::default();

        let error = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &router,
                emitter: &emitter,
                verifier: &verifier,
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SeedError::InstanceGraph(SynthesisError::UnsupportedStatefulTopology { .. })
        ));
        assert_eq!(router.calls.get(), 0);
        assert_eq!(emitter.calls.get(), 0);
        assert_eq!(verifier.calls.get(), 0);
        assert_eq!(certifier.calls.get(), 0);
    }

    #[test]
    fn invalid_pin_is_rejected_before_any_physical_service() {
        let netlist = not_netlist();
        let mut pins = PortPlacements::default();
        pins.pin("a", Anchor { x: 4, y: 1, z: 4 }, Facing::Up);
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
        let emitter = CountingEmitter::default();
        let verifier = CountingVerifier::default();
        let certifier = CountingCertifier::default();

        let error = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: Some(&pins),
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &router,
                emitter: &emitter,
                verifier: &verifier,
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap_err();

        assert!(matches!(error, SeedError::InvalidPins(_)));
        assert_eq!(router.calls.get(), 0);
        assert_eq!(emitter.calls.get(), 0);
        assert_eq!(verifier.calls.get(), 0);
        assert_eq!(certifier.calls.get(), 0);
    }

    #[test]
    fn zero_seed_radius_is_a_named_bounded_refusal() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let mut config = SearchConfig::checked_defaults();
        config.max_seed_shell_radius = 0;
        let error = compile_sparse_seed_with_services(
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
        .unwrap_err();

        assert!(matches!(
            error,
            SeedError::PlacementExhausted {
                instance: InstanceId(0),
                radius: 0,
                ..
            }
        ));
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedRouteRequest {
        id: RouteId,
        source: PhysicalEndpointId,
        source_anchor: Anchor,
        sinks: Vec<(PhysicalEndpointId, Anchor)>,
        limits: crate::compile::routing::RouterLimits,
    }

    /// Forwards every request to the production router unchanged while keeping
    /// the evidence: request identity, typed endpoints in caller order, limits,
    /// and a snapshot of the first request's reservations.
    #[derive(Default)]
    struct RecordingRouter {
        requests: RefCell<Vec<RecordedRouteRequest>>,
        first_reservations: RefCell<Option<PhysicalReservations>>,
    }

    impl PhysicalRouter for RecordingRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.first_reservations
                .borrow_mut()
                .get_or_insert_with(|| request.reservations.clone());
            self.requests.borrow_mut().push(RecordedRouteRequest {
                id: request.id,
                source: request.source.id,
                source_anchor: request.source.anchor,
                sinks: request
                    .sinks
                    .as_slice()
                    .iter()
                    .map(|sink| (sink.endpoint, sink.anchor))
                    .collect(),
                limits: request.limits,
            });
            DurablePhysicalRouter.route(request)
        }
    }

    #[test]
    fn and4_schedule_reaches_the_router_in_order_over_pre_reserved_terminals() {
        let (netlist, _) = build_and4_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = RecordingRouter::default();

        compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &router,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();

        let requests = router.requests.borrow();
        let observed = requests
            .iter()
            .map(|request| {
                (
                    request.source,
                    request
                        .sinks
                        .iter()
                        .map(|(endpoint, _)| *endpoint)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        fn primitive_output(instance: u32) -> PhysicalEndpointId {
            PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: InstanceId(instance),
                node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
            })
        }
        fn external_landing(instance: u32, input_index: u16) -> PhysicalEndpointId {
            PhysicalEndpointId::Landing(ConnectionId::External {
                instance: InstanceId(instance),
                input_index,
            })
        }
        let expected = vec![
            (
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                vec![external_landing(0, 0)],
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(1)),
                vec![external_landing(1, 0)],
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(2)),
                vec![external_landing(2, 0)],
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(3)),
                vec![external_landing(5, 0)],
            ),
            (primitive_output(0), vec![external_landing(3, 0)]),
            (primitive_output(1), vec![external_landing(3, 1)]),
            (primitive_output(2), vec![external_landing(3, 2)]),
            (primitive_output(3), vec![external_landing(4, 0)]),
            (primitive_output(4), vec![external_landing(6, 0)]),
            (
                primitive_output(6),
                vec![PhysicalEndpointId::DeclaredOutput(PortId(0))],
            ),
            (primitive_output(5), vec![external_landing(6, 1)]),
        ];
        assert_eq!(observed, expected);

        for (scheduled_index, request) in requests.iter().enumerate() {
            assert_eq!(
                request.id,
                RouteId(u32::try_from(scheduled_index).unwrap()),
                "scheduled request {scheduled_index} carries a non-sequential RouteId"
            );
            assert_eq!(
                request.limits, config.router_limits,
                "scheduled request {scheduled_index} altered the configured router limits"
            );
        }

        let first_reservations = router.first_reservations.borrow();
        let first_reservations = first_reservations
            .as_ref()
            .expect("the certifying build must issue at least one route request");
        for request in requests.iter() {
            assert!(
                first_reservations.get(&request.source_anchor).is_some(),
                "source terminal {:?} of {:?} was not reserved before routing began",
                request.source_anchor,
                request.source,
            );
            for (endpoint, anchor) in &request.sinks {
                assert!(
                    first_reservations.get(anchor).is_some(),
                    "sink terminal {anchor:?} of {endpoint:?} was not reserved before routing began",
                );
            }
        }
    }

    #[test]
    fn schedule_repairs_change_only_the_named_sink_or_route_precedence() {
        use crate::compile::topology::GateKind;

        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into(), "z".into()],
            gates: vec![
                gate("source", &["a"], "n", GateKind::Nor(1)),
                gate("left", &["n"], "y", GateKind::Nor(1)),
                gate("right", &["n"], "z", GateKind::Nor(1)),
            ],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let schedule_for = |repairs: &[LayoutRepair]| {
            let router = RecordingRouter::default();
            let instances =
                InstanceGraph::with_variants(&netlist, &library, &BTreeMap::new(), &[]).unwrap();
            let _ = SparseSeedBuilder::build_attempt(
                SeedInput {
                    lowered: &netlist,
                    source_provenance: None,
                    pins: None,
                },
                SeedServices {
                    library: &library,
                    placer: &TopologyAwareSeedPlacer,
                    router: &router,
                    emitter: &DurableSeedEmitter,
                    verifier: &DurableSeedVerifier,
                    certifier: &CompleteCandidateCertifier,
                    search_config: &config,
                },
                &SeedVariant::default(),
                instances,
                repairs,
            );
            let requests = router.requests.borrow();
            requests
                .iter()
                .map(|request| {
                    (
                        request.source,
                        request
                            .sinks
                            .iter()
                            .map(|(endpoint, _)| *endpoint)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let fanout_source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(0),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });
        let left_landing = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(1),
            input_index: 0,
        });
        let right_landing = PhysicalEndpointId::Landing(ConnectionId::External {
            instance: InstanceId(2),
            input_index: 0,
        });

        let baseline = schedule_for(&[]);
        let fanout_index = baseline
            .iter()
            .position(|(source, _)| *source == fanout_source)
            .expect("baseline schedule must route the grouped fanout source");
        assert_eq!(
            baseline[fanout_index].1,
            vec![left_landing, right_landing],
            "baseline fanout tree must order the named sink second",
        );

        let promoted = schedule_for(&[LayoutRepair::EarlyTreeSinkAndEscape {
            source: fanout_source,
            sink: right_landing,
        }]);
        assert_eq!(promoted.len(), baseline.len());
        assert_eq!(
            promoted[fanout_index].1,
            vec![right_landing, left_landing],
            "naming the non-first sink must move exactly it to ordinal 0",
        );
        for (index, (route, baseline_route)) in promoted.iter().zip(&baseline).enumerate() {
            assert_eq!(
                route.0, baseline_route.0,
                "route-level schedule order changed at scheduled index {index}",
            );
            if index != fanout_index {
                assert_eq!(
                    route.1, baseline_route.1,
                    "unrelated route at scheduled index {index} was reordered",
                );
            }
        }

        let precedence_source = baseline
            .last()
            .expect("test schedule must contain a later route")
            .0;
        let precedence_blocker = baseline
            .first()
            .expect("test schedule must contain an earlier route")
            .0;
        assert_ne!(precedence_source, precedence_blocker);
        let precedence = schedule_for(&[LayoutRepair::RouteBefore {
            source: precedence_source,
            blocker: precedence_blocker,
        }]);
        let source_index = precedence
            .iter()
            .position(|(source, _)| *source == precedence_source)
            .expect("repaired source must stay scheduled");
        let blocker_index = precedence
            .iter()
            .position(|(source, _)| *source == precedence_blocker)
            .expect("blocker source must stay scheduled");
        assert!(
            source_index < blocker_index,
            "RouteBefore must move its source ahead of the named blocker",
        );
    }
}
