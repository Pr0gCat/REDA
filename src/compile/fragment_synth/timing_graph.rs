//! Primitive-level max-plus timing over a certified expanded candidate.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::compile::fragment_synth::identity::{TimingArcId, TimingNodeId};
use crate::compile::fragment_synth::candidate::{
    ExpandedPhysicalCandidate, RealisedRouteBranch, RealisedRouteTree,
};
use crate::compile::fragment_synth::identity::{
    PhysicalEndpointId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::topology::{ConnectionTarget, ContributorSpec, OutputSpec};
use crate::compile::fragment_synth::verify::StructuralCertificate;
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::topology::Primitive;
use crate::redstone::simulator::component::{
    repeater_delay_game_ticks, COMPARATOR_DELAY_GAME_TICKS, TORCH_DELAY_GAME_TICKS,
};
use crate::redstone::world::block::{BlockKind, BlockState};

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ExactDelay(pub u64);

impl ExactDelay {
    fn checked_add(self, other: Self) -> Result<Self, TimingGraphError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(TimingGraphError::DelayOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TimingArcKind {
    InputBinding,
    Route { route: RouteId, sink: RoutedSinkId },
    Primitive { primitive: PrimitiveId },
    Junction,
    InstanceOutput,
    OutputBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimingArc {
    pub id: TimingArcId,
    pub from: TimingNodeId,
    pub to: TimingNodeId,
    pub kind: TimingArcKind,
    pub delay: ExactDelay,
}

impl TimingArc {
    pub const fn new(
        id: TimingArcId,
        from: TimingNodeId,
        to: TimingNodeId,
        kind: TimingArcKind,
        delay: ExactDelay,
    ) -> Self {
        Self { id, from, to, kind, delay }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TimingGraphError {
    #[error("structural certificate is for candidate {certified:?}, not {actual:?}")]
    CertificateMismatch {
        certified: Fingerprint,
        actual: Fingerprint,
    },
    #[error("timing arc {arc:?} is duplicated")]
    DuplicateArc { arc: TimingArcId },
    #[error("timing arc {arc:?} references missing node {node:?}")]
    MissingNode { arc: TimingArcId, node: TimingNodeId },
    #[error("realised timing graph contains a cycle")]
    Cycle,
    #[error("timing identity width exceeded")]
    IdentityOverflow,
    #[error("timing derivation cannot resolve {what} for {node:?}")]
    Unresolved {
        what: &'static str,
        node: TimingNodeId,
    },
    #[error("delayed timing node {node:?} has no typed observation")]
    DelayedWithoutObservation { node: TimingNodeId },
    #[error("exact timing delay overflowed u64 game ticks")]
    DelayOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealisedTimingGraph {
    pub nodes: BTreeSet<TimingNodeId>,
    pub arcs: BTreeMap<TimingArcId, TimingArc>,
}

impl RealisedTimingGraph {
    pub fn fingerprint(&self) -> Fingerprint {
        canonical_fingerprint(
            &serde_json::to_vec(self).expect("a realised timing graph must serialize"),
        )
    }

    /// Derive timing only from a structurally certified candidate. The
    /// certificate fingerprint prevents callers from certifying one value
    /// and then timing a mutated clone.
    pub fn derive(
        candidate: &ExpandedPhysicalCandidate,
        certificate: &StructuralCertificate,
    ) -> Result<Self, TimingGraphError> {
        let actual = candidate.fingerprint();
        if actual != certificate.candidate_fingerprint {
            return Err(TimingGraphError::CertificateMismatch {
                certified: certificate.candidate_fingerprint.clone(),
                actual,
            });
        }

        let mut builder = TimingGraphBuilder::default();

        for &port in &candidate.instances.primary_inputs {
            let boundary = TimingNodeId::InputBoundary(port);
            let input = TimingNodeId::PrimaryInput(port);
            builder.node(boundary);
            builder.node(input);
            let delay = candidate
                .boundaries
                .get(&PhysicalEndpointId::PrimaryInput(port))
                .and_then(|placement| placement.delayed)
                .map(|delayed| {
                    require_observation(candidate, input)?;
                    let state = candidate
                        .boundaries
                        .get(&PhysicalEndpointId::PrimaryInput(port))
                        .and_then(|placement| {
                            placement.blocks.iter().find(|block| block.at == delayed.at)
                        })
                        .map(|block| &block.state)
                        .ok_or(TimingGraphError::Unresolved {
                            what: "input-binding delayed block",
                            node: input,
                        })?;
                    delay_of_state(state, input)
                })
                .transpose()?
                .unwrap_or_default();
            builder.arc(boundary, input, TimingArcKind::InputBinding, delay)?;
        }

        for instance in &candidate.instances.instances {
            let instance_output = TimingNodeId::InstanceOutput(instance.id);
            builder.node(instance_output);

            for specification in &instance.expanded.topology.primitives {
                builder.node(TimingNodeId::PrimitiveOutput(specification.id));
            }
            for connection in &instance.expanded.topology.connections {
                let landing = TimingNodeId::Landing(connection.id);
                builder.node(landing);
                if let ConnectionTarget::Primitive(primitive) = connection.target {
                    let output = TimingNodeId::PrimitiveOutput(primitive);
                    let specification = instance
                        .expanded
                        .topology
                        .primitives
                        .iter()
                        .find(|specification| specification.id == primitive)
                        .ok_or(TimingGraphError::Unresolved {
                            what: "primitive specification",
                            node: output,
                        })?;
                    let delay = primitive_delay(
                        candidate,
                        specification.id,
                        specification.primitive,
                    )?;
                    builder.arc(
                        landing,
                        output,
                        TimingArcKind::Primitive { primitive },
                        delay,
                    )?;
                }
            }

            match &instance.expanded.topology.output {
                OutputSpec::Primitive(primitive) => builder.arc(
                    TimingNodeId::PrimitiveOutput(*primitive),
                    instance_output,
                    TimingArcKind::InstanceOutput,
                    ExactDelay(0),
                )?,
                OutputSpec::Junction { contributors, .. } => {
                    let junction = TimingNodeId::JunctionOutput(instance.id);
                    builder.node(junction);
                    for contributor in contributors {
                        let from = match contributor {
                            ContributorSpec::Landing(connection) => {
                                TimingNodeId::Landing(*connection)
                            }
                            ContributorSpec::Primitive(primitive) => {
                                TimingNodeId::PrimitiveOutput(*primitive)
                            }
                        };
                        builder.arc(
                            from,
                            junction,
                            TimingArcKind::Junction,
                            ExactDelay(0),
                        )?;
                    }
                    builder.arc(
                        junction,
                        instance_output,
                        TimingArcKind::InstanceOutput,
                        ExactDelay(0),
                    )?;
                }
            }
        }

        for (&connection, binding) in &candidate.connections {
            let route = candidate
                .routes
                .get(&binding.route)
                .ok_or(TimingGraphError::Unresolved {
                    what: "connection route",
                    node: TimingNodeId::Landing(connection),
                })?;
            let branch = branch_for_sink(route, binding.sink, TimingNodeId::Landing(connection))?;
            builder.arc(
                endpoint_node(binding.source),
                TimingNodeId::Landing(connection),
                TimingArcKind::Route {
                    route: binding.route,
                    sink: binding.sink,
                },
                route_delay(route, branch)?,
            )?;
        }

        for &port in &candidate.instances.declared_outputs {
            builder.node(TimingNodeId::OutputLanding(port));
            builder.node(TimingNodeId::DeclaredOutput(port));
        }
        for route in candidate.routes.values() {
            for branch in &route.branches {
                let crate::compile::fragment_synth::candidate::RouteTarget::DeclaredOutput(port) =
                    branch.target
                else {
                    continue;
                };
                builder.arc(
                    endpoint_node(route.source),
                    TimingNodeId::OutputLanding(port),
                    TimingArcKind::Route {
                        route: route.id,
                        sink: branch.sink,
                    },
                    route_delay(route, branch)?,
                )?;
            }
        }
        for &port in &candidate.instances.declared_outputs {
            builder.arc(
                TimingNodeId::OutputLanding(port),
                TimingNodeId::DeclaredOutput(port),
                TimingArcKind::OutputBinding,
                ExactDelay(0),
            )?;
        }

        builder.finish()
    }

    pub fn new(
        nodes: impl IntoIterator<Item = TimingNodeId>,
        arcs: impl IntoIterator<Item = TimingArc>,
    ) -> Result<Self, TimingGraphError> {
        let nodes: BTreeSet<_> = nodes.into_iter().collect();
        let mut by_id = BTreeMap::new();
        for arc in arcs {
            for node in [arc.from, arc.to] {
                if !nodes.contains(&node) {
                    return Err(TimingGraphError::MissingNode { arc: arc.id, node });
                }
            }
            let id = arc.id;
            if by_id.insert(id, arc).is_some() {
                return Err(TimingGraphError::DuplicateArc { arc: id });
            }
        }
        let graph = Self { nodes, arcs: by_id };
        graph.topological_order()?;
        Ok(graph)
    }

    fn topological_order(&self) -> Result<Vec<TimingNodeId>, TimingGraphError> {
        let mut indegree: BTreeMap<_, usize> =
            self.nodes.iter().copied().map(|node| (node, 0)).collect();
        let mut outgoing = BTreeMap::<TimingNodeId, Vec<TimingArc>>::new();
        for arc in self.arcs.values() {
            *indegree.get_mut(&arc.to).expect("arc endpoints were validated") += 1;
            outgoing.entry(arc.from).or_default().push(*arc);
        }
        for arcs in outgoing.values_mut() {
            arcs.sort_by_key(|arc| arc.id);
        }
        let mut ready: BTreeSet<_> = indegree
            .iter()
            .filter_map(|(&node, &degree)| (degree == 0).then_some(node))
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(node) = ready.pop_first() {
            order.push(node);
            for arc in outgoing.get(&node).into_iter().flatten() {
                let degree = indegree.get_mut(&arc.to).expect("arc endpoints were validated");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert(arc.to);
                }
            }
        }
        if order.len() != self.nodes.len() {
            return Err(TimingGraphError::Cycle);
        }
        Ok(order)
    }

    pub fn analyse(&self) -> Result<StaticTiming, TimingGraphError> {
        let order = self.topological_order()?;
        let mut incoming = BTreeMap::<TimingNodeId, Vec<TimingArc>>::new();
        let mut outgoing = BTreeMap::<TimingNodeId, Vec<TimingArc>>::new();
        for arc in self.arcs.values() {
            incoming.entry(arc.to).or_default().push(*arc);
            outgoing.entry(arc.from).or_default().push(*arc);
        }
        for arcs in incoming.values_mut() {
            arcs.sort_by_key(|arc| arc.id);
        }
        for arcs in outgoing.values_mut() {
            arcs.sort_by_key(|arc| arc.id);
        }

        let mut head = BTreeMap::<TimingNodeId, ExactDelay>::new();
        let mut predecessor = BTreeMap::<TimingNodeId, TimingArcId>::new();
        for &node in &order {
            let mut best = ExactDelay(0);
            let mut best_arc = None;
            for arc in incoming.get(&node).into_iter().flatten() {
                let arrival = head
                    .get(&arc.from)
                    .copied()
                    .unwrap_or_default()
                    .checked_add(arc.delay)?;
                if arrival > best
                    || (arrival == best && best_arc.is_none_or(|id| arc.id < id))
                {
                    best = arrival;
                    best_arc = Some(arc.id);
                }
            }
            head.insert(node, best);
            if let Some(arc) = best_arc {
                predecessor.insert(node, arc);
            }
        }
        let critical_delay = head.values().copied().max().unwrap_or_default();

        let mut tail = BTreeMap::<TimingNodeId, ExactDelay>::new();
        for &node in order.iter().rev() {
            let mut best = ExactDelay(0);
            for arc in outgoing.get(&node).into_iter().flatten() {
                let delay = arc
                    .delay
                    .checked_add(tail.get(&arc.to).copied().unwrap_or_default())?;
                best = best.max(delay);
            }
            tail.insert(node, best);
        }
        let mut slack = BTreeMap::new();
        for arc in self.arcs.values() {
            let used = head[&arc.from]
                .checked_add(arc.delay)?
                .checked_add(tail[&arc.to])?;
            slack.insert(arc.id, ExactDelay(critical_delay.0.saturating_sub(used.0)));
        }

        Ok(StaticTiming { head, tail, predecessor, slack, critical_delay })
    }
}

#[derive(Default)]
struct TimingGraphBuilder {
    nodes: BTreeSet<TimingNodeId>,
    arcs: Vec<TimingArc>,
}

impl TimingGraphBuilder {
    fn node(&mut self, node: TimingNodeId) {
        self.nodes.insert(node);
    }

    fn arc(
        &mut self,
        from: TimingNodeId,
        to: TimingNodeId,
        kind: TimingArcKind,
        delay: ExactDelay,
    ) -> Result<(), TimingGraphError> {
        self.node(from);
        self.node(to);
        let ordinal =
            u32::try_from(self.arcs.len()).map_err(|_| TimingGraphError::IdentityOverflow)?;
        self.arcs
            .push(TimingArc::new(TimingArcId(ordinal), from, to, kind, delay));
        Ok(())
    }

    fn finish(self) -> Result<RealisedTimingGraph, TimingGraphError> {
        RealisedTimingGraph::new(self.nodes, self.arcs)
    }
}

fn endpoint_node(endpoint: PhysicalEndpointId) -> TimingNodeId {
    match endpoint {
        PhysicalEndpointId::PrimaryInput(port) => TimingNodeId::PrimaryInput(port),
        PhysicalEndpointId::DeclaredOutput(port) => TimingNodeId::DeclaredOutput(port),
        PhysicalEndpointId::PrimitiveOutput(primitive) => {
            TimingNodeId::PrimitiveOutput(primitive)
        }
        PhysicalEndpointId::Landing(connection) => TimingNodeId::Landing(connection),
        PhysicalEndpointId::Junction(instance) => TimingNodeId::JunctionOutput(instance),
    }
}

fn require_observation(
    candidate: &ExpandedPhysicalCandidate,
    node: TimingNodeId,
) -> Result<(), TimingGraphError> {
    let observation = node
        .observation()
        .ok_or(TimingGraphError::DelayedWithoutObservation { node })?;
    if candidate.observations.contains_key(&observation) {
        Ok(())
    } else {
        Err(TimingGraphError::DelayedWithoutObservation { node })
    }
}

fn primitive_delay(
    candidate: &ExpandedPhysicalCandidate,
    primitive: PrimitiveId,
    kind: Primitive,
) -> Result<ExactDelay, TimingGraphError> {
    let node = TimingNodeId::PrimitiveOutput(primitive);
    let placement = candidate
        .placements
        .get(&primitive)
        .ok_or(TimingGraphError::Unresolved {
            what: "primitive placement",
            node,
        })?;
    let delay = match kind {
        Primitive::Torch => ExactDelay(TORCH_DELAY_GAME_TICKS),
        Primitive::Repeater => {
            require_observation(candidate, node)?;
            let state = placement
                .delayed
                .and_then(|delayed| {
                    placement.blocks.iter().find(|block| block.at == delayed.at)
                })
                .or_else(|| {
                    placement
                        .blocks
                        .iter()
                        .find(|block| block.state.kind == BlockKind::Repeater)
                })
                .map(|block| &block.state)
                .ok_or(TimingGraphError::Unresolved {
                    what: "topology repeater state",
                    node,
                })?;
            ExactDelay(repeater_delay_game_ticks(state))
        }
        Primitive::Comparator => ExactDelay(COMPARATOR_DELAY_GAME_TICKS),
        Primitive::Lever | Primitive::Lamp => ExactDelay(0),
    };
    if delay.0 > 0 {
        require_observation(candidate, node)?;
    }
    Ok(delay)
}

fn branch_for_sink(
    route: &RealisedRouteTree,
    sink: RoutedSinkId,
    node: TimingNodeId,
) -> Result<&RealisedRouteBranch, TimingGraphError> {
    route
        .branches
        .iter()
        .find(|branch| branch.sink == sink)
        .ok_or(TimingGraphError::Unresolved {
            what: "route branch",
            node,
        })
}

fn route_delay(
    route: &RealisedRouteTree,
    branch: &RealisedRouteBranch,
) -> Result<ExactDelay, TimingGraphError> {
    let cells: BTreeMap<_, _> = route
        .cells
        .iter()
        .map(|block| (block.at, &block.state))
        .collect();
    let mut delay = ExactDelay(0);
    let mut charged = BTreeSet::new();
    for &at in &branch.path {
        // A branch path includes its source mouth. That coordinate can be a
        // primitive-owned output and is intentionally absent from
        // `route.cells`; its delay belongs to the primitive arc. Only cells
        // explicitly owned by this route may contribute route delay.
        if charged.insert(at) {
            let Some(state) = cells.get(&at).copied() else {
                continue;
            };
            if state.kind == BlockKind::Repeater {
                delay = delay.checked_add(ExactDelay(repeater_delay_game_ticks(state)))?;
            }
        }
    }
    Ok(delay)
}

fn delay_of_state(
    state: &BlockState,
    node: TimingNodeId,
) -> Result<ExactDelay, TimingGraphError> {
    match state.kind {
        BlockKind::Torch | BlockKind::WallTorch => Ok(ExactDelay(TORCH_DELAY_GAME_TICKS)),
        BlockKind::Repeater => Ok(ExactDelay(repeater_delay_game_ticks(state))),
        BlockKind::Comparator => Ok(ExactDelay(COMPARATOR_DELAY_GAME_TICKS)),
        _ => Err(TimingGraphError::Unresolved {
            what: "delayed block semantics",
            node,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticTiming {
    pub head: BTreeMap<TimingNodeId, ExactDelay>,
    pub tail: BTreeMap<TimingNodeId, ExactDelay>,
    pub predecessor: BTreeMap<TimingNodeId, TimingArcId>,
    pub slack: BTreeMap<TimingArcId, ExactDelay>,
    pub critical_delay: ExactDelay,
}

#[cfg(test)]
mod tests {
    use super::{
        ExactDelay, RealisedTimingGraph, TimingArc, TimingArcId, TimingArcKind, TimingGraphError,
        TimingNodeId,
    };
    use crate::compile::fragment_synth::candidate::{
        BoundaryPlacement, ConnectionBinding, DelayedComponent, DelayedOwner,
        ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedRouteBranch,
        RealisedRouteTree, RouteTarget, TerminalRecord, VerifiedObservation,
    };
    use crate::compile::fragment_synth::identity::{
        ConnectionId, ImplementationKey, InputMask, InstanceId, ObservationId, ObservationSite,
        PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::instance_graph::{Instance, InstanceGraph, InstanceRole};
    use crate::compile::fragment_synth::topology::instantiate;
    use crate::compile::fragment_synth::verify::StructuralCertificate;
    use crate::compile::geometry::{Anchor, CellFacing};
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::planner::{PortPlacements, RouteTerminalKind};
    use crate::compile::topology::{GateKind, Library, Primitive};
    use crate::compile::{Gate, Netlist};
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

    fn primitive(instance: u32) -> TimingNodeId {
        TimingNodeId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(instance),
            node: TopologyNodeId(0),
        })
    }

    fn state(kind: BlockKind) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = match kind {
            BlockKind::RedstoneWire => "minecraft:redstone_wire",
            BlockKind::Repeater => "minecraft:repeater",
            BlockKind::WallTorch => "minecraft:redstone_wall_torch",
            BlockKind::Lever => "minecraft:lever",
            BlockKind::Lamp => "minecraft:redstone_lamp",
            _ => "minecraft:stone",
        }
        .to_string();
        if kind == BlockKind::Repeater {
            state.delay = 1;
            state.facing = Some(Facing::East);
        }
        state
    }

    fn observe(
        candidate: &mut ExpandedPhysicalCandidate,
        id: ObservationId,
        at: Anchor,
        state: BlockState,
    ) {
        candidate.observations.insert(
            id,
            VerifiedObservation {
                site: ObservationSite {
                    id,
                    at,
                    logical_owner: match id {
                        ObservationId::PrimitiveOutput(id) => Some(id.instance),
                        ObservationId::InstanceOutput(id) | ObservationId::JunctionOutput(id) => {
                            Some(id)
                        }
                        ObservationId::PrimaryInput(_) | ObservationId::DeclaredOutput(_) => None,
                    },
                    display_label: None,
                },
                state,
            },
        );
    }

    fn place_primitive(
        candidate: &mut ExpandedPhysicalCandidate,
        id: PrimitiveId,
        primitive: Primitive,
        at: Anchor,
    ) {
        let kind = match primitive {
            Primitive::Torch => BlockKind::WallTorch,
            Primitive::Repeater => BlockKind::Repeater,
            Primitive::Comparator => BlockKind::Comparator,
            Primitive::Lever => BlockKind::Lever,
            Primitive::Lamp => BlockKind::Lamp,
        };
        let block = PlacedBlock { at, state: state(kind) };
        candidate.placements.insert(
            id,
            PrimitivePlacement {
                id,
                variant: 0,
                facing: CellFacing::NORTH,
                anchor: at,
                delayed: matches!(primitive, Primitive::Repeater).then_some(DelayedComponent {
                    at,
                    owner: DelayedOwner::Primitive(id),
                }),
                blocks: vec![block.clone()],
            },
        );
        observe(candidate, ObservationId::PrimitiveOutput(id), at, block.state);
    }

    fn add_route(
        candidate: &mut ExpandedPhysicalCandidate,
        route: RouteId,
        source: PhysicalEndpointId,
        target: RouteTarget,
        at: Anchor,
        repeater: bool,
    ) -> RoutedSinkId {
        let sink = RoutedSinkId { route, ordinal: 0 };
        let block = PlacedBlock {
            at,
            state: state(if repeater {
                BlockKind::Repeater
            } else {
                BlockKind::RedstoneWire
            }),
        };
        candidate.routes.insert(
            route,
            RealisedRouteTree {
                id: route,
                source,
                cells: vec![block.clone()],
                floors: Vec::new(),
                branches: vec![RealisedRouteBranch {
                    sink,
                    target,
                    root: at,
                    path: vec![at],
                    terminal: TerminalRecord {
                        sink,
                        at,
                        state: block.state,
                        kind: if matches!(target, RouteTarget::DeclaredOutput(_)) && repeater {
                            RouteTerminalKind::OutputTerminalRepeater
                        } else if repeater {
                            RouteTerminalKind::RepeaterIntoSupport
                        } else {
                            RouteTerminalKind::DirectedDustIntoSupport
                        },
                        repeaters: u64::from(repeater),
                        delayed_owner: repeater.then_some(DelayedOwner::Route(route)),
                    },
                }],
            },
        );
        sink
    }

    fn certificate(candidate: &ExpandedPhysicalCandidate) -> StructuralCertificate {
        StructuralCertificate {
            candidate_fingerprint: candidate.fingerprint(),
            library_revision: canonical_fingerprint(b"timing-graph-test"),
            instance_count: candidate.instances.instances.len(),
        }
    }

    fn buf_candidate() -> ExpandedPhysicalCandidate {
        let library = Library::default_library();
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate {
                name: "buf".to_string(),
                inputs: vec!["a".to_string()],
                output: "y".to_string(),
                kind: GateKind::Buf,
            }],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &library).unwrap();
        let topology = graph.instances[0].expanded.topology.clone();
        let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
        for (index, specification) in topology.primitives.iter().enumerate() {
            place_primitive(
                &mut candidate,
                specification.id,
                specification.primitive,
                Anchor { x: 10 + index as i32 * 4, y: 1, z: 10 },
            );
        }
        observe(
            &mut candidate,
            ObservationId::PrimaryInput(PortId(0)),
            Anchor { x: 2, y: 1, z: 10 },
            state(BlockKind::Lever),
        );
        candidate.boundaries.insert(
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            BoundaryPlacement {
                endpoint: PhysicalEndpointId::PrimaryInput(PortId(0)),
                delayed: None,
                blocks: vec![PlacedBlock {
                    at: Anchor { x: 2, y: 1, z: 10 },
                    state: state(BlockKind::Lever),
                }],
            },
        );
        for (route_index, connection) in topology.connections.iter().enumerate() {
            let source = match connection.id {
                ConnectionId::External { .. } => PhysicalEndpointId::PrimaryInput(PortId(0)),
                ConnectionId::Internal { .. } => match connection.source {
                    crate::compile::fragment_synth::topology::ConnectionSource::Primitive(id) => {
                        PhysicalEndpointId::PrimitiveOutput(id)
                    }
                    _ => unreachable!(),
                },
            };
            let route = RouteId(route_index as u32);
            let sink = add_route(
                &mut candidate,
                route,
                source,
                RouteTarget::Connection(connection.id),
                Anchor { x: 6 + route_index as i32 * 4, y: 1, z: 10 },
                false,
            );
            candidate.connections.insert(
                connection.id,
                ConnectionBinding {
                    id: connection.id,
                    source,
                    landing: PhysicalEndpointId::Landing(connection.id),
                    route,
                    sink,
                },
            );
        }
        let output = match topology.output {
            crate::compile::fragment_synth::topology::OutputSpec::Primitive(id) => id,
            _ => unreachable!(),
        };
        add_route(
            &mut candidate,
            RouteId(2),
            PhysicalEndpointId::PrimitiveOutput(output),
            RouteTarget::DeclaredOutput(PortId(0)),
            Anchor { x: 18, y: 1, z: 10 },
            false,
        );
        observe(
            &mut candidate,
            ObservationId::InstanceOutput(InstanceId(0)),
            Anchor { x: 14, y: 1, z: 10 },
            state(BlockKind::WallTorch),
        );
        observe(
            &mut candidate,
            ObservationId::DeclaredOutput(PortId(0)),
            Anchor { x: 19, y: 1, z: 10 },
            state(BlockKind::Lamp),
        );
        candidate
    }

    fn mixed_merge_candidate() -> ExpandedPhysicalCandidate {
        let library = Library::default_library();
        let gate = Gate::merge("m", &["a", "b"]);
        let instance_id = InstanceId(0);
        let implementation = ImplementationKey::Merge {
            isolation_mask: InputMask::new(1),
        };
        let expanded = instantiate(&library, &gate, instance_id, &implementation).unwrap();
        let graph = InstanceGraph {
            instances: vec![Instance {
                id: instance_id,
                logical_gate: crate::compile::fragment_synth::identity::GateIndex(0),
                role: InstanceRole::Canonical,
                implementation,
                expanded: expanded.clone(),
            }],
            assignments: Vec::new(),
            primary_inputs: vec![PortId(0), PortId(1)],
            declared_outputs: vec![PortId(0)],
            blocks: Vec::new(),
        };
        let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
        let isolator = expanded.topology.primitives[0];
        place_primitive(
            &mut candidate,
            isolator.id,
            isolator.primitive,
            Anchor { x: 8, y: 1, z: 4 },
        );
        for port in [PortId(0), PortId(1)] {
            let endpoint = PhysicalEndpointId::PrimaryInput(port);
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement { endpoint, delayed: None, blocks: Vec::new() },
            );
            observe(
                &mut candidate,
                ObservationId::PrimaryInput(port),
                Anchor { x: 1, y: 1, z: 4 + port.0 as i32 * 2 },
                state(BlockKind::Lever),
            );
        }
        for (index, connection) in expanded.topology.connections.iter().enumerate() {
            let route = RouteId(index as u32);
            let source = PhysicalEndpointId::PrimaryInput(PortId(index as u32));
            let sink = add_route(
                &mut candidate,
                route,
                source,
                RouteTarget::Connection(connection.id),
                Anchor { x: 5, y: 1, z: 4 + index as i32 * 2 },
                false,
            );
            candidate.connections.insert(
                connection.id,
                ConnectionBinding {
                    id: connection.id,
                    source,
                    landing: PhysicalEndpointId::Landing(connection.id),
                    route,
                    sink,
                },
            );
        }
        add_route(
            &mut candidate,
            RouteId(2),
            PhysicalEndpointId::Junction(instance_id),
            RouteTarget::DeclaredOutput(PortId(0)),
            Anchor { x: 12, y: 1, z: 5 },
            false,
        );
        observe(
            &mut candidate,
            ObservationId::JunctionOutput(instance_id),
            Anchor { x: 10, y: 1, z: 5 },
            state(BlockKind::RedstoneWire),
        );
        observe(
            &mut candidate,
            ObservationId::InstanceOutput(instance_id),
            Anchor { x: 10, y: 1, z: 5 },
            state(BlockKind::RedstoneWire),
        );
        observe(
            &mut candidate,
            ObservationId::DeclaredOutput(PortId(0)),
            Anchor { x: 13, y: 1, z: 5 },
            state(BlockKind::Lamp),
        );
        candidate
    }

    #[test]
    fn predecessor_uses_head_plus_edge_delay_when_arrival_heads_tie() {
        let source = primitive(0);
        let g19 = primitive(19);
        let g20 = primitive(20);
        let output = primitive(21);
        let arcs = vec![
            TimingArc::new(
                TimingArcId(0),
                source,
                g19,
                TimingArcKind::Primitive {
                    primitive: PrimitiveId {
                        instance: InstanceId(19),
                        node: TopologyNodeId(0),
                    },
                },
                ExactDelay(34),
            ),
            TimingArc::new(
                TimingArcId(1),
                source,
                g20,
                TimingArcKind::Primitive {
                    primitive: PrimitiveId {
                        instance: InstanceId(20),
                        node: TopologyNodeId(0),
                    },
                },
                ExactDelay(34),
            ),
            TimingArc::new(
                TimingArcId(2),
                g19,
                output,
                TimingArcKind::Route {
                    route: RouteId(19),
                    sink: RoutedSinkId { route: RouteId(19), ordinal: 0 },
                },
                ExactDelay(6),
            ),
            TimingArc::new(
                TimingArcId(3),
                g20,
                output,
                TimingArcKind::Route {
                    route: RouteId(20),
                    sink: RoutedSinkId { route: RouteId(20), ordinal: 0 },
                },
                ExactDelay(0),
            ),
        ];
        let graph = RealisedTimingGraph::new([source, g19, g20, output], arcs).unwrap();
        let timing = graph.analyse().unwrap();

        assert_eq!(timing.head[&g19], ExactDelay(34));
        assert_eq!(timing.head[&g20], ExactDelay(34));
        assert_eq!(timing.head[&output], ExactDelay(40));
        assert_eq!(timing.predecessor[&output], TimingArcId(2));
        assert_eq!(timing.critical_delay, ExactDelay(40));
    }

    #[test]
    fn buf_path_contains_both_torches_and_the_internal_route() {
        let candidate = buf_candidate();
        let graph = RealisedTimingGraph::derive(&candidate, &certificate(&candidate)).unwrap();
        let instance = &candidate.instances.instances[0];
        let first = instance.expanded.topology.primitives[0].id;
        let second = instance.expanded.topology.primitives[1].id;
        let external = instance.expanded.topology.connections[0].id;
        let internal = instance.expanded.topology.connections[1].id;

        let expected = [
            (
                TimingNodeId::PrimaryInput(PortId(0)),
                TimingNodeId::Landing(external),
                ExactDelay(0),
            ),
            (
                TimingNodeId::Landing(external),
                TimingNodeId::PrimitiveOutput(first),
                ExactDelay(2),
            ),
            (
                TimingNodeId::PrimitiveOutput(first),
                TimingNodeId::Landing(internal),
                ExactDelay(0),
            ),
            (
                TimingNodeId::Landing(internal),
                TimingNodeId::PrimitiveOutput(second),
                ExactDelay(2),
            ),
            (
                TimingNodeId::PrimitiveOutput(second),
                TimingNodeId::InstanceOutput(InstanceId(0)),
                ExactDelay(0),
            ),
        ];
        for (from, to, delay) in expected {
            assert!(graph
                .arcs
                .values()
                .any(|arc| arc.from == from && arc.to == to && arc.delay == delay));
        }
        assert_eq!(graph.analyse().unwrap().critical_delay, ExactDelay(4));
    }

    #[test]
    fn mixed_merge_charges_only_the_isolated_branch_repeater() {
        let candidate = mixed_merge_candidate();
        let graph = RealisedTimingGraph::derive(&candidate, &certificate(&candidate)).unwrap();
        let instance = &candidate.instances.instances[0];
        let isolated = instance.expanded.topology.connections[0].id;
        let bare = instance.expanded.topology.connections[1].id;
        let repeater = instance.expanded.topology.primitives[0].id;
        let junction = TimingNodeId::JunctionOutput(InstanceId(0));

        assert!(graph.arcs.values().any(|arc| {
            arc.from == TimingNodeId::Landing(isolated)
                && arc.to == TimingNodeId::PrimitiveOutput(repeater)
                && arc.delay == ExactDelay(2)
        }));
        assert!(graph.arcs.values().any(|arc| {
            arc.from == TimingNodeId::PrimitiveOutput(repeater)
                && arc.to == junction
                && arc.delay == ExactDelay(0)
        }));
        assert!(graph.arcs.values().any(|arc| {
            arc.from == TimingNodeId::Landing(bare)
                && arc.to == junction
                && arc.delay == ExactDelay(0)
        }));
    }

    #[test]
    fn pinned_bindings_are_charged_once_and_unpinned_output_is_zero_delay() {
        let mut candidate = buf_candidate();
        let input = PhysicalEndpointId::PrimaryInput(PortId(0));
        let input_repeater = Anchor { x: 3, y: 1, z: 10 };
        let input_state = state(BlockKind::Repeater);
        candidate.boundaries.get_mut(&input).unwrap().blocks.push(PlacedBlock {
            at: input_repeater,
            state: input_state,
        });
        candidate.boundaries.get_mut(&input).unwrap().delayed = Some(DelayedComponent {
            at: input_repeater,
            owner: DelayedOwner::InputBinding(PortId(0)),
        });

        let output_route = candidate.routes.get_mut(&RouteId(2)).unwrap();
        let output_state = state(BlockKind::Repeater);
        output_route.cells[0].state = output_state.clone();
        output_route.branches[0].terminal.state = output_state;
        output_route.branches[0].terminal.kind = RouteTerminalKind::OutputTerminalRepeater;
        output_route.branches[0].terminal.repeaters = 1;
        output_route.branches[0].terminal.delayed_owner = Some(DelayedOwner::Route(RouteId(2)));

        let graph = RealisedTimingGraph::derive(&candidate, &certificate(&candidate)).unwrap();
        let input_arc = graph
            .arcs
            .values()
            .find(|arc| arc.kind == TimingArcKind::InputBinding)
            .unwrap();
        assert_eq!(input_arc.delay, ExactDelay(2));
        let output_arc = graph
            .arcs
            .values()
            .find(|arc| arc.to == TimingNodeId::OutputLanding(PortId(0)))
            .unwrap();
        assert_eq!(output_arc.delay, ExactDelay(2));
        let binding = graph
            .arcs
            .values()
            .find(|arc| arc.to == TimingNodeId::DeclaredOutput(PortId(0)))
            .unwrap();
        assert_eq!(binding.kind, TimingArcKind::OutputBinding);
        assert_eq!(binding.delay, ExactDelay(0));

        let unpinned = buf_candidate();
        let graph = RealisedTimingGraph::derive(&unpinned, &certificate(&unpinned)).unwrap();
        assert_eq!(
            graph
                .arcs
                .values()
                .find(|arc| arc.to == TimingNodeId::OutputLanding(PortId(0)))
                .unwrap()
                .delay,
            ExactDelay(0)
        );
    }

    #[test]
    fn derivation_rejects_mutation_after_certification() {
        let mut candidate = buf_candidate();
        let certificate = certificate(&candidate);
        candidate.routes.get_mut(&RouteId(0)).unwrap().cells[0].state.power = 7;
        assert!(matches!(
            RealisedTimingGraph::derive(&candidate, &certificate),
            Err(TimingGraphError::CertificateMismatch { .. })
        ));
    }

    #[test]
    fn delayed_primitive_without_typed_observation_is_rejected() {
        let mut candidate = mixed_merge_candidate();
        let primitive = candidate.instances.instances[0].expanded.topology.primitives[0].id;
        candidate
            .observations
            .remove(&ObservationId::PrimitiveOutput(primitive));
        assert_eq!(
            RealisedTimingGraph::derive(&candidate, &certificate(&candidate)),
            Err(TimingGraphError::DelayedWithoutObservation {
                node: TimingNodeId::PrimitiveOutput(primitive),
            })
        );
    }

    #[test]
    fn graph_constructor_rejects_duplicate_arc_ids_and_cycles() {
        let a = primitive(0);
        let b = primitive(1);
        let arc = TimingArc::new(
            TimingArcId(0),
            a,
            b,
            TimingArcKind::InstanceOutput,
            ExactDelay(0),
        );
        assert_eq!(
            RealisedTimingGraph::new([a, b], [arc, arc]),
            Err(TimingGraphError::DuplicateArc { arc: TimingArcId(0) })
        );
        assert_eq!(
            RealisedTimingGraph::new(
                [a, b],
                [
                    arc,
                    TimingArc::new(
                        TimingArcId(1),
                        b,
                        a,
                        TimingArcKind::InstanceOutput,
                        ExactDelay(0),
                    ),
                ],
            ),
            Err(TimingGraphError::Cycle)
        );
    }
}
