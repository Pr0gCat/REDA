//! Complete typed physical state for one fragment-synthesis candidate.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{
    ConnectionId, GateIndex, ObservationId, ObservationSite, PhysicalEndpointId, PortId,
    PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::instance_graph::{
    InstanceDriver, InstanceGraph, InstanceRole, PhysicalDriver, PhysicalSink,
};
use crate::compile::fragment_synth::topology::{ConnectionSource, OutputSpec};
use crate::compile::geometry::{Anchor, CellFacing};
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::planner::{PortPin, PortPlacements, PortRole, RouteTerminalKind};
pub use crate::compile::routing::{
    DelayedComponent, DelayedOwner, PlacedBlock, RealisedRouteBranch, RealisedRouteTree,
    RouteTarget, TerminalRecord,
};
use crate::compile::Netlist;
use crate::redstone::world::block::{BlockKind, BlockState};
use crate::redstone::world::storage::World;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimitivePlacement {
    pub id: PrimitiveId,
    pub variant: u16,
    pub facing: CellFacing,
    pub anchor: Anchor,
    pub delayed: Option<DelayedComponent>,
    pub blocks: Vec<PlacedBlock>,
}

impl RealisedRouteTree {
    pub fn validate(&self) -> Result<(), CandidateError> {
        let mut cells = BTreeMap::new();
        for block in &self.cells {
            if cells.insert(block.at, &block.state).is_some() {
                return Err(CandidateError::DuplicateRouteCell {
                    route: self.id,
                    at: block.at,
                });
            }
        }
        let mut floors = BTreeSet::new();
        for block in &self.floors {
            if !floors.insert(block.at) {
                return Err(CandidateError::DuplicateRouteFloor {
                    route: self.id,
                    at: block.at,
                });
            }
        }
        let mut sinks = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for branch in &self.branches {
            if branch.sink.route != self.id || branch.terminal.sink != branch.sink {
                return Err(CandidateError::RouteSinkMismatch {
                    route: self.id,
                    sink: branch.sink,
                });
            }
            if !sinks.insert(branch.sink) {
                return Err(CandidateError::DuplicateRoutedSink {
                    route: self.id,
                    sink: branch.sink,
                });
            }
            if !targets.insert(branch.target) {
                return Err(CandidateError::DuplicateRouteTarget {
                    route: self.id,
                    target: branch.target,
                });
            }
            if branch.path.is_empty() {
                return Err(CandidateError::EmptyRoutePath {
                    route: self.id,
                    sink: branch.sink,
                });
            }
            if branch.path.first() != Some(&branch.root)
                || branch.path.last() != Some(&branch.terminal.at)
            {
                return Err(CandidateError::RoutePathEndpointMismatch {
                    route: self.id,
                    sink: branch.sink,
                });
            }
            for pair in branch.path.windows(2) {
                let horizontal = u64::from(pair[0].x.abs_diff(pair[1].x))
                    + u64::from(pair[0].z.abs_diff(pair[1].z));
                let vertical = u64::from(pair[0].y.abs_diff(pair[1].y));
                if horizontal > 1 || vertical > 1 || horizontal + vertical == 0 {
                    return Err(CandidateError::DiscontinuousRoutePath {
                        route: self.id,
                        sink: branch.sink,
                        from: pair[0],
                        to: pair[1],
                    });
                }
            }
            if let Some(state) = cells.get(&branch.terminal.at) {
                if **state != branch.terminal.state {
                    return Err(CandidateError::TerminalStateMismatch {
                        route: self.id,
                        sink: branch.sink,
                        at: branch.terminal.at,
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundaryPlacement {
    pub endpoint: PhysicalEndpointId,
    pub delayed: Option<DelayedComponent>,
    pub blocks: Vec<PlacedBlock>,
}

impl BoundaryPlacement {
    fn validate_key(&self, key: PhysicalEndpointId) -> Result<(), CandidateError> {
        if self.endpoint != key {
            return Err(CandidateError::BoundaryKeyMismatch {
                key,
                endpoint: self.endpoint,
            });
        }
        if !matches!(
            key,
            PhysicalEndpointId::PrimaryInput(_) | PhysicalEndpointId::DeclaredOutput(_)
        ) {
            return Err(CandidateError::InvalidBoundaryEndpoint { endpoint: key });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectionBinding {
    pub id: ConnectionId,
    pub source: PhysicalEndpointId,
    pub landing: PhysicalEndpointId,
    pub route: RouteId,
    pub sink: RoutedSinkId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealisedJunction {
    pub id: crate::compile::fragment_synth::identity::InstanceId,
    pub at: Anchor,
    pub facing: CellFacing,
    pub contributors: Vec<PhysicalEndpointId>,
    pub cells: Vec<PlacedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedObservation {
    pub site: ObservationSite,
    pub state: BlockState,
}

#[derive(Debug, Clone)]
pub struct ExpandedPhysicalCandidate {
    pub instances: InstanceGraph,
    pub placements: BTreeMap<PrimitiveId, PrimitivePlacement>,
    pub boundaries: BTreeMap<PhysicalEndpointId, BoundaryPlacement>,
    pub connections: BTreeMap<ConnectionId, ConnectionBinding>,
    pub routes: BTreeMap<RouteId, RealisedRouteTree>,
    pub junctions: BTreeMap<crate::compile::fragment_synth::identity::InstanceId, RealisedJunction>,
    pub observations: BTreeMap<ObservationId, VerifiedObservation>,
    pub pins: PortPlacements,
    /// Name-free pin contracts resolved once at the netlist boundary.
    pub pin_contracts: BTreeMap<PhysicalEndpointId, PortPin>,
    /// Provenance from the caller-facing name to the authoritative typed ID.
    pub pin_name_bindings: BTreeMap<String, PhysicalEndpointId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityViews {
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_facings: Vec<CellFacing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CandidateError {
    #[error("route {route:?} owns conductor cell {at:?} more than once")]
    DuplicateRouteCell { route: RouteId, at: Anchor },
    #[error("route {route:?} owns floor cell {at:?} more than once")]
    DuplicateRouteFloor { route: RouteId, at: Anchor },
    #[error("route {route:?} terminal {sink:?} at {at:?} disagrees with its owned route cell")]
    TerminalStateMismatch {
        route: RouteId,
        sink: RoutedSinkId,
        at: Anchor,
    },
    #[error("route {route:?} has inconsistent sink {sink:?}")]
    RouteSinkMismatch { route: RouteId, sink: RoutedSinkId },
    #[error("route {route:?} contains duplicate sink {sink:?}")]
    DuplicateRoutedSink { route: RouteId, sink: RoutedSinkId },
    #[error("route {route:?} contains duplicate target {target:?}")]
    DuplicateRouteTarget { route: RouteId, target: RouteTarget },
    #[error("route {route:?} sink {sink:?} has an empty path")]
    EmptyRoutePath { route: RouteId, sink: RoutedSinkId },
    #[error("route {route:?} sink {sink:?} path endpoints do not match root and terminal")]
    RoutePathEndpointMismatch { route: RouteId, sink: RoutedSinkId },
    #[error("route {route:?} sink {sink:?} path jumps from {from:?} to {to:?}")]
    DiscontinuousRoutePath {
        route: RouteId,
        sink: RoutedSinkId,
        from: Anchor,
        to: Anchor,
    },
    #[error("route {route:?} sink {sink:?} path references unowned cell {at:?}")]
    MissingRoutePathCell {
        route: RouteId,
        sink: RoutedSinkId,
        at: Anchor,
    },
    #[error("delayed component at {at:?} is owned by both {first:?} and {second:?}")]
    DuplicateDelayedOwner {
        at: Anchor,
        first: DelayedOwner,
        second: DelayedOwner,
    },
    #[error("world size must be positive")]
    InvalidWorldSize,
    #[error("candidate block {at:?} is outside world size {size:?}")]
    BlockOutsideWorld { at: Anchor, size: (i32, i32, i32) },
    #[error("candidate cell {at:?} has conflicting states")]
    ConflictingBlockState { at: Anchor },
    #[error("candidate cell {at:?} has more than one physical owner")]
    DuplicatePhysicalOwner { at: Anchor },
    #[error("boundary map key {key:?} disagrees with placement endpoint {endpoint:?}")]
    BoundaryKeyMismatch {
        key: PhysicalEndpointId,
        endpoint: PhysicalEndpointId,
    },
    #[error("{endpoint:?} is not a primary-input or declared-output boundary")]
    InvalidBoundaryEndpoint { endpoint: PhysicalEndpointId },
    #[error("candidate identity width exceeded")]
    IdentityOverflow,
    #[error("candidate has no compatibility observation {observation:?}")]
    MissingCompatibilityObservation { observation: ObservationId },
    #[error("candidate has no compatibility facing for instance {instance:?}")]
    MissingCompatibilityFacing {
        instance: crate::compile::fragment_synth::identity::InstanceId,
    },
    #[error("primitive map key {key:?} disagrees with placement {id:?}")]
    PrimitiveKeyMismatch { key: PrimitiveId, id: PrimitiveId },
    #[error("route map key {key:?} disagrees with route {id:?}")]
    RouteKeyMismatch { key: RouteId, id: RouteId },
    #[error("junction map key {key:?} disagrees with junction {id:?}")]
    JunctionKeyMismatch {
        key: crate::compile::fragment_synth::identity::InstanceId,
        id: crate::compile::fragment_synth::identity::InstanceId,
    },
    #[error("delayed component {owner:?} at {at:?} is not one of its owner's blocks")]
    DelayedComponentNotOwned { at: Anchor, owner: DelayedOwner },
    #[error("delayed component at {at:?} names {actual:?}, expected {expected:?}")]
    DelayedOwnerMismatch {
        at: Anchor,
        expected: Option<DelayedOwner>,
        actual: DelayedOwner,
    },
    #[error("delayed component at {at:?} is missing its required owner {expected:?}")]
    MissingDelayedOwner { at: Anchor, expected: DelayedOwner },
    #[error("candidate {collection} IDs do not match its expanded instance graph")]
    CandidateShapeMismatch { collection: &'static str },
    #[error("pin `{name}` is not one unambiguous declared input or output")]
    UnknownPinName { name: String },
    #[error("pinned endpoint {endpoint:?} does not match its boundary contract")]
    PinContractMismatch { endpoint: PhysicalEndpointId },
    #[error("route {route:?} sink {sink:?} does not begin at its typed source")]
    RouteSourceMismatch { route: RouteId, sink: RoutedSinkId },
    #[error("route {route:?} sink {sink:?} timing metadata does not match its owned path")]
    RouteTimingMismatch { route: RouteId, sink: RoutedSinkId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhysicalOwner {
    Primitive(PrimitiveId),
    Boundary(PhysicalEndpointId),
    Route(RouteId),
    Junction(crate::compile::fragment_synth::identity::InstanceId),
}

#[derive(Serialize)]
struct PlacementFingerprint<'a> {
    key: PrimitiveId,
    id: PrimitiveId,
    variant: u16,
    facing: u8,
    anchor: Anchor,
    delayed: Option<DelayedComponent>,
    blocks: Vec<&'a PlacedBlock>,
}

#[derive(Serialize)]
struct BoundaryFingerprint<'a> {
    key: PhysicalEndpointId,
    endpoint: PhysicalEndpointId,
    delayed: Option<DelayedComponent>,
    blocks: Vec<&'a PlacedBlock>,
}

#[derive(Serialize)]
struct RouteFingerprint<'a> {
    key: RouteId,
    id: RouteId,
    source: PhysicalEndpointId,
    cells: Vec<&'a PlacedBlock>,
    floors: Vec<&'a PlacedBlock>,
    branches: &'a [RealisedRouteBranch],
}

#[derive(Serialize)]
struct PinFingerprint<'a> {
    name: &'a str,
    at: Anchor,
    toward: crate::redstone::world::block::Facing,
}

#[derive(Serialize)]
struct TypedPinFingerprint {
    endpoint: PhysicalEndpointId,
    at: Anchor,
    toward: crate::redstone::world::block::Facing,
}

#[derive(Serialize)]
struct JunctionFingerprint<'a> {
    key: crate::compile::fragment_synth::identity::InstanceId,
    id: crate::compile::fragment_synth::identity::InstanceId,
    at: Anchor,
    facing: u8,
    contributors: &'a [PhysicalEndpointId],
    cells: Vec<&'a PlacedBlock>,
}

#[derive(Serialize)]
struct CandidateFingerprintPayload<'a> {
    instances: &'a InstanceGraph,
    placements: Vec<PlacementFingerprint<'a>>,
    boundaries: Vec<BoundaryFingerprint<'a>>,
    connections: Vec<(&'a ConnectionId, &'a ConnectionBinding)>,
    routes: Vec<RouteFingerprint<'a>>,
    junctions: Vec<JunctionFingerprint<'a>>,
    observations: Vec<(&'a ObservationId, &'a VerifiedObservation)>,
    pins: Vec<PinFingerprint<'a>>,
    pin_contracts: Vec<TypedPinFingerprint>,
    pin_name_bindings: Vec<(&'a String, &'a PhysicalEndpointId)>,
}

impl ExpandedPhysicalCandidate {
    pub fn empty(instances: InstanceGraph, pins: PortPlacements) -> Self {
        Self {
            instances,
            placements: BTreeMap::new(),
            boundaries: BTreeMap::new(),
            connections: BTreeMap::new(),
            routes: BTreeMap::new(),
            junctions: BTreeMap::new(),
            observations: BTreeMap::new(),
            pins,
            pin_contracts: BTreeMap::new(),
            pin_name_bindings: BTreeMap::new(),
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        let payload = self.fingerprint_payload();
        let bytes =
            serde_json::to_vec(&payload).expect("candidate fingerprint data must serialize");
        canonical_fingerprint(&bytes)
    }

    /// Test-only escape hatch: `relocate.rs`'s `anchors_of` test needs to
    /// serialise the exact payload `fingerprint()` hashes, to count how many
    /// `Anchor`s it contains independently of the field-by-field walker.
    #[cfg(test)]
    pub(crate) fn fingerprint_payload_for_test(&self) -> impl Serialize + '_ {
        self.fingerprint_payload()
    }

    fn fingerprint_payload(&self) -> CandidateFingerprintPayload<'_> {
        let placements = self
            .placements
            .iter()
            .map(|(&key, placement)| PlacementFingerprint {
                key,
                id: placement.id,
                variant: placement.variant,
                facing: placement.facing.index(),
                anchor: placement.anchor,
                delayed: placement.delayed,
                blocks: sorted_blocks(&placement.blocks),
            })
            .collect();
        let pins = self
            .pins
            .iter()
            .map(|(name, pin)| PinFingerprint {
                name,
                at: pin.at,
                toward: pin.toward,
            })
            .collect();
        CandidateFingerprintPayload {
            instances: &self.instances,
            placements,
            boundaries: self
                .boundaries
                .iter()
                .map(|(&key, boundary)| BoundaryFingerprint {
                    key,
                    endpoint: boundary.endpoint,
                    delayed: boundary.delayed,
                    blocks: sorted_blocks(&boundary.blocks),
                })
                .collect(),
            connections: self.connections.iter().collect(),
            routes: self
                .routes
                .iter()
                .map(|(&key, route)| RouteFingerprint {
                    key,
                    id: route.id,
                    source: route.source,
                    cells: sorted_blocks(&route.cells),
                    floors: sorted_blocks(&route.floors),
                    branches: &route.branches,
                })
                .collect(),
            junctions: self
                .junctions
                .iter()
                .map(|(&key, junction)| JunctionFingerprint {
                    key,
                    id: junction.id,
                    at: junction.at,
                    facing: junction.facing.index(),
                    contributors: &junction.contributors,
                    cells: sorted_blocks(&junction.cells),
                })
                .collect(),
            observations: self.observations.iter().collect(),
            pins,
            pin_contracts: self
                .pin_contracts
                .iter()
                .map(|(&endpoint, pin)| TypedPinFingerprint {
                    endpoint,
                    at: pin.at,
                    toward: pin.toward,
                })
                .collect(),
            pin_name_bindings: self.pin_name_bindings.iter().collect(),
        }
    }

    pub fn validate_physical_ownership(&self) -> Result<(), CandidateError> {
        let mut delayed_at = BTreeMap::new();
        for (&key, placement) in &self.placements {
            if key != placement.id {
                return Err(CandidateError::PrimitiveKeyMismatch {
                    key,
                    id: placement.id,
                });
            }
            if let Some(delayed) = placement.delayed {
                let expected = DelayedOwner::Primitive(key);
                if delayed.owner != expected {
                    return Err(CandidateError::DelayedOwnerMismatch {
                        at: delayed.at,
                        expected: Some(expected),
                        actual: delayed.owner,
                    });
                }
                if !placement.blocks.iter().any(|block| block.at == delayed.at) {
                    return Err(CandidateError::DelayedComponentNotOwned {
                        at: delayed.at,
                        owner: delayed.owner,
                    });
                }
                claim_delayed(&mut delayed_at, delayed.at, delayed.owner)?;
            } else if let Some(block) = placement
                .blocks
                .iter()
                .find(|block| block.state.kind == BlockKind::Repeater)
            {
                return Err(CandidateError::MissingDelayedOwner {
                    at: block.at,
                    expected: DelayedOwner::Primitive(key),
                });
            }
        }
        for (&endpoint, placement) in &self.boundaries {
            placement.validate_key(endpoint)?;
            if let Some(delayed) = placement.delayed {
                let expected = match endpoint {
                    PhysicalEndpointId::PrimaryInput(port) => {
                        Some(DelayedOwner::InputBinding(port))
                    }
                    PhysicalEndpointId::DeclaredOutput(_) => None,
                    _ => unreachable!("boundary key validation rejects non-port endpoints"),
                };
                if Some(delayed.owner) != expected {
                    return Err(CandidateError::DelayedOwnerMismatch {
                        at: delayed.at,
                        expected,
                        actual: delayed.owner,
                    });
                }
                if !placement.blocks.iter().any(|block| block.at == delayed.at) {
                    return Err(CandidateError::DelayedComponentNotOwned {
                        at: delayed.at,
                        owner: delayed.owner,
                    });
                }
                claim_delayed(&mut delayed_at, delayed.at, delayed.owner)?;
            }
        }
        for (&key, route) in &self.routes {
            if key != route.id {
                return Err(CandidateError::RouteKeyMismatch { key, id: route.id });
            }
            route.validate()?;
            for branch in &route.branches {
                if let Some(owner) = branch.terminal.delayed_owner {
                    match owner {
                        DelayedOwner::Route(owner_route) => {
                            let expected = DelayedOwner::Route(route.id);
                            if owner_route != route.id
                                || !route.cells.iter().any(|block| {
                                    block.at == branch.terminal.at
                                        && block.state == branch.terminal.state
                                })
                            {
                                return Err(CandidateError::DelayedOwnerMismatch {
                                    at: branch.terminal.at,
                                    expected: Some(expected),
                                    actual: owner,
                                });
                            }
                            claim_delayed(&mut delayed_at, branch.terminal.at, owner)?;
                        }
                        DelayedOwner::Primitive(primitive) => {
                            let valid = self.placements.get(&primitive).is_some_and(|placement| {
                                placement.delayed
                                    == Some(DelayedComponent {
                                        at: branch.terminal.at,
                                        owner,
                                    })
                                    && placement.blocks.iter().any(|block| {
                                        block.at == branch.terminal.at
                                            && block.state == branch.terminal.state
                                    })
                            });
                            if !valid {
                                return Err(CandidateError::DelayedOwnerMismatch {
                                    at: branch.terminal.at,
                                    expected: None,
                                    actual: owner,
                                });
                            }
                        }
                        DelayedOwner::InputBinding(port) => {
                            let endpoint = PhysicalEndpointId::PrimaryInput(port);
                            let valid = self.boundaries.get(&endpoint).is_some_and(|boundary| {
                                boundary.delayed
                                    == Some(DelayedComponent {
                                        at: branch.terminal.at,
                                        owner,
                                    })
                                    && boundary.blocks.iter().any(|block| {
                                        block.at == branch.terminal.at
                                            && block.state == branch.terminal.state
                                    })
                            });
                            if !valid {
                                return Err(CandidateError::DelayedOwnerMismatch {
                                    at: branch.terminal.at,
                                    expected: None,
                                    actual: owner,
                                });
                            }
                        }
                    }
                }
            }
        }
        for (&key, junction) in &self.junctions {
            if key != junction.id {
                return Err(CandidateError::JunctionKeyMismatch {
                    key,
                    id: junction.id,
                });
            }
        }
        let ledger = self.physical_ledger()?;
        for route in self.routes.values() {
            for branch in &route.branches {
                for &at in branch
                    .path
                    .iter()
                    .skip(1)
                    .take(branch.path.len().saturating_sub(2))
                {
                    if !ledger.contains_key(&at) {
                        return Err(CandidateError::MissingRoutePathCell {
                            route: route.id,
                            sink: branch.sink,
                            at,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    pub fn validate_shape(&self) -> Result<(), CandidateError> {
        self.validate_pin_contracts()?;
        let expected_primitives = self
            .instances
            .instances
            .iter()
            .flat_map(|instance| {
                instance
                    .expanded
                    .topology
                    .primitives
                    .iter()
                    .map(|node| node.id)
            })
            .collect::<BTreeSet<_>>();
        if self.placements.keys().copied().collect::<BTreeSet<_>>() != expected_primitives {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "primitive placement",
            });
        }

        let expected_connections = self
            .instances
            .instances
            .iter()
            .flat_map(|instance| {
                instance
                    .expanded
                    .topology
                    .connections
                    .iter()
                    .map(|connection| connection.id)
            })
            .collect::<BTreeSet<_>>();
        if self.connections.keys().copied().collect::<BTreeSet<_>>() != expected_connections {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "connection binding",
            });
        }

        let expected_junctions = self
            .instances
            .instances
            .iter()
            .filter_map(|instance| {
                matches!(
                    instance.expanded.topology.output,
                    OutputSpec::Junction { .. }
                )
                .then_some(instance.id)
            })
            .collect::<BTreeSet<_>>();
        if self.junctions.keys().copied().collect::<BTreeSet<_>>() != expected_junctions {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "junction",
            });
        }

        let expected_boundaries = self
            .instances
            .primary_inputs
            .iter()
            .copied()
            .map(PhysicalEndpointId::PrimaryInput)
            .chain(
                self.instances
                    .declared_outputs
                    .iter()
                    .copied()
                    .map(PhysicalEndpointId::DeclaredOutput),
            )
            .collect::<BTreeSet<_>>();
        if self.boundaries.keys().copied().collect::<BTreeSet<_>>() != expected_boundaries {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "boundary",
            });
        }

        let mut expected_observations = self
            .instances
            .primary_inputs
            .iter()
            .copied()
            .map(ObservationId::PrimaryInput)
            .chain(
                self.instances
                    .declared_outputs
                    .iter()
                    .copied()
                    .map(ObservationId::DeclaredOutput),
            )
            .collect::<BTreeSet<_>>();
        for instance in &self.instances.instances {
            expected_observations.insert(ObservationId::InstanceOutput(instance.id));
            for primitive in &instance.expanded.topology.primitives {
                expected_observations.insert(ObservationId::PrimitiveOutput(primitive.id));
            }
            if matches!(
                instance.expanded.topology.output,
                OutputSpec::Junction { .. }
            ) {
                expected_observations.insert(ObservationId::JunctionOutput(instance.id));
            }
        }
        if self.observations.keys().copied().collect::<BTreeSet<_>>() != expected_observations {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "observation",
            });
        }

        if self
            .observations
            .iter()
            .any(|(key, observation)| *key != observation.site.id)
        {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "observation identity",
            });
        }

        let mut connection_specs = BTreeMap::new();
        for instance in &self.instances.instances {
            for connection in &instance.expanded.topology.connections {
                connection_specs.insert(connection.id, (instance.id, connection));
            }
        }

        for (&id, binding) in &self.connections {
            if binding.id != id || binding.sink.route != binding.route {
                return Err(CandidateError::CandidateShapeMismatch {
                    collection: "connection identity",
                });
            }
            let route =
                self.routes
                    .get(&binding.route)
                    .ok_or(CandidateError::CandidateShapeMismatch {
                        collection: "connection route",
                    })?;
            if binding.landing != PhysicalEndpointId::Landing(id) {
                return Err(CandidateError::CandidateShapeMismatch {
                    collection: "connection landing",
                });
            }
            let (instance, spec) =
                connection_specs
                    .get(&id)
                    .ok_or(CandidateError::CandidateShapeMismatch {
                        collection: "connection specification",
                    })?;
            let expected_source = match spec.source {
                ConnectionSource::Primitive(primitive) => {
                    Some(PhysicalEndpointId::PrimitiveOutput(primitive))
                }
                ConnectionSource::ExternalInput { input_index } => self
                    .instances
                    .assignments
                    .iter()
                    .find(|assignment| {
                        assignment.sink
                            == PhysicalSink::InstanceInput {
                                instance: *instance,
                                input_index,
                            }
                    })
                    .and_then(|assignment| endpoint_for_driver(&assignment.driver)),
            }
            .ok_or(CandidateError::CandidateShapeMismatch {
                collection: "connection source",
            })?;
            if binding.source != expected_source || route.source != expected_source {
                return Err(CandidateError::CandidateShapeMismatch {
                    collection: "connection source",
                });
            }
            let matching_branches = route
                .branches
                .iter()
                .filter(|branch| {
                    branch.sink == binding.sink && branch.target == RouteTarget::Connection(id)
                })
                .count();
            if matching_branches != 1 {
                return Err(CandidateError::CandidateShapeMismatch {
                    collection: "connection sink",
                });
            }
        }

        let realised_connections = self
            .routes
            .values()
            .flat_map(|route| route.branches.iter())
            .filter_map(|branch| match branch.target {
                RouteTarget::Connection(connection) => Some(connection),
                RouteTarget::DeclaredOutput(_) => None,
            })
            .collect::<Vec<_>>();
        if realised_connections.len() != expected_connections.len()
            || realised_connections
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
                != expected_connections
        {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "connection route target",
            });
        }

        for &port in &self.instances.declared_outputs {
            let assignment = self
                .instances
                .assignments
                .iter()
                .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                .ok_or(CandidateError::CandidateShapeMismatch {
                    collection: "declared output assignment",
                })?;
            let expected_source = endpoint_for_driver(&assignment.driver).ok_or(
                CandidateError::CandidateShapeMismatch {
                    collection: "declared output source",
                },
            )?;
            let matches = self
                .routes
                .values()
                .flat_map(|route| route.branches.iter().map(move |branch| (route, branch)))
                .filter(|(route, branch)| {
                    route.source == expected_source
                        && branch.target == RouteTarget::DeclaredOutput(port)
                })
                .count();
            if matches != 1 {
                return Err(CandidateError::CandidateShapeMismatch {
                    collection: "declared output route",
                });
            }
        }
        let realised_outputs = self
            .routes
            .values()
            .flat_map(|route| route.branches.iter())
            .filter_map(|branch| match branch.target {
                RouteTarget::DeclaredOutput(port) => Some(port),
                RouteTarget::Connection(_) => None,
            })
            .collect::<Vec<_>>();
        if realised_outputs.len() != self.instances.declared_outputs.len()
            || realised_outputs.iter().copied().collect::<BTreeSet<_>>()
                != self
                    .instances
                    .declared_outputs
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
        {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "declared output route",
            });
        }
        self.validate_route_timing()?;
        Ok(())
    }

    pub fn validate_route_timing(&self) -> Result<(), CandidateError> {
        let ledger = self.physical_ledger()?;
        for route in self.routes.values() {
            let source_observation = match route.source {
                PhysicalEndpointId::PrimaryInput(port) => ObservationId::PrimaryInput(port),
                PhysicalEndpointId::PrimitiveOutput(primitive) => {
                    ObservationId::PrimitiveOutput(primitive)
                }
                PhysicalEndpointId::Junction(instance) => ObservationId::JunctionOutput(instance),
                _ => {
                    return Err(CandidateError::CandidateShapeMismatch {
                        collection: "route source",
                    })
                }
            };
            let source_at = self
                .pin_contracts
                .get(&route.source)
                .filter(|_| matches!(route.source, PhysicalEndpointId::PrimaryInput(_)))
                .map(|pin| pin.net_cell(PortRole::Input))
                .or_else(|| {
                    self.observations
                        .get(&source_observation)
                        .map(|observation| observation.site.at)
                })
                .ok_or(CandidateError::CandidateShapeMismatch {
                    collection: "route source observation",
                })?;
            for branch in &route.branches {
                let source_gap = u64::from(source_at.x.abs_diff(branch.root.x))
                    + u64::from(source_at.y.abs_diff(branch.root.y))
                    + u64::from(source_at.z.abs_diff(branch.root.z));
                if source_gap > 1 {
                    return Err(CandidateError::RouteSourceMismatch {
                        route: route.id,
                        sink: branch.sink,
                    });
                }
                let mut owned_repeaters = 0u64;
                for &at in &branch.path {
                    let Some((state, owner)) = ledger.get(&at) else {
                        return Err(CandidateError::RouteTimingMismatch {
                            route: route.id,
                            sink: branch.sink,
                        });
                    };
                    let is_this_branches_target_primitive = at == branch.terminal.at
                        && matches!(
                            branch.terminal.delayed_owner,
                            Some(DelayedOwner::Primitive(_))
                        );
                    let is_delivery_terminal = at == branch.terminal.at
                        && branch.terminal.kind == RouteTerminalKind::OutputTerminalRepeater;
                    if state.kind == crate::redstone::world::block::BlockKind::Repeater
                        && (*owner == PhysicalOwner::Route(route.id)
                            || matches!(owner, PhysicalOwner::Primitive(_)))
                        && !is_this_branches_target_primitive
                        && !is_delivery_terminal
                    {
                        owned_repeaters += 1;
                    }
                }
                let actual_terminal =
                    ledger
                        .get(&branch.terminal.at)
                        .ok_or(CandidateError::RouteTimingMismatch {
                            route: route.id,
                            sink: branch.sink,
                        })?;
                let terminal_kind_matches = match branch.terminal.kind {
                    RouteTerminalKind::RepeaterIntoSupport
                    | RouteTerminalKind::BareMergeRepeater
                    | RouteTerminalKind::OutputTerminalRepeater => {
                        actual_terminal.0.kind == BlockKind::Repeater
                    }
                    RouteTerminalKind::DirectedDustIntoSupport
                    | RouteTerminalKind::BareMergeDust => {
                        actual_terminal.0.kind == BlockKind::RedstoneWire
                    }
                };
                if actual_terminal.0 != branch.terminal.state
                    || !terminal_kind_matches
                    || owned_repeaters != branch.terminal.repeaters
                {
                    return Err(CandidateError::RouteTimingMismatch {
                        route: route.id,
                        sink: branch.sink,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn bind_pin_contracts(&mut self, netlist: &Netlist) -> Result<(), CandidateError> {
        let mut resolved = BTreeMap::new();
        let mut names = BTreeMap::new();
        for (name, pin) in self.pins.iter() {
            let input = netlist
                .inputs
                .iter()
                .position(|candidate| candidate == name);
            let output = netlist
                .outputs
                .iter()
                .position(|candidate| candidate == name);
            let endpoint = match (input, output) {
                (Some(index), None) => PhysicalEndpointId::PrimaryInput(PortId(
                    u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?,
                )),
                (None, Some(index)) => PhysicalEndpointId::DeclaredOutput(PortId(
                    u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?,
                )),
                _ => return Err(CandidateError::UnknownPinName { name: name.clone() }),
            };
            resolved.insert(endpoint, *pin);
            names.insert(name.clone(), endpoint);
        }
        self.pin_contracts = resolved;
        self.pin_name_bindings = names;
        Ok(())
    }

    pub fn validate_pin_contracts_against(&self, netlist: &Netlist) -> Result<(), CandidateError> {
        self.validate_pin_contracts()?;
        crate::compile::planner::validate_port_placements(netlist, &self.pins).map_err(|_| {
            CandidateError::CandidateShapeMismatch {
                collection: "pin placement",
            }
        })?;
        for (name, pin) in self.pins.iter() {
            let input = netlist
                .inputs
                .iter()
                .position(|candidate| candidate == name);
            let output = netlist
                .outputs
                .iter()
                .position(|candidate| candidate == name);
            let endpoint = match (input, output) {
                (Some(index), None) => PhysicalEndpointId::PrimaryInput(PortId(
                    u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?,
                )),
                (None, Some(index)) => PhysicalEndpointId::DeclaredOutput(PortId(
                    u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?,
                )),
                _ => return Err(CandidateError::UnknownPinName { name: name.clone() }),
            };
            if self.pin_name_bindings.get(name) != Some(&endpoint)
                || self.pin_contracts.get(&endpoint) != Some(pin)
            {
                return Err(CandidateError::PinContractMismatch { endpoint });
            }
        }
        Ok(())
    }

    pub fn validate_pin_contracts(&self) -> Result<(), CandidateError> {
        if self.pins.iter().count() != self.pin_name_bindings.len()
            || self.pin_name_bindings.len() != self.pin_contracts.len()
            || self.pins.iter().any(|(name, pin)| {
                self.pin_name_bindings
                    .get(name)
                    .and_then(|endpoint| self.pin_contracts.get(endpoint))
                    != Some(pin)
            })
            || self
                .pin_name_bindings
                .values()
                .copied()
                .collect::<BTreeSet<_>>()
                != self.pin_contracts.keys().copied().collect::<BTreeSet<_>>()
        {
            return Err(CandidateError::CandidateShapeMismatch {
                collection: "pin contract",
            });
        }

        for (&endpoint, pin) in &self.pin_contracts {
            if matches!(
                pin.toward,
                crate::redstone::world::block::Facing::Up
                    | crate::redstone::world::block::Facing::Down
            ) {
                return Err(CandidateError::PinContractMismatch { endpoint });
            }
            let (role, observation, expected_delayed) = match endpoint {
                PhysicalEndpointId::PrimaryInput(port) => (
                    PortRole::Input,
                    ObservationId::PrimaryInput(port),
                    Some(DelayedOwner::InputBinding(port)),
                ),
                PhysicalEndpointId::DeclaredOutput(port) => {
                    (PortRole::Output, ObservationId::DeclaredOutput(port), None)
                }
                _ => return Err(CandidateError::PinContractMismatch { endpoint }),
            };
            let site = self
                .observations
                .get(&observation)
                .ok_or(CandidateError::PinContractMismatch { endpoint })?;
            if site.site.at != pin.at
                || site.state.kind != crate::redstone::world::block::BlockKind::Air
                || self.all_owned_blocks().any(|block| block.at == pin.at)
            {
                return Err(CandidateError::PinContractMismatch { endpoint });
            }
            let handover = pin.handover(role);
            let forbidden_conductor = [
                Anchor {
                    x: pin.at.x - 1,
                    ..pin.at
                },
                Anchor {
                    x: pin.at.x + 1,
                    ..pin.at
                },
                Anchor {
                    y: pin.at.y - 1,
                    ..pin.at
                },
                Anchor {
                    y: pin.at.y + 1,
                    ..pin.at
                },
                Anchor {
                    z: pin.at.z - 1,
                    ..pin.at
                },
                Anchor {
                    z: pin.at.z + 1,
                    ..pin.at
                },
            ]
            .into_iter()
            .filter(|at| *at != handover)
            .any(|at| {
                self.all_owned_blocks()
                    .any(|block| block.at == at && is_signal_carrying(block.state.kind))
            });
            if forbidden_conductor {
                return Err(CandidateError::PinContractMismatch { endpoint });
            }
            match role {
                PortRole::Input => {
                    let boundary = self
                        .boundaries
                        .get(&endpoint)
                        .ok_or(CandidateError::PinContractMismatch { endpoint })?;
                    if boundary.delayed
                        != expected_delayed.map(|owner| DelayedComponent {
                            at: handover,
                            owner,
                        })
                        || !boundary.blocks.iter().any(|block| {
                            block.at == handover
                                && block.state.kind
                                    == crate::redstone::world::block::BlockKind::Repeater
                                && block.state.facing == Some(pin.toward.opposite())
                        })
                    {
                        return Err(CandidateError::PinContractMismatch { endpoint });
                    }
                }
                PortRole::Output => {
                    let matching = self
                        .routes
                        .values()
                        .flat_map(|route| route.branches.iter().map(move |branch| (route, branch)))
                        .filter(|(route, branch)| {
                            branch.target
                                == RouteTarget::DeclaredOutput(match endpoint {
                                    PhysicalEndpointId::DeclaredOutput(port) => port,
                                    _ => unreachable!(),
                                })
                                && branch.terminal.at == handover
                                && branch.terminal.state.kind
                                    == crate::redstone::world::block::BlockKind::Repeater
                                && branch.terminal.state.facing == Some(pin.toward.opposite())
                                && branch.terminal.delayed_owner
                                    == Some(DelayedOwner::Route(route.id))
                        })
                        .count();
                    if matching != 1 {
                        return Err(CandidateError::PinContractMismatch { endpoint });
                    }
                }
            }
        }
        Ok(())
    }

    fn all_owned_blocks(&self) -> impl Iterator<Item = &PlacedBlock> {
        self.placements
            .values()
            .flat_map(|placement| placement.blocks.iter())
            .chain(
                self.boundaries
                    .values()
                    .flat_map(|boundary| boundary.blocks.iter()),
            )
            .chain(
                self.routes
                    .values()
                    .flat_map(|route| route.cells.iter().chain(route.floors.iter())),
            )
            .chain(
                self.junctions
                    .values()
                    .flat_map(|junction| junction.cells.iter()),
            )
    }

    fn physical_ledger(
        &self,
    ) -> Result<BTreeMap<Anchor, (BlockState, PhysicalOwner)>, CandidateError> {
        let mut ledger = BTreeMap::new();
        let mut claim = |block: &PlacedBlock, owner: PhysicalOwner| {
            if ledger
                .insert(block.at, (block.state.clone(), owner))
                .is_some()
            {
                return Err(CandidateError::DuplicatePhysicalOwner { at: block.at });
            }
            Ok(())
        };
        for (&id, placement) in &self.placements {
            for block in &placement.blocks {
                claim(block, PhysicalOwner::Primitive(id))?;
            }
        }
        for (&endpoint, boundary) in &self.boundaries {
            for block in &boundary.blocks {
                claim(block, PhysicalOwner::Boundary(endpoint))?;
            }
        }
        for (&id, route) in &self.routes {
            for block in route.cells.iter().chain(route.floors.iter()) {
                claim(block, PhysicalOwner::Route(id))?;
            }
        }
        for (&id, junction) in &self.junctions {
            for block in &junction.cells {
                claim(block, PhysicalOwner::Junction(id))?;
            }
        }
        Ok(ledger)
    }

    pub fn emit_world(&self, size: (i32, i32, i32)) -> Result<World, CandidateError> {
        self.validate_shape()?;
        self.emit_world_unchecked(size)
    }

    fn emit_world_unchecked(&self, size: (i32, i32, i32)) -> Result<World, CandidateError> {
        if size.0 <= 0 || size.1 <= 0 || size.2 <= 0 {
            return Err(CandidateError::InvalidWorldSize);
        }
        self.validate_physical_ownership()?;
        let mut cells = BTreeMap::<Anchor, (BlockState, PhysicalOwner)>::new();
        for (&id, placement) in &self.placements {
            for block in &placement.blocks {
                claim_physical_block(&mut cells, block, PhysicalOwner::Primitive(id), size)?;
            }
        }
        for (&endpoint, placement) in &self.boundaries {
            for block in &placement.blocks {
                claim_physical_block(&mut cells, block, PhysicalOwner::Boundary(endpoint), size)?;
            }
        }
        for (&id, route) in &self.routes {
            for block in route.owned_blocks() {
                claim_physical_block(&mut cells, &block, PhysicalOwner::Route(id), size)?;
            }
        }
        for (&id, junction) in &self.junctions {
            for block in &junction.cells {
                claim_physical_block(&mut cells, block, PhysicalOwner::Junction(id), size)?;
            }
        }
        let mut world = World::new(size.0, size.1, size.2);
        for (at, (state, _)) in cells {
            world.set(at.x, at.y, at.z, state);
        }
        Ok(world)
    }

    /// Derive the legacy coordinate/facing surface from typed observations and
    /// physical ownership. These values are never independent candidate state.
    pub fn compatibility_views(
        &self,
        netlist: &Netlist,
    ) -> Result<CompatibilityViews, CandidateError> {
        let mut input_positions = BTreeMap::new();
        for (index, name) in netlist.inputs.iter().enumerate() {
            let observation = ObservationId::PrimaryInput(PortId(
                u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?,
            ));
            let at = self
                .observations
                .get(&observation)
                .ok_or(CandidateError::MissingCompatibilityObservation { observation })?
                .site
                .at;
            input_positions.insert(name.clone(), (at.x, at.y, at.z));
        }

        let mut output_positions = BTreeMap::new();
        for (index, name) in netlist.outputs.iter().enumerate() {
            let observation = ObservationId::DeclaredOutput(PortId(
                u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?,
            ));
            let at = self
                .observations
                .get(&observation)
                .ok_or(CandidateError::MissingCompatibilityObservation { observation })?
                .site
                .at;
            output_positions.insert(name.clone(), (at.x, at.y, at.z));
        }

        let mut gate_output_positions = BTreeMap::new();
        let mut gate_facings = Vec::with_capacity(netlist.gates.len());
        for (index, gate) in netlist.gates.iter().enumerate() {
            let logical_gate =
                GateIndex(u32::try_from(index).map_err(|_| CandidateError::IdentityOverflow)?);
            let instance = self
                .instances
                .instances
                .iter()
                .find(|instance| {
                    instance.logical_gate == logical_gate
                        && instance.role == InstanceRole::Canonical
                })
                .ok_or(CandidateError::MissingCompatibilityFacing {
                    instance: crate::compile::fragment_synth::identity::InstanceId(logical_gate.0),
                })?;
            let observation = ObservationId::InstanceOutput(instance.id);
            let at = self
                .observations
                .get(&observation)
                .ok_or(CandidateError::MissingCompatibilityObservation { observation })?
                .site
                .at;
            gate_output_positions.insert(gate.output.clone(), (at.x, at.y, at.z));
            let facing = match &instance.expanded.topology.output {
                OutputSpec::Primitive(primitive) => self
                    .placements
                    .get(primitive)
                    .map(|placement| placement.facing),
                OutputSpec::Junction { .. } => self
                    .junctions
                    .get(&instance.id)
                    .map(|junction| junction.facing),
            }
            .ok_or(CandidateError::MissingCompatibilityFacing {
                instance: instance.id,
            })?;
            gate_facings.push(facing);
        }

        Ok(CompatibilityViews {
            input_positions,
            output_positions,
            gate_output_positions,
            gate_facings,
        })
    }
}

fn is_signal_carrying(kind: BlockKind) -> bool {
    !matches!(
        kind,
        BlockKind::Air | BlockKind::Solid | BlockKind::Glass | BlockKind::Slab
    )
}

pub(crate) fn endpoint_for_driver(driver: &PhysicalDriver) -> Option<PhysicalEndpointId> {
    match driver {
        PhysicalDriver::PrimaryInput(port) => Some(PhysicalEndpointId::PrimaryInput(*port)),
        PhysicalDriver::Instance(InstanceDriver::Primitive { terminals, .. }) => {
            let [primitive] = terminals.as_slice() else {
                return None;
            };
            Some(PhysicalEndpointId::PrimitiveOutput(*primitive))
        }
        PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
            Some(PhysicalEndpointId::Junction(*logical_owner))
        }
    }
}

fn claim_physical_block(
    cells: &mut BTreeMap<Anchor, (BlockState, PhysicalOwner)>,
    block: &PlacedBlock,
    owner: PhysicalOwner,
    size: (i32, i32, i32),
) -> Result<(), CandidateError> {
    if block.at.x < 0
        || block.at.y < 0
        || block.at.z < 0
        || block.at.x >= size.0
        || block.at.y >= size.1
        || block.at.z >= size.2
    {
        return Err(CandidateError::BlockOutsideWorld { at: block.at, size });
    }
    if let Some((existing, first_owner)) = cells.get(&block.at) {
        if *first_owner != owner {
            return Err(CandidateError::DuplicatePhysicalOwner { at: block.at });
        }
        if existing != &block.state {
            return Err(CandidateError::ConflictingBlockState { at: block.at });
        }
        return Ok(());
    }
    cells.insert(block.at, (block.state.clone(), owner));
    Ok(())
}

fn claim_delayed(
    delayed_at: &mut BTreeMap<Anchor, DelayedOwner>,
    at: Anchor,
    owner: DelayedOwner,
) -> Result<(), CandidateError> {
    if let Some(first) = delayed_at.insert(at, owner) {
        return Err(CandidateError::DuplicateDelayedOwner {
            at,
            first,
            second: owner,
        });
    }
    Ok(())
}

fn sorted_blocks(blocks: &[PlacedBlock]) -> Vec<&PlacedBlock> {
    let mut sorted = blocks.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|block| block.at);
    sorted
}

#[cfg(test)]
mod tests {
    use crate::compile::fragment_synth::identity::{
        ConnectionId, InstanceId, ObservationId, ObservationSite, PhysicalEndpointId, PortId,
        PrimitiveId, RouteId, RoutedSinkId,
    };
    use crate::compile::fragment_synth::instance_graph::InstanceGraph;
    use crate::compile::geometry::{Anchor, CellFacing};
    use crate::compile::planner::PortPlacements;
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};
    use crate::redstone::world::block::{BlockKind, BlockState};

    use super::{
        BoundaryPlacement, CandidateError, ConnectionBinding, DelayedComponent, DelayedOwner,
        ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedRouteBranch,
        RealisedRouteTree, RouteTarget, TerminalRecord, VerifiedObservation,
    };

    fn state(kind: BlockKind, name: &str) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = name.to_string();
        state
    }

    fn two_gate_candidate() -> ExpandedPhysicalCandidate {
        let netlist = two_gate_netlist();
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        ExpandedPhysicalCandidate::empty(graph, PortPlacements::default())
    }

    fn two_gate_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["x".to_string(), "y".to_string()],
            gates: vec![Gate::nor("x", &["a"]), Gate::nor("y", &["b"])],
        }
    }

    fn placement(id: PrimitiveId, at: Anchor) -> PrimitivePlacement {
        PrimitivePlacement {
            id,
            variant: 0,
            facing: CellFacing::NORTH,
            anchor: at,
            delayed: None,
            blocks: vec![PlacedBlock {
                at,
                state: state(BlockKind::Torch, "minecraft:redstone_torch"),
            }],
        }
    }

    #[test]
    fn candidate_fingerprint_binds_typed_ownership_even_when_world_bytes_match() {
        let mut left = two_gate_candidate();
        let first = left.instances.instances[0].expanded.topology.primitives[0].id;
        let second = left.instances.instances[1].expanded.topology.primitives[0].id;
        let a = Anchor { x: 1, y: 1, z: 1 };
        let b = Anchor { x: 5, y: 1, z: 1 };
        left.placements.insert(first, placement(first, a));
        left.placements.insert(second, placement(second, b));

        let mut swapped = two_gate_candidate();
        swapped.placements.insert(first, placement(first, b));
        swapped.placements.insert(second, placement(second, a));

        assert_ne!(left.fingerprint(), swapped.fingerprint());
        assert_eq!(
            left.emit_world_unchecked((8, 4, 4)).unwrap().cells(),
            swapped.emit_world_unchecked((8, 4, 4)).unwrap().cells()
        );
    }

    #[test]
    fn route_tree_owns_shared_cells_once_and_terminals_only_reference_them() {
        let trunk_cell = PlacedBlock {
            at: Anchor { x: 1, y: 1, z: 1 },
            state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
        };
        let route = RouteId(9);
        let terminal = |ordinal, x| TerminalRecord {
            sink: RoutedSinkId { route, ordinal },
            at: Anchor { x, y: 1, z: 1 },
            state: state(BlockKind::Repeater, "minecraft:repeater"),
            kind: crate::compile::planner::RouteTerminalKind::RepeaterIntoSupport,
            repeaters: 1,
            delayed_owner: Some(DelayedOwner::Route(route)),
        };
        let valid = RealisedRouteTree {
            id: route,
            source: crate::compile::fragment_synth::identity::PhysicalEndpointId::Junction(
                InstanceId(0),
            ),
            cells: vec![
                trunk_cell.clone(),
                PlacedBlock {
                    at: terminal(0, 2).at,
                    state: terminal(0, 2).state,
                },
                PlacedBlock {
                    at: terminal(1, 3).at,
                    state: terminal(1, 3).state,
                },
            ],
            floors: Vec::new(),
            branches: vec![
                RealisedRouteBranch {
                    sink: RoutedSinkId { route, ordinal: 0 },
                    target: RouteTarget::Connection(ConnectionId::External {
                        instance: InstanceId(0),
                        input_index: 0,
                    }),
                    root: trunk_cell.at,
                    path: vec![trunk_cell.at, terminal(0, 2).at],
                    terminal: terminal(0, 2),
                },
                RealisedRouteBranch {
                    sink: RoutedSinkId { route, ordinal: 1 },
                    target: RouteTarget::Connection(ConnectionId::External {
                        instance: InstanceId(0),
                        input_index: 1,
                    }),
                    root: trunk_cell.at,
                    path: vec![trunk_cell.at, terminal(0, 2).at, terminal(1, 3).at],
                    terminal: terminal(1, 3),
                },
            ],
        };
        assert_eq!(valid.owned_blocks().count(), 3);
        valid.validate().unwrap();

        let mut invalid = valid;
        invalid.cells.push(trunk_cell.clone());
        assert_eq!(
            invalid.validate(),
            Err(CandidateError::DuplicateRouteCell {
                route,
                at: trunk_cell.at,
            })
        );
    }

    #[test]
    fn topology_repeater_and_route_terminal_cannot_both_own_one_delayed_component() {
        let mut candidate = two_gate_candidate();
        let primitive = candidate.instances.instances[0]
            .expanded
            .topology
            .primitives[0]
            .id;
        let at = Anchor { x: 2, y: 1, z: 2 };
        let mut primitive_placement = placement(primitive, at);
        primitive_placement.blocks[0].state = state(BlockKind::Repeater, "minecraft:repeater");
        primitive_placement.delayed = Some(DelayedComponent {
            at,
            owner: DelayedOwner::Primitive(primitive),
        });
        candidate.placements.insert(primitive, primitive_placement);

        let route = RouteId(0);
        candidate.routes.insert(
            route,
            RealisedRouteTree {
                id: route,
                source:
                    crate::compile::fragment_synth::identity::PhysicalEndpointId::PrimitiveOutput(
                        primitive,
                    ),
                cells: vec![PlacedBlock {
                    at,
                    state: state(BlockKind::Repeater, "minecraft:repeater"),
                }],
                floors: Vec::new(),
                branches: vec![RealisedRouteBranch {
                    sink: RoutedSinkId { route, ordinal: 0 },
                    target: RouteTarget::Connection(ConnectionId::External {
                        instance: InstanceId(1),
                        input_index: 0,
                    }),
                    root: at,
                    path: vec![at],
                    terminal: TerminalRecord {
                        sink: RoutedSinkId { route, ordinal: 0 },
                        at,
                        state: state(BlockKind::Repeater, "minecraft:repeater"),
                        kind: crate::compile::planner::RouteTerminalKind::RepeaterIntoSupport,
                        repeaters: 1,
                        delayed_owner: Some(DelayedOwner::Route(route)),
                    },
                }],
            },
        );

        assert!(matches!(
            candidate.validate_physical_ownership(),
            Err(CandidateError::DuplicateDelayedOwner { at: duplicate, .. }) if duplicate == at
        ));
    }

    #[test]
    fn boundary_realisations_emit_and_participate_in_candidate_identity() {
        let mut candidate = two_gate_candidate();
        let endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
        let at = Anchor { x: 1, y: 1, z: 1 };
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: Some(DelayedComponent {
                    at,
                    owner: DelayedOwner::InputBinding(PortId(0)),
                }),
                blocks: vec![
                    PlacedBlock {
                        at,
                        state: state(BlockKind::Repeater, "minecraft:repeater"),
                    },
                    PlacedBlock {
                        at: Anchor { x: 2, y: 1, z: 1 },
                        state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                    },
                ],
            },
        );
        let fingerprint = candidate.fingerprint();

        let mut wrong_owner = candidate.clone();
        wrong_owner
            .boundaries
            .get_mut(&endpoint)
            .unwrap()
            .delayed
            .as_mut()
            .unwrap()
            .owner = DelayedOwner::InputBinding(PortId(1));
        assert!(matches!(
            wrong_owner.validate_physical_ownership(),
            Err(CandidateError::DelayedOwnerMismatch { .. })
        ));

        assert_eq!(
            candidate
                .emit_world_unchecked((4, 4, 4))
                .unwrap()
                .get(at.x, at.y, at.z)
                .kind,
            BlockKind::Repeater
        );
        candidate.boundaries.clear();
        assert_ne!(candidate.fingerprint(), fingerprint);
    }

    #[test]
    fn pinned_input_contract_binds_caller_cell_handover_and_outside_facing() {
        let mut candidate = two_gate_candidate();
        let caller = Anchor { x: 2, y: 1, z: 3 };
        candidate
            .pins
            .pin("a", caller, crate::redstone::world::block::Facing::North);
        candidate.bind_pin_contracts(&two_gate_netlist()).unwrap();
        let endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
        let pin = candidate.pin_contracts[&endpoint];
        let handover = pin.handover(crate::compile::planner::PortRole::Input);
        let mut repeater = state(BlockKind::Repeater, "minecraft:repeater");
        repeater.facing = Some(pin.toward.opposite());
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: Some(DelayedComponent {
                    at: handover,
                    owner: DelayedOwner::InputBinding(PortId(0)),
                }),
                blocks: vec![PlacedBlock {
                    at: handover,
                    state: repeater,
                }],
            },
        );
        candidate.observations.insert(
            ObservationId::PrimaryInput(PortId(0)),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::PrimaryInput(PortId(0)),
                    at: caller,
                    logical_owner: None,
                    display_label: Some("a".to_string()),
                },
                state: BlockState::air(),
            },
        );
        candidate.validate_pin_contracts().unwrap();

        let mut drifted = candidate.clone();
        drifted.pins.pin(
            "a",
            Anchor { x: 8, y: 1, z: 8 },
            crate::redstone::world::block::Facing::North,
        );
        assert!(drifted.validate_pin_contracts().is_err());

        let mut rotated = candidate;
        rotated.boundaries.get_mut(&endpoint).unwrap().blocks[0]
            .state
            .facing = Some(crate::redstone::world::block::Facing::East);
        assert_eq!(
            rotated.validate_pin_contracts(),
            Err(CandidateError::PinContractMismatch { endpoint })
        );
    }

    #[test]
    fn pinned_input_contract_rejects_a_conductor_on_any_non_handover_face() {
        let mut candidate = two_gate_candidate();
        let caller = Anchor { x: 2, y: 1, z: 3 };
        candidate
            .pins
            .pin("a", caller, crate::redstone::world::block::Facing::North);
        candidate.bind_pin_contracts(&two_gate_netlist()).unwrap();
        let endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
        let pin = candidate.pin_contracts[&endpoint];
        let handover = pin.handover(crate::compile::planner::PortRole::Input);
        let mut repeater = state(BlockKind::Repeater, "minecraft:repeater");
        repeater.facing = Some(pin.toward.opposite());
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: Some(DelayedComponent {
                    at: handover,
                    owner: DelayedOwner::InputBinding(PortId(0)),
                }),
                blocks: vec![
                    PlacedBlock {
                        at: handover,
                        state: repeater,
                    },
                    PlacedBlock {
                        at: Anchor { x: 3, ..caller },
                        state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                    },
                ],
            },
        );
        candidate.observations.insert(
            ObservationId::PrimaryInput(PortId(0)),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::PrimaryInput(PortId(0)),
                    at: caller,
                    logical_owner: None,
                    display_label: Some("a".to_string()),
                },
                state: BlockState::air(),
            },
        );

        assert_eq!(
            candidate.validate_pin_contracts(),
            Err(CandidateError::PinContractMismatch { endpoint })
        );
    }

    #[test]
    fn repeater_primitive_cannot_omit_its_delayed_owner() {
        let mut candidate = two_gate_candidate();
        let primitive = candidate.instances.instances[0]
            .expanded
            .topology
            .primitives[0]
            .id;
        let at = Anchor { x: 3, y: 1, z: 3 };
        candidate.placements.insert(
            primitive,
            PrimitivePlacement {
                id: primitive,
                variant: 0,
                facing: CellFacing::NORTH,
                anchor: at,
                delayed: None,
                blocks: vec![PlacedBlock {
                    at,
                    state: state(BlockKind::Repeater, "minecraft:repeater"),
                }],
            },
        );

        assert!(candidate.validate_physical_ownership().is_err());
    }

    #[test]
    fn incomplete_candidate_cannot_emit_an_all_air_world() {
        let candidate = two_gate_candidate();
        assert!(matches!(
            candidate.emit_world((8, 4, 4)),
            Err(CandidateError::CandidateShapeMismatch {
                collection: "primitive placement"
            })
        ));
    }

    #[test]
    fn fingerprint_canonicalises_block_arenas_and_includes_map_keys() {
        let mut left = two_gate_candidate();
        let primitive = left.instances.instances[0].expanded.topology.primitives[0].id;
        let mut placed = placement(primitive, Anchor { x: 1, y: 1, z: 1 });
        placed.blocks.push(PlacedBlock {
            at: Anchor { x: 2, y: 1, z: 1 },
            state: state(BlockKind::Solid, "minecraft:stone"),
        });
        left.placements.insert(primitive, placed.clone());

        let mut reordered = left.clone();
        reordered
            .placements
            .get_mut(&primitive)
            .unwrap()
            .blocks
            .reverse();
        assert_eq!(left.fingerprint(), reordered.fingerprint());

        let mut wrong_key = left;
        wrong_key.placements.clear();
        wrong_key.placements.insert(
            PrimitiveId {
                instance: InstanceId(99),
                node: primitive.node,
            },
            placed,
        );
        assert_ne!(wrong_key.fingerprint(), reordered.fingerprint());
    }

    #[test]
    fn two_node_buf_keeps_both_primitives_and_its_internal_route() {
        let netlist = Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate {
                name: "buf".to_string(),
                inputs: vec!["a".to_string()],
                output: "y".to_string(),
                kind: crate::compile::topology::GateKind::Buf,
            }],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let instance = graph.instances[0].clone();
        let first = instance.expanded.topology.primitives[0].id;
        let second = instance.expanded.topology.primitives[1].id;
        let external = instance.expanded.topology.connections[0].id;
        let internal = instance.expanded.topology.connections[1].id;
        assert!(matches!(internal, ConnectionId::Internal { .. }));

        let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
        let first_at = Anchor { x: 2, y: 1, z: 1 };
        let second_at = Anchor { x: 5, y: 1, z: 1 };
        candidate
            .placements
            .insert(first, placement(first, first_at));
        candidate
            .placements
            .insert(second, placement(second, second_at));
        let input_endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
        candidate.boundaries.insert(
            input_endpoint,
            BoundaryPlacement {
                endpoint: input_endpoint,
                delayed: None,
                blocks: vec![PlacedBlock {
                    at: Anchor { x: 0, y: 1, z: 1 },
                    state: state(BlockKind::Lever, "minecraft:lever"),
                }],
            },
        );
        let output_endpoint = PhysicalEndpointId::DeclaredOutput(PortId(0));
        let output_lamp_at = Anchor { x: 6, y: 0, z: 1 };
        candidate.boundaries.insert(
            output_endpoint,
            BoundaryPlacement {
                endpoint: output_endpoint,
                delayed: None,
                blocks: vec![PlacedBlock {
                    at: output_lamp_at,
                    state: state(BlockKind::Lamp, "minecraft:redstone_lamp"),
                }],
            },
        );

        let route_spec = [
            (
                external,
                RouteId(0),
                input_endpoint,
                Anchor { x: 0, y: 1, z: 1 },
                vec![Anchor { x: 1, y: 1, z: 1 }],
            ),
            (
                internal,
                RouteId(1),
                PhysicalEndpointId::PrimitiveOutput(first),
                first_at,
                vec![Anchor { x: 3, y: 1, z: 1 }, Anchor { x: 4, y: 1, z: 1 }],
            ),
        ];
        for (connection, route_id, source, root, path_cells) in route_spec {
            let sink = RoutedSinkId {
                route: route_id,
                ordinal: 0,
            };
            let terminal_at = *path_cells.last().unwrap();
            let mut path = vec![root];
            path.extend(path_cells.iter().copied());
            let cells = path_cells
                .iter()
                .copied()
                .map(|at| PlacedBlock {
                    at,
                    state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                })
                .collect();
            candidate.connections.insert(
                connection,
                ConnectionBinding {
                    id: connection,
                    source,
                    landing: PhysicalEndpointId::Landing(connection),
                    route: route_id,
                    sink,
                },
            );
            candidate.routes.insert(
                route_id,
                RealisedRouteTree {
                    id: route_id,
                    source,
                    cells,
                    floors: Vec::new(),
                    branches: vec![RealisedRouteBranch {
                        sink,
                        target: RouteTarget::Connection(connection),
                        root,
                        path,
                        terminal: TerminalRecord {
                            sink,
                            at: terminal_at,
                            state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                            kind:
                                crate::compile::planner::RouteTerminalKind::DirectedDustIntoSupport,
                            repeaters: 0,
                            delayed_owner: None,
                        },
                    }],
                },
            );
        }

        let output_route = RouteId(2);
        let output_sink = RoutedSinkId {
            route: output_route,
            ordinal: 0,
        };
        let output_at = Anchor { x: 6, y: 1, z: 1 };
        candidate.routes.insert(
            output_route,
            RealisedRouteTree {
                id: output_route,
                source: PhysicalEndpointId::PrimitiveOutput(second),
                cells: vec![PlacedBlock {
                    at: output_at,
                    state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                }],
                floors: Vec::new(),
                branches: vec![RealisedRouteBranch {
                    sink: output_sink,
                    target: RouteTarget::DeclaredOutput(PortId(0)),
                    root: second_at,
                    path: vec![second_at, output_at],
                    terminal: TerminalRecord {
                        sink: output_sink,
                        at: output_at,
                        state: state(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
                        kind: crate::compile::planner::RouteTerminalKind::DirectedDustIntoSupport,
                        repeaters: 0,
                        delayed_owner: None,
                    },
                }],
            },
        );

        let observations = [
            (
                ObservationId::PrimaryInput(PortId(0)),
                Anchor { x: 0, y: 1, z: 1 },
                None,
            ),
            (
                ObservationId::PrimitiveOutput(first),
                first_at,
                Some(instance.id),
            ),
            (
                ObservationId::PrimitiveOutput(second),
                second_at,
                Some(instance.id),
            ),
            (
                ObservationId::InstanceOutput(instance.id),
                second_at,
                Some(instance.id),
            ),
            (
                ObservationId::DeclaredOutput(PortId(0)),
                output_lamp_at,
                Some(instance.id),
            ),
        ];
        for (id, at, owner) in observations {
            candidate.observations.insert(
                id,
                VerifiedObservation {
                    site: ObservationSite {
                        id,
                        at,
                        logical_owner: owner,
                        display_label: None,
                    },
                    state: candidate
                        .placements
                        .values()
                        .flat_map(|placement| placement.blocks.iter())
                        .chain(
                            candidate
                                .boundaries
                                .values()
                                .flat_map(|boundary| boundary.blocks.iter()),
                        )
                        .find(|block| block.at == at)
                        .map(|block| block.state.clone())
                        .unwrap_or_else(|| {
                            state(BlockKind::RedstoneWire, "minecraft:redstone_wire")
                        }),
                },
            );
        }

        candidate.validate_shape().unwrap();
        candidate.emit_world((8, 4, 4)).unwrap();
        assert_eq!(candidate.placements.len(), 2);
        assert!(candidate.connections.contains_key(&internal));

        let mut forged_timing = candidate.clone();
        let forged_branch = &mut forged_timing.routes.get_mut(&RouteId(1)).unwrap().branches[0];
        forged_branch.root = forged_branch.terminal.at;
        forged_branch.path = vec![forged_branch.terminal.at];
        forged_branch.terminal.repeaters = 10_000;
        assert!(matches!(
            forged_timing.validate_shape(),
            Err(CandidateError::RouteSourceMismatch { .. })
                | Err(CandidateError::RouteTimingMismatch { .. })
        ));

        let mut extreme_coordinate = candidate.clone();
        let extreme_branch = &mut extreme_coordinate
            .routes
            .get_mut(&RouteId(1))
            .unwrap()
            .branches[0];
        extreme_branch.root.x = i32::MIN;
        extreme_branch.path = vec![extreme_branch.root];
        extreme_branch.terminal.at = extreme_branch.root;
        assert!(matches!(
            extreme_coordinate.validate_shape(),
            Err(CandidateError::RouteSourceMismatch { .. })
                | Err(CandidateError::RouteTimingMismatch { .. })
        ));

        let mut missing_output = candidate.clone();
        missing_output.routes.remove(&output_route);
        assert!(matches!(
            missing_output.validate_shape(),
            Err(CandidateError::CandidateShapeMismatch {
                collection: "declared output route"
            })
        ));

        let mut forged_source = candidate.clone();
        forged_source.connections.get_mut(&internal).unwrap().source = input_endpoint;
        assert!(matches!(
            forged_source.validate_shape(),
            Err(CandidateError::CandidateShapeMismatch {
                collection: "connection source"
            })
        ));

        let mut forged_landing = candidate.clone();
        forged_landing
            .connections
            .get_mut(&internal)
            .unwrap()
            .landing = input_endpoint;
        assert!(matches!(
            forged_landing.validate_shape(),
            Err(CandidateError::CandidateShapeMismatch {
                collection: "connection landing"
            })
        ));

        let mut forged_observation = candidate;
        forged_observation
            .observations
            .get_mut(&ObservationId::DeclaredOutput(PortId(0)))
            .unwrap()
            .site
            .id = ObservationId::PrimaryInput(PortId(0));
        assert!(matches!(
            forged_observation.validate_shape(),
            Err(CandidateError::CandidateShapeMismatch {
                collection: "observation identity"
            })
        ));
    }
}
