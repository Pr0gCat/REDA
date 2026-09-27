//! Parent-owned composition: translate certified child worlds into one
//! global world, build the root terminal hardware, and route one trunk per
//! boundary signal through the corridor with the durable physical router.
//!
//! Ownership at a child portal follows the planner's pin contract.  The
//! child owns its handover repeater one cell south of the caller cell; the
//! parent owns the caller cell itself and everything north of it.  A trunk
//! that feeds a child ends with a repeater in the caller cell driving the
//! child's repeater; a trunk fed by a child starts as dust in the caller
//! cell driven by the child's repeater.  Root caller cells on the plan's
//! `caller_row_z` row stay external: the parent builds a south-facing repeater
//! in each root input's handover cell and lets the router place each root
//! output's terminal repeater in its handover cell, exactly as the seed does
//! for a pinned port.
//!
//! Child cells, pending terminal exits, and every trunk already laid are
//! reserved before a route runs; every routed cell is then checked against
//! the corridor, so a trunk can neither couple to a child nor leave the
//! parent's space.  No wire is drawn by hand.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::fabric::{plan_clash, plan_fabric, FabricEnd, FabricError, FabricTrunk};
use crate::compile::fragment_synth::allocation::{
    AllocationPlan, Corridor, Prism, RootPort, SignalContract, TrunkEnd, TrunkOwner,
    ACCESS_HALF_WIDTH, MIN_CORRIDOR_DEPTH,
};
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId,
    TopologyNodeId,
};
use crate::compile::fragment_synth::leaf::{
    FreeLeafInterfaceId, LeafArtifact, ParentConnectableInterface,
};
use crate::compile::fragment_synth::packing::{PackedChildWorld, PackedFreeLeaf, PackedFreeLeaves};
use crate::compile::fragment_synth::partition::ChunkId;
use crate::compile::fragment_synth::terminal_geometry::{
    terminal_access_cells, terminal_access_cells_from, terminal_egress_clearance,
    terminal_egress_closure, terminal_egress_path, terminal_guard_cells, terminal_mouth_ring,
    TERMINAL_RUNWAY_CELLS,
};
use crate::compile::geometry::Anchor;
use crate::compile::planner::PortRole;
use crate::compile::routing::{
    AirKeepOut, ForcedTerminalRunways, IsolatingOwners, NonEmptyRouteSinks, PhysicalReservationKind,
    PhysicalReservationOwner,
    PlannedBranch,
    PhysicalReservations, PhysicalRouter, RealisedRouteTree, RouteEndpoint, RouteGuidance,
    RouteRequest, RouteSink, RouteTarget, RouteTerminalKind, RouterFailure, RouterLimits,
    TerminalContract, TerminalRequirement, TerminalRunway,
};
use crate::compile::routing::{
    keep_out_typed, plan_riser, staircase_clearance, RiserFlow, RiserRequest, RiserStep,
};
use crate::compile::{repeater, stone};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

#[derive(Debug, Clone)]
pub struct ComposedCircuit {
    pub world: World,
    /// One realised tree per plan trunk, in plan order.
    pub trunks: Vec<RealisedRouteTree>,
}

/// One parent-owned tree between packed, independently certified leaves.
///
/// Requests deliberately carry stable child interface identities rather than
/// physical locations.  The latter are derived only after packing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedTrunkRequest {
    pub signal: String,
    pub source: FreeLeafInterfaceId,
    pub sinks: Vec<FreeLeafInterfaceId>,
}

/// A node-owned input: caller cell and child sinks have distinct identities.
#[derive(Debug, Clone)]
pub(crate) struct PackedInputTrunkRequest {
    pub port: PortId,
    pub root: RootPort,
    pub contract: SignalContract,
    pub sinks: Vec<FreeLeafInterfaceId>,
}

/// A node-owned output that a child drives and other children also read: the
/// signal's own trunk gains one more sink, a node-built handover the caller
/// reads, so the caller cell and the trunk's source are distinct cells.
#[derive(Debug, Clone)]
pub(crate) struct PackedOutputExport {
    pub port: PortId,
    pub root: RootPort,
}

/// The one shared packed world and every route tree committed into it.
#[derive(Debug, Clone)]
pub(crate) struct PackedTrunks {
    pub world: World,
    pub routes: Vec<RealisedRouteTree>,
    /// The boundary signal each route carries, in the same stable order as
    /// `routes`: the identity a measurement ties a trunk hop back to.
    pub signals: Vec<String>,
    /// Lane height per route, in stable request order. A single route needs no
    /// lane; competing routes each receive a distinct guided lane.
    pub lanes: Vec<Option<i32>>,
}

/// **Vertical pitch between packed trunk lanes.**
///
/// Derived from the widest reach any authority in this crate claims around a
/// conductor, not chosen. [`keep_out_typed`](crate::compile::routing) claims
/// each horizontal neighbour at `+/-1` in `y`; the leaf halo's two-hop
/// coupling authority claims the whole `L1` ball of radius two. Two
/// conductors sharing a column at `dy = 3` are outside both, so three is the
/// smallest separation that cannot couple under either rule. Anything smaller
/// would be two trunks that merely look separate.
pub(crate) const PACKED_LANE_PITCH: i32 = 3;

const PACKED_TERMINAL_GUARD_KEEP_OUT: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 5);

/// Every child's halo: the two-hop ball its conductors must stay outside.
/// Isolation only, so a route may lay an inert floor in it.
const PACKED_HALO_KEEP_OUT: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 4);

/// **What a packed trunk contracts for at both ends.**
///
/// A packed terminal releases its three straight core columns and nothing
/// else: the lateral columns and the `+/-Y` coupling ring stay keep-out,
/// because a foreign conductor there couples into the certified child. So the
/// only route out of a packed source is straight down its own runway, and the
/// only way into a packed sink is straight up the mirror image -- which is
/// what this says to the router, in the router's own vocabulary, instead of
/// leaving the search to rediscover it one blocked turn at a time.
///
/// Built from [`TERMINAL_RUNWAY_CELLS`], the same constant
/// `terminal_access_cells` projects the released columns from, so the contract
/// and the cells it is contracting over cannot drift apart.
const PACKED_TERMINAL_RUNWAYS: ForcedTerminalRunways = ForcedTerminalRunways {
    source: TerminalRunway::Forced {
        cells: TERMINAL_RUNWAY_CELLS,
    },
    sinks: TerminalRunway::Forced {
        cells: TERMINAL_RUNWAY_CELLS,
    },
};

#[derive(Debug, Error, Clone, PartialEq)]
pub(crate) enum PackedConnectionError {
    #[error("the lid fabric cannot plan this node: {0}")]
    Fabric(FabricError),
    #[error("the lid fabric's plan for this layout breaks its isolation at {at:?}")]
    FabricClash { at: Anchor },
    #[error("packed child {chunk:?} has no placement")]
    MissingPlacement { chunk: ChunkId },
    #[error("packed child {chunk:?} has no interface {endpoint:?}")]
    MissingInterface {
        chunk: ChunkId,
        endpoint: PhysicalEndpointId,
    },
    #[error("packed connection source {endpoint:?} has role {actual:?}, expected Output")]
    InvalidSourceRole {
        endpoint: PhysicalEndpointId,
        actual: PortRole,
    },
    #[error("packed connection sink {endpoint:?} has role {actual:?}, expected Input")]
    InvalidSinkRole {
        endpoint: PhysicalEndpointId,
        actual: PortRole,
    },
    #[error("packed connection joins source signal {source_signal} to sink signal {sink}")]
    SignalMismatch { source_signal: String, sink: String },
    #[error("packed connection has incompatible contracts")]
    ContractMismatch,
    #[error("packed trunk request for {request_signal} names source signal {source_signal}")]
    RequestSignalMismatch {
        request_signal: String,
        source_signal: String,
    },
    #[error("packed trunk signal {signal} occurs more than once")]
    DuplicateSignal { signal: String },
    #[error("packed trunk {signal} has no sinks")]
    NoSinks { signal: String },
    #[error("packed interface {endpoint:?} is consumed more than once")]
    MultipleConsumption { endpoint: PhysicalEndpointId },
    #[error("packed {role:?} interface has invalid local endpoint {endpoint:?}")]
    InvalidLocalEndpoint {
        role: PortRole,
        endpoint: PhysicalEndpointId,
    },
    #[error("packed child index cannot fit in a physical endpoint id")]
    EndpointIndexOverflow,
    #[error("packed connection source and sink resolve to the same endpoint {endpoint:?}")]
    EndpointCollision { endpoint: PhysicalEndpointId },
    #[error("packed root endpoint {endpoint:?} is also consumed by a trunk")]
    RootEndpointConsumed { endpoint: PhysicalEndpointId },
    #[error("packed root {root:?} guards {at:?}, which is a trunk endpoint's own core")]
    RootGuardBlocksEndpoint {
        root: PhysicalEndpointId,
        at: Anchor,
    },
    #[error(
        "packed endpoints {first:?} and {second:?} of different trunks both need {at:?} to leave their runways"
    )]
    EndpointEgressConflict {
        first: PhysicalEndpointId,
        second: PhysicalEndpointId,
        at: Anchor,
    },
    #[error("banded trunk {signal} at {at:?} stands within two cells of {near:?}")]
    BandIsolation {
        signal: String,
        at: Anchor,
        near: Anchor,
    },
    #[error("packed endpoint {endpoint:?} runway at {at:?} is guarded by sibling {sibling:?}")]
    EndpointRunwayBlocked {
        endpoint: PhysicalEndpointId,
        at: Anchor,
        sibling: ChunkId,
    },
    #[error("packed endpoint {endpoint:?} core cell {at:?} is not declared as leaf access")]
    EndpointAccessMissing {
        endpoint: PhysicalEndpointId,
        at: Anchor,
    },
    #[error("packed child world does not match packed occupied cells")]
    ChildWorldMismatch,
    #[error("packed route canvas has no cells")]
    EmptyCanvas,
    #[error("packed route canvas includes negative coordinate {at:?}")]
    NegativeCanvas { at: Anchor },
    #[error("packed route coordinate {at:?} is outside finite canvas")]
    RouteEscaped { at: Anchor },
    #[error("packed route block {at:?} overlaps an existing block")]
    Overlap { at: Anchor },
    #[error("packed route floor at {at:?} collides with {kind:?}")]
    InvalidFloor { at: Anchor, kind: BlockKind },
    #[error("packed connection {signal} from {source:?} could not be routed: {failure}")]
    Route {
        signal: String,
        /// The refused request's own identity: which child owns the source and
        /// which of its endpoints it is.  A caller that packs many nodes reads
        /// a bare signal name as ambiguous -- two nodes can both have an `a` --
        /// and a stable [`ChunkId`] is the only thing that is not.
        source: Option<FreeLeafInterfaceId>,
        #[source]
        failure: RouterFailure,
    },
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ComposeError {
    #[error("artifact for chunk {chunk:?} appears more than once")]
    DuplicateArtifact { chunk: ChunkId },
    #[error("the plan allocates chunk {chunk:?} but no artifact was given")]
    MissingArtifact { chunk: ChunkId },
    #[error("artifact for chunk {chunk:?} has no allocation in the plan")]
    UnexpectedArtifact { chunk: ChunkId },
    #[error("translating chunk {chunk:?} overflows i32")]
    CoordinateOverflow { chunk: ChunkId },
    #[error("chunk {chunk:?} cell lands at {at:?}, outside its allocated region")]
    Escape { chunk: ChunkId, at: Anchor },
    #[error("{at:?} is already occupied")]
    Overlap { at: Anchor },
    #[error("parent cell {at:?} is outside the composed world")]
    OutOfBounds { at: Anchor },
    #[error("route floor at {at:?} collides with {kind:?}")]
    InvalidFloor { at: Anchor, kind: BlockKind },
    #[error("parent corridor is only {depth} cells deep; at least {minimum} are required")]
    CorridorTooShallow { depth: i32, minimum: i32 },
    #[error("trunk {signal} needs lane {lane}, but the corridor has {lanes} lanes in {band:?}")]
    CorridorLanesExhausted {
        signal: String,
        lane: u32,
        lanes: u32,
        band: Option<(i32, i32)>,
    },
    #[error("trunk {signal} end has no portal on its child")]
    MissingPortal { signal: String },
    #[error("trunk {signal} has no sink")]
    NoSinks { signal: String },
    #[error("trunk {signal} could not be routed: {failure}")]
    Route {
        signal: String,
        #[source]
        failure: RouterFailure,
    },
    #[error("trunk {signal} routed through {at:?}, outside the corridor")]
    RouteEscaped { signal: String, at: Anchor },
}

fn place(world: &mut World, at: Anchor, state: BlockState) -> Result<(), ComposeError> {
    if world.index(at.x, at.y, at.z).is_none() {
        return Err(ComposeError::OutOfBounds { at });
    }
    if world.get(at.x, at.y, at.z).kind != BlockKind::Air {
        return Err(ComposeError::Overlap { at });
    }
    world.set(at.x, at.y, at.z, state);
    Ok(())
}

/// One trunk end resolved to the router's vocabulary.
#[derive(Debug)]
struct Terminal {
    endpoint: PhysicalEndpointId,
    /// Cell the route starts at or terminates in.
    anchor: Anchor,
    /// Toward the corridor: a source's exit, a sink's entry.
    facing: Facing,
    /// Sink only: the cell the terminal drives.
    support: Anchor,
    target: Option<RouteTarget>,
}

fn resolve(plan: &AllocationPlan, signal: &str, end: &TrunkEnd) -> Result<Terminal, ComposeError> {
    let missing = || ComposeError::MissingPortal {
        signal: signal.to_owned(),
    };
    match (&end.owner, end.role) {
        (TrunkOwner::Root, role) => {
            let index = plan
                .root_ports
                .iter()
                .position(|port| port.signal == signal && port.role == role)
                .ok_or_else(missing)?;
            let port = PortId(u32::try_from(index).map_err(|_| missing())?);
            let handover = end.pin.handover(role);
            Ok(match role {
                PortRole::Input => Terminal {
                    endpoint: PhysicalEndpointId::PrimaryInput(port),
                    anchor: end.pin.net_cell(role),
                    facing: end.pin.toward,
                    support: handover,
                    target: None,
                },
                PortRole::Output => Terminal {
                    endpoint: PhysicalEndpointId::DeclaredOutput(port),
                    anchor: handover,
                    facing: end.pin.toward.opposite(),
                    support: end.pin.at,
                    target: Some(RouteTarget::DeclaredOutput(port)),
                },
            })
        }
        (TrunkOwner::Child(chunk), role) => {
            let child_index = plan
                .children
                .iter()
                .position(|child| &child.chunk == chunk)
                .ok_or_else(missing)?;
            let child = &plan.children[child_index];
            let portal_index = child
                .portals
                .iter()
                .position(|portal| portal.signal == signal && portal.role == role)
                .ok_or_else(missing)?;
            let instance = InstanceId(u32::try_from(child_index).map_err(|_| missing())?);
            let slot = u16::try_from(portal_index).map_err(|_| missing())?;
            let handover = end.pin.handover(role);
            Ok(match role {
                PortRole::Output => Terminal {
                    endpoint: PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                        instance,
                        node: TopologyNodeId(slot),
                    }),
                    anchor: end.pin.at,
                    facing: end.pin.toward,
                    support: handover,
                    target: None,
                },
                PortRole::Input => {
                    let connection = ConnectionId::External {
                        instance,
                        input_index: slot,
                    };
                    Terminal {
                        endpoint: PhysicalEndpointId::Landing(connection),
                        anchor: end.pin.at,
                        facing: end.pin.toward.opposite(),
                        support: handover,
                        target: Some(RouteTarget::Connection(connection)),
                    }
                }
            })
        }
    }
}

/// Route packed leaf trunks in stable signal/source order.  All children,
/// endpoints, halos, and already committed trees share one world and one
/// reservation map; each search sees only its own endpoint guards released.
pub(crate) fn route_packed_trunks(
    children: &PackedChildWorld,
    packed: &PackedFreeLeaves,
    requests: &[PackedTrunkRequest],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
) -> Result<PackedTrunks, PackedConnectionError> {
    route_packed_trunks_with_root_guards(children, packed, requests, &[], router, limits)
}

/// [`route_packed_trunks`], with `roots` reserved for a caller that is not
/// this node.
///
/// A root interface is a cell this composition deliberately leaves unrouted so
/// that whoever packs *this* node can reach it.  It therefore needs exactly
/// what a trunk terminal needs and one thing more: its guard column must stand
/// for the whole run, through the canvas top rather than only to its own
/// leaf's halo top, and it is never released.  The leaf's own halo already
/// covers the column up to that leaf's height; a taller sibling raises the
/// canvas above it, and the cells in between are what a trunk would otherwise
/// be free to cross.
///
/// Reserving them is one line -- the roots join `terminals` -- because a root
/// terminal differs from a trunk terminal only in never being opened: the
/// access reservation that makes a trunk endpoint releasable is not taken for
/// a root, so its core columns stay keep-out like the lateral guards around
/// them.
pub(crate) fn route_packed_trunks_with_root_guards(
    children: &PackedChildWorld,
    packed: &PackedFreeLeaves,
    requests: &[PackedTrunkRequest],
    roots: &[FreeLeafInterfaceId],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
) -> Result<PackedTrunks, PackedConnectionError> {
    route_packed_trunks_with_inputs(children, packed, requests, roots, &[], &[], router, limits)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn route_packed_trunks_with_inputs(
    children: &PackedChildWorld,
    packed: &PackedFreeLeaves,
    requests: &[PackedTrunkRequest],
    roots: &[FreeLeafInterfaceId],
    inputs: &[PackedInputTrunkRequest],
    outputs: &[PackedOutputExport],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
) -> Result<PackedTrunks, PackedConnectionError> {
    route_packed_trunks_on(
        children, packed, requests, roots, inputs, outputs, router, limits, None,
    )
}

/// [`route_packed_trunks_with_inputs`] on the lid fabric
/// ([`super::fabric`]): every trunk planned onto three fixed layers over the
/// children's lid and laid by [`PhysicalRouter::route_planned`], with no lanes
/// and no search. The children must stand with room for their faces' pads and
/// columns between them ([`super::fabric::face_reach`]).
///
/// `feet`, when given, is the x of a column where node-built ports enter the
/// fabric instead of at their own cells: a caller's pin can face any way from
/// anywhere, so each one is joined to its foot by an ordinary search, and the
/// fabric takes it from there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn route_packed_trunks_fabric(
    children: &PackedChildWorld,
    packed: &PackedFreeLeaves,
    requests: &[PackedTrunkRequest],
    roots: &[FreeLeafInterfaceId],
    inputs: &[PackedInputTrunkRequest],
    outputs: &[PackedOutputExport],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
    feet: Option<i32>,
    checked: bool,
) -> Result<PackedTrunks, PackedConnectionError> {
    route_packed_trunks_on(
        children,
        packed,
        requests,
        roots,
        inputs,
        outputs,
        router,
        limits,
        Some((feet, checked)),
    )
}

#[allow(clippy::too_many_arguments)]
fn route_packed_trunks_on(
    children: &PackedChildWorld,
    packed: &PackedFreeLeaves,
    requests: &[PackedTrunkRequest],
    roots: &[FreeLeafInterfaceId],
    inputs: &[PackedInputTrunkRequest],
    outputs: &[PackedOutputExport],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
    fabric: Option<(Option<i32>, bool)>,
) -> Result<PackedTrunks, PackedConnectionError> {
    let mut requests = requests.to_vec();
    requests.sort_by(|left, right| {
        (&left.signal, &left.source, &left.sinks).cmp(&(&right.signal, &right.source, &right.sinks))
    });
    for request in &mut requests {
        request.sinks.sort();
        // A trunk whose only reader is an exported output gains its sink
        // below, from the export.
        let exported = outputs
            .iter()
            .any(|output| output.root.signal == request.signal);
        if request.sinks.is_empty() && !exported {
            return Err(PackedConnectionError::NoSinks {
                signal: request.signal.clone(),
            });
        }
    }
    for pair in requests.windows(2) {
        if pair[0].signal == pair[1].signal {
            return Err(PackedConnectionError::DuplicateSignal {
                signal: pair[0].signal.clone(),
            });
        }
    }

    let mut resolved = Vec::with_capacity(requests.len());
    let mut consumed = BTreeSet::new();
    for request in requests {
        let (source_leaf, source_interface, source_index) =
            packed_interface(packed, &request.source)?;
        if source_interface.role != PortRole::Output {
            return Err(PackedConnectionError::InvalidSourceRole {
                endpoint: request.source.endpoint,
                actual: source_interface.role,
            });
        }
        if source_interface.signal != request.signal {
            return Err(PackedConnectionError::RequestSignalMismatch {
                request_signal: request.signal,
                source_signal: source_interface.signal.clone(),
            });
        }
        let source = packed_terminal(
            source_leaf,
            source_interface,
            source_index,
            request.source.endpoint,
        )?;
        if !consumed.insert(source.endpoint) {
            return Err(PackedConnectionError::MultipleConsumption {
                endpoint: source.endpoint,
            });
        }
        let mut sinks = Vec::with_capacity(request.sinks.len());
        for sink_id in &request.sinks {
            let (sink_leaf, sink_interface, sink_index) = packed_interface(packed, sink_id)?;
            if sink_interface.role != PortRole::Input {
                return Err(PackedConnectionError::InvalidSinkRole {
                    endpoint: sink_id.endpoint,
                    actual: sink_interface.role,
                });
            }
            if source_interface.signal != sink_interface.signal {
                return Err(PackedConnectionError::SignalMismatch {
                    source_signal: source_interface.signal.clone(),
                    sink: sink_interface.signal.clone(),
                });
            }
            if source_interface.contract != sink_interface.contract {
                return Err(PackedConnectionError::ContractMismatch);
            }
            let sink = packed_terminal(sink_leaf, sink_interface, sink_index, sink_id.endpoint)?;
            if sink.endpoint == source.endpoint {
                return Err(PackedConnectionError::EndpointCollision {
                    endpoint: sink.endpoint,
                });
            }
            if !consumed.insert(sink.endpoint) {
                return Err(PackedConnectionError::MultipleConsumption {
                    endpoint: sink.endpoint,
                });
            }
            sinks.push((Some(sink_id.clone()), sink));
        }
        resolved.push(ResolvedPackedTrunk {
            signal: source_interface.signal.clone(),
            strength: source_interface.contract.strength,
            source_id: Some(request.source),
            source,
            sinks,
        });
    }

    let mut root_terminals = Vec::with_capacity(roots.len() + inputs.len());
    for id in roots {
        let (leaf, interface, index) = packed_interface(packed, id)?;
        let terminal = packed_terminal(leaf, interface, index, id.endpoint)?;
        // A root end is the caller's. A request that also consumes it would be
        // routing a cell this node has already promised away.
        if consumed.contains(&terminal.endpoint) {
            return Err(PackedConnectionError::RootEndpointConsumed {
                endpoint: terminal.endpoint,
            });
        }
        root_terminals.push((Some(id.clone()), terminal));
    }

    for input in inputs {
        if input.sinks.is_empty() {
            return Err(PackedConnectionError::NoSinks {
                signal: input.root.signal.clone(),
            });
        }
        let pin = input.root.pin;
        let handover = pin.handover(PortRole::Input);
        let source = Terminal {
            endpoint: PhysicalEndpointId::PrimaryInput(input.port),
            anchor: step(handover, pin.toward),
            facing: pin.toward,
            support: handover,
            target: None,
        };
        let mut sinks = Vec::new();
        let mut ids = input.sinks.clone();
        ids.sort();
        for id in ids {
            let (leaf, interface, index) = packed_interface(packed, &id)?;
            if interface.role != PortRole::Input {
                return Err(PackedConnectionError::InvalidSinkRole {
                    endpoint: id.endpoint,
                    actual: interface.role,
                });
            }
            if interface.signal != input.root.signal {
                return Err(PackedConnectionError::SignalMismatch {
                    source_signal: input.root.signal.clone(),
                    sink: interface.signal.clone(),
                });
            }
            if interface.contract != input.contract {
                return Err(PackedConnectionError::ContractMismatch);
            }
            let sink = packed_terminal(leaf, interface, index, id.endpoint)?;
            if !consumed.insert(sink.endpoint) {
                return Err(PackedConnectionError::MultipleConsumption {
                    endpoint: sink.endpoint,
                });
            }
            sinks.push((Some(id), sink));
        }
        root_terminals.push((
            None,
            Terminal {
                endpoint: PhysicalEndpointId::PrimaryInput(input.port),
                anchor: pin.at,
                facing: pin.toward.opposite(),
                support: handover,
                target: None,
            },
        ));
        resolved.push(ResolvedPackedTrunk {
            signal: input.root.signal.clone(),
            strength: input.contract.strength,
            source_id: None,
            source,
            sinks,
        });
    }
    // An exported output rides its own trunk: the node-built handover is one
    // more sink, entered from its rear the way a child input's handover is,
    // and the caller cell in front of it is a root end like any other.
    for output in outputs {
        let pin = output.root.pin;
        let endpoint = PhysicalEndpointId::DeclaredOutput(output.port);
        let handover = pin.handover(PortRole::Output);
        let trunk = resolved
            .iter_mut()
            .find(|trunk| trunk.source_id.is_some() && trunk.signal == output.root.signal)
            .ok_or_else(|| PackedConnectionError::NoSinks {
                signal: output.root.signal.clone(),
            })?;
        if !consumed.insert(endpoint) {
            return Err(PackedConnectionError::MultipleConsumption { endpoint });
        }
        trunk.sinks.push((
            None,
            Terminal {
                endpoint,
                anchor: pin.net_cell(PortRole::Output),
                facing: pin.toward.opposite(),
                support: handover,
                target: Some(RouteTarget::DeclaredOutput(output.port)),
            },
        ));
        root_terminals.push((
            None,
            Terminal {
                endpoint,
                anchor: pin.at,
                facing: pin.toward,
                support: handover,
                target: None,
            },
        ));
    }
    resolved.sort_by(|a, b| a.signal.cmp(&b.signal));
    for pair in resolved.windows(2) {
        if pair[0].signal == pair[1].signal {
            return Err(PackedConnectionError::DuplicateSignal {
                signal: pair[0].signal.clone(),
            });
        }
    }

    let terminals = resolved
        .iter()
        .flat_map(|trunk| {
            std::iter::once(&trunk.source).chain(trunk.sinks.iter().map(|(_, sink)| sink))
        })
        .chain(root_terminals.iter().map(|(_, terminal)| terminal))
        .collect::<Vec<_>>();
    let canvas = packed_canvas(packed, &terminals)?;
    // **The lid is the children's.** A port this node built at a caller's pin
    // may stand far above every child -- a wall of outputs -- and must lift no
    // lane, guard or fabric layer with it; the canvas still holds it.
    let child_terminals = resolved
        .iter()
        .flat_map(|trunk| {
            trunk
                .source_id
                .is_some()
                .then_some(&trunk.source)
                .into_iter()
                .chain(trunk.sinks.iter().filter(|(id, _)| id.is_some()).map(|(_, sink)| sink))
        })
        .chain(root_terminals.iter().filter(|(id, _)| id.is_some()).map(|(_, terminal)| terminal))
        .collect::<Vec<_>>();
    let lid = packed_canvas(packed, &child_terminals)?.max.y;
    // Node-built ports on more than one layer -- pins stacked on a wall --
    // each open only their own layer and one either side: a column up from
    // the floor would hand the lower pin the upper one's runway.
    let layered = resolved
        .iter()
        .flat_map(|trunk| {
            std::iter::once((trunk.source_id.is_none(), trunk.source.anchor.y))
                .chain(trunk.sinks.iter().map(|(id, sink)| (id.is_none(), sink.anchor.y)))
        })
        .filter(|(built, _)| *built)
        .map(|(_, y)| y)
        .collect::<BTreeSet<_>>()
        .len()
        > 1;
    // **Where a terminal's guard column stops, and where its core opens to.**
    //
    // A guard exists to keep a foreign conductor out of the reach a certified
    // child claims around its own terminal -- the two-hop coupling ball, which
    // is exactly what the packed halo already spans. `packed_canvas` is that
    // span, so it is the right lid, and it is the lid the guards had before
    // lanes existed.
    //
    // Raising it with the lanes would extend every terminal's column up
    // through the lane airspace, and that is not isolation but a roof: the
    // second trunk into a leaf has to cross over the first terminal's column
    // to reach its own, and a guard that reaches the ceiling leaves it
    // nowhere to cross. The headroom above the halos is free space no child
    // claims, so nothing is released by stopping here -- the cells that
    // matter are guarded exactly as they always were.
    //
    // The same top is what every terminal's *core* is opened through. A leaf
    // packed beside a taller sibling declares access only to its own halo
    // top; the guard column above that, up to this lid, is the parent's, and
    // it is the parent that must open it for the terminal's own search or the
    // trunk has no way up out of a shorter leaf. One value, read once, so the
    // guard, the validation, the reservation and every release agree.
    let guard_top = lid;
    let mut trunk_access = BTreeSet::new();
    for trunk in &resolved {
        if let Some(id) = &trunk.source_id {
            validate_packed_terminal_runway(packed, id, &trunk.source, guard_top)?;
            validate_packed_terminal_access(packed, id, &trunk.source)?;
        }
        trunk_access.extend(trunk.source_access(packed, guard_top, layered)?);
        for (sink_id, sink) in &trunk.sinks {
            if let Some(sink_id) = sink_id {
                validate_packed_terminal_runway(packed, sink_id, sink, guard_top)?;
                validate_packed_terminal_access(packed, sink_id, sink)?;
            }
            trunk_access.extend(sink_access(packed, sink_id.as_ref(), sink, guard_top, layered)?);
        }
    }
    for (id, terminal) in &root_terminals {
        if let Some(id) = id {
            validate_packed_terminal_runway(packed, id, terminal, guard_top)?;
            validate_packed_terminal_access(packed, id, terminal)?;
        }
        // A root guard that stood on a trunk endpoint's own core would shut
        // that endpoint before its search began. Saying so is better than
        // letting the router report an unreachable terminal.
        for at in guard_cells(terminal, guard_top.max(terminal.anchor.y + 1)) {
            if trunk_access.contains(&at) {
                return Err(PackedConnectionError::RootGuardBlocksEndpoint {
                    root: terminal.endpoint,
                    at,
                });
            }
        }
    }

    // **Band mode or lanes.** When the packer left every trunk of this node
    // an explicit band between its two halos, the trunks cross at terminal
    // height inside it -- one layer, or two a pitch apart when their order
    // inverts -- and the lane airspace over the halo lid is never built.
    // Otherwise every trunk gets a lane over the halo lid, shared with the
    // trunks it cannot meet (`shared_lanes`). One decision for the whole node,
    // so a node is either banded or laned, never both.
    // **The fabric.** Every trunk planned onto fixed layers over the lid: no
    // band, no lanes, and a canvas that holds the plan.
    let fabric_plan = fabric
        .map(|(feet, checked)| {
            // A node-built port at a caller's pin enters at its foot: one
            // row each, in the order of the pins along z.
            let mut pins = resolved
                .iter()
                .flat_map(|trunk| {
                    std::iter::once((trunk.source_id.is_none(), trunk.source.anchor))
                        .chain(trunk.sinks.iter().map(|(id, sink)| (id.is_none(), sink.anchor)))
                })
                .filter(|(built, _)| *built && feet.is_some())
                .map(|(_, at)| (at.z, at.x, at.y))
                .collect::<Vec<_>>();
            pins.sort();
            pins.dedup();
            let base = packed.halo.iter().map(|at| at.z).min().unwrap_or(0);
            let foot = |at: Anchor| {
                feet.and_then(|x| {
                    let row = pins.iter().position(|pin| *pin == (at.z, at.x, at.y))?;
                    Some(Anchor {
                        x,
                        y: 3,
                        z: base + 3 + 6 * i32::try_from(row).unwrap_or(i32::MAX / 6),
                    })
                })
            };
            let end = |id: Option<&FreeLeafInterfaceId>, terminal: &Terminal| match (id, foot(terminal.anchor)) {
                (None, Some(at)) => FabricEnd {
                    anchor: at,
                    facing: Facing::East,
                    face: None,
                },
                _ => FabricEnd {
                    anchor: terminal.anchor,
                    facing: terminal.facing,
                    face: id.map(|id| id.chunk.clone()),
                },
            };
            let trunks = resolved
                .iter()
                .map(|trunk| FabricTrunk {
                    source: end(trunk.source_id.as_ref(), &trunk.source),
                    sinks: trunk
                        .sinks
                        .iter()
                        .map(|(id, sink)| end(id.as_ref(), sink))
                        .collect(),
                })
                .collect::<Vec<_>>();
            // Pinned spines run inside the room, from its first child row;
            // an unpinned node's from the frame's.
            let row_base = if feet.is_some() { base } else { 1 };
            let plan = plan_fabric(&trunks, lid, row_base).map_err(PackedConnectionError::Fabric)?;
            // A layout tighter than every face's full reach is checked before
            // anything is routed: a clash here is the layout's, not a route's.
            if let Some(at) = checked.then(|| plan_clash(&plan, &packed.halo)).flatten() {
                return Err(PackedConnectionError::FabricClash { at });
            }
            // A branch with a foot at either end searches its own way to it.
            let branches = plan
                .paths
                .iter()
                .zip(&resolved)
                .map(|(paths, trunk)| {
                    let source_foot = trunk.source_id.is_none() && feet.is_some();
                    paths
                        .iter()
                        .zip(&trunk.sinks)
                        .map(|(path, (id, _))| PlannedBranch {
                            path: path.clone(),
                            free_ends: source_foot || (id.is_none() && feet.is_some()),
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            Ok::<_, PackedConnectionError>((plan, branches))
        })
        .transpose()?;
    let band = if fabric.is_some() {
        None
    } else {
        band_plan(packed, &resolved)
    };
    let lanes = if fabric.is_some() {
        vec![None; resolved.len()]
    } else if let Some(plan) = &band {
        plan.layers.iter().map(|layer| Some(*layer)).collect()
    } else if resolved.len() < 2 {
        vec![None; resolved.len()]
    } else {
        shared_lanes(&resolved, lid.saturating_add(1))
    };
    // The canvas must still hold every lane, the floor each lane's dust
    // stands on, and one cell of headroom above the highest -- the `+Y` half
    // of the ring `keep_out_typed` claims around a conductor. The shell is
    // reserved at this extended top, so a route still cannot leave the world.
    let canvas = match lanes.iter().flatten().max() {
        None => canvas,
        Some(top) => PackedCanvas {
            max: Anchor {
                y: canvas.max.y.max(top.saturating_add(1)),
                ..canvas.max
            },
        },
    };
    // The fabric's cells, with a cell of headroom over its top layer.
    let canvas = match &fabric_plan {
        None => canvas,
        Some((plan, _)) => PackedCanvas {
            max: Anchor {
                x: canvas.max.x.max(plan.max.x + 1),
                y: canvas.max.y.max(plan.max.y + 1),
                z: canvas.max.z.max(plan.max.z + 1),
            },
        },
    };

    let mut world = expanded_packed_world(children, canvas)?;
    let mut reservations = PhysicalReservations::new();
    // Children are reserved as the conductors they are; every keep-out below
    // is a halo, guard, lane or ring -- air a route must floor itself. The
    // halo only isolates, so an inert floor may stand in it.
    reservations.treat_keep_out_as_air(AirKeepOut {
        blocks: BTreeMap::new(),
        isolating: IsolatingOwners::Only(BTreeSet::from([PACKED_HALO_KEEP_OUT])),
    });
    reserve_packed_children(&mut reservations, children, packed)?;
    let node_handovers = inputs
        .iter()
        .map(|input| {
            (
                input.root.pin,
                PortRole::Input,
                PhysicalEndpointId::PrimaryInput(input.port),
            )
        })
        .chain(outputs.iter().map(|output| {
            (
                output.root.pin,
                PortRole::Output,
                PhysicalEndpointId::DeclaredOutput(output.port),
            )
        }));
    for (pin, role, endpoint) in node_handovers {
        let at = pin.handover(role);
        let floor = Anchor { y: at.y - 1, ..at };
        for (at, state, kind) in [
            (
                at,
                repeater(pin.toward),
                PhysicalReservationKind::Conductor(repeater(pin.toward)),
            ),
            (floor, stone(), PhysicalReservationKind::Floor(stone())),
        ] {
            place_packed(&mut world, at, state)?;
            reservations.reserve(at, PhysicalReservationOwner::Endpoint(endpoint), kind);
        }
    }
    // Endpoint core access deliberately precedes guard columns and opaque
    // halos. Only these three cells may be released for a route attempt;
    // lateral/ring guards remain held even while their terminal is active.
    for trunk in &resolved {
        for at in trunk.source_access(packed, guard_top, layered)? {
            reservations.reserve(
                at,
                PhysicalReservationOwner::Endpoint(trunk.source.endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
        for (sink_id, sink) in &trunk.sinks {
            for at in sink_access(packed, sink_id.as_ref(), sink, guard_top, layered)? {
                reservations.reserve(
                    at,
                    PhysicalReservationOwner::Endpoint(sink.endpoint),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }
    for terminal in &terminals {
        for at in guard_cells(terminal, guard_top.max(terminal.anchor.y + 1)) {
            reservations.reserve(
                at,
                PACKED_TERMINAL_GUARD_KEEP_OUT,
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    for at in &packed.halo {
        reservations.reserve(
            *at,
            PACKED_HALO_KEEP_OUT,
            PhysicalReservationKind::KeepOut,
        );
    }
    // **Egress, for every pending terminal, up front.** See
    // [`reserve_packed_egress`]: the way from each terminal's runway to its
    // trunk's lane, held for that endpoint until its own search, so no
    // earlier trunk can lay the conductor that would seal it.
    let banded = band.is_some();
    let mut egress = resolved
        .iter()
        .enumerate()
        .flat_map(|(ordinal, trunk)| {
            let lane = lanes[ordinal];
            let corridor_edge = band.as_ref().map(|plan| plan.corridor_edge[ordinal]);
            std::iter::once((&trunk.source, None))
                .chain(
                    trunk
                        .sinks
                        .iter()
                        .map(move |(_, sink)| (sink, corridor_edge)),
                )
                .map(move |(terminal, corridor_edge)| {
                    let mut egress = packed_egress(terminal, lane, canvas);
                    if let (Some(edge), Some(layer)) = (corridor_edge, lane) {
                        extend_with_band_corridor(&mut egress, terminal, layer, edge, canvas);
                    }
                    if banded {
                        // On a band layer the coupling authority is the
                        // two-hop ball, as it is for a child: the closure a
                        // banded terminal holds around everything it may
                        // occupy is that ball, so no earlier trunk can lay a
                        // conductor within two cells of its way out.
                        let owned = egress.owned().collect::<BTreeSet<_>>();
                        for at in owned.iter().copied().collect::<Vec<_>>() {
                            for near in two_hop_ball(at) {
                                if canvas.contains(near) && !owned.contains(&near) {
                                    egress.closure.insert(near);
                                }
                            }
                        }
                    }
                    (ordinal, egress)
                })
        })
        .collect::<Vec<_>>();
    for index in 0..egress.len() {
        let (ordinal, held) = &mut egress[index];
        if let (false, Some(lane)) = (banded, lanes[*ordinal]) {
            coil_egress(&reservations, held, lane, canvas);
        }
        reserve_packed_egress(&mut reservations, &egress[index..=index]);
    }
    let egress_of = |endpoint: PhysicalEndpointId| -> Egress {
        egress
            .iter()
            .find(|(_, e)| e.endpoint == endpoint)
            .map(|(_, e)| e.clone())
            .expect("every resolved terminal has an egress")
    };
    // A root end's egress is nobody's to open. Reserve its occupied cells
    // after every pending trunk path (so paths have precedence), but before
    // any closure (so the root's persistent claim has precedence there).
    // No lane: the route that will leave through it belongs to the parent
    // that packs this node, in that parent's frame.
    for (_, terminal) in &root_terminals {
        let root_egress = packed_egress(terminal, None, canvas);
        for at in root_egress.owned() {
            if let Some(held) = reservations.get(&at) {
                if let PhysicalReservationOwner::Endpoint(first) = held.owner {
                    if egress.iter().any(|(_, pending)| pending.endpoint == first) {
                        return Err(PackedConnectionError::EndpointEgressConflict {
                            first,
                            second: terminal.endpoint,
                            at,
                        });
                    }
                }
            }
        }
        for at in root_egress
            .owned()
            .chain(root_egress.closure.iter().copied())
        {
            reservations.reserve(
                at,
                PACKED_TERMINAL_GUARD_KEEP_OUT,
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    reserve_packed_egress_closures(&mut reservations, &egress);
    reserve_packed_canvas_shell(&mut reservations, canvas);

    // Every planned cell and the floor under it is its own trunk's before any
    // trunk is laid: a search to or from a foot then keeps off every other
    // trunk's plan, and a trunk still passes its own.
    // Then each plan's ring, so a search keeps a trunk's distance from it too;
    // rings of trunks planned apart never meet, and a ring never takes a cell
    // already another plan's.
    if let Some((plan, _)) = &fabric_plan {
        let owners = (0..plan.paths.len())
            .map(|ordinal| {
                u32::try_from(ordinal)
                    .map(|id| PhysicalReservationOwner::Route(RouteId(id)))
                    .map_err(|_| PackedConnectionError::EndpointIndexOverflow)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (ordinal, (paths, owner)) in plan.paths.iter().zip(&owners).enumerate() {
            let stair = PhysicalReservationOwner::RouteStair(RouteId(
                u32::try_from(ordinal).map_err(|_| PackedConnectionError::EndpointIndexOverflow)?,
            ));
            for cell in paths.iter().flatten() {
                reservations.reserve(*cell, *owner, PhysicalReservationKind::KeepOut);
            }
            // The floor under each cell as the route's own floor, which is what
            // the router lays a support on.
            for cell in paths.iter().flatten() {
                reservations.reserve(
                    Anchor { y: cell.y - 1, ..*cell },
                    stair,
                    PhysicalReservationKind::Floor(crate::compile::stone()),
                );
            }
        }
        // A plan's own stairs keep their air, as the router's stairs do.
        for (ordinal, paths) in plan.paths.iter().enumerate() {
            let stair = PhysicalReservationOwner::RouteStair(RouteId(
                u32::try_from(ordinal).map_err(|_| PackedConnectionError::EndpointIndexOverflow)?,
            ));
            for path in paths {
                // The air over the lower dust of each step: above `from` going
                // up, above `to` going down. (The other cell a climb's
                // clearance names is the upper dust's own floor.)
                for pair in path.windows(2) {
                    let (from, to) = (pair[0], pair[1]);
                    let air = match to.y.cmp(&from.y) {
                        std::cmp::Ordering::Greater => Some(Anchor { y: from.y + 1, ..from }),
                        std::cmp::Ordering::Less => Some(Anchor { y: from.y, ..to }),
                        std::cmp::Ordering::Equal => None,
                    };
                    if let Some(at) = air {
                        reservations.reserve(at, stair, PhysicalReservationKind::MandatoryAir);
                    }
                }
            }
        }
        for (paths, owner) in plan.paths.iter().zip(&owners) {
            for cell in paths.iter().flatten() {
                for at in keep_out_typed(*cell) {
                    if canvas.contains(at) {
                        reservations.reserve(at, *owner, PhysicalReservationKind::KeepOut);
                    }
                }
            }
        }
    }
    let mut routes: Vec<RealisedRouteTree> = Vec::with_capacity(resolved.len());
    let mut signals: Vec<String> = Vec::with_capacity(resolved.len());
    for (ordinal, trunk) in resolved.into_iter().enumerate() {
        let route_id = RouteId(
            u32::try_from(ordinal).map_err(|_| PackedConnectionError::EndpointIndexOverflow)?,
        );
        let route_sinks = NonEmptyRouteSinks::new(
            trunk
                .sinks
                .iter()
                .enumerate()
                .map(|(ordinal, (_, sink))| {
                    Ok(RouteSink {
                        id: RoutedSinkId {
                            route: route_id,
                            ordinal: u16::try_from(ordinal)
                                .map_err(|_| PackedConnectionError::EndpointIndexOverflow)?,
                        },
                        endpoint: sink.endpoint,
                        anchor: sink.anchor,
                        allowed_entry: sink.facing,
                        terminal: TerminalContract::Sink {
                            target: sink.target.expect("input terminal has a target"),
                            support: sink.support,
                            requirement: TerminalRequirement::Repeater,
                        },
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
        .expect("validated packed trunk has a sink");
        let endpoints = std::iter::once(trunk.source.endpoint)
            .chain(trunk.sinks.iter().map(|(_, sink)| sink.endpoint))
            .collect::<Vec<_>>();
        // The source's access is this trunk's to search through, so it opens
        // here. Every sink's stays shut: the router releases each sink's own
        // endpoint keep-outs when it starts that sink's branch, so an earlier
        // branch of a fanout cannot wander into a later sink's runway and box
        // it in. Opening all of them here would give exactly that away.
        reservations.begin_attempt();
        trunk.release_source(
            &mut reservations,
            packed,
            guard_top,
            layered,
            &egress_of(trunk.source.endpoint),
        )?;
        // The lateral track is left wide open: what separates these trunks is
        // height, not a corridor, so `half_width` admits every column and the
        // lane is the only rule. With no lane there is no guidance at all, and
        // the call is the one this function has always made.
        let guidance = lanes[ordinal].map(|lane| RouteGuidance {
            origin: trunk.source.anchor,
            lateral: trunk.source.facing,
            track: 0,
            half_width: u32::MAX,
            access_half_width: 0,
            preferred_y: Some(lane),
            access_y: None,
            hard: true,
            penalty_per_block: 1,
        });
        let request = RouteRequest {
            id: route_id,
            source: RouteEndpoint {
                id: trunk.source.endpoint,
                anchor: trunk.source.anchor,
                allowed_exit: trunk.source.facing,
                terminal: TerminalContract::Source {
                    signal_strength: trunk.strength,
                },
            },
            sinks: &route_sinks,
            reservations: &reservations,
            limits,
        };
        let routed = match &fabric_plan {
            Some((_, branches)) => {
                router.route_planned(request, PACKED_TERMINAL_RUNWAYS, &branches[ordinal])
            }
            None => router.route_guided_with_runways(request, guidance, PACKED_TERMINAL_RUNWAYS),
        };
        reservations.rollback_attempt();
        if let (Err(_), Some((_, branches)), true) = (
            &routed,
            &fabric_plan,
            std::env::var_os("REDA_TRACE_FABRIC").is_some(),
        ) {
            trace_fabric_refusal(&trunk.signal, route_id, &branches[ordinal], &reservations);
        }
        if routed.is_err() {
            if std::env::var_os("REDA_TRACE_PINNED").is_some() {
                eprintln!(
                    "reda: trunk {} from {:?} facing {:?} lane {:?} to {:?}",
                    trunk.signal,
                    trunk.source.anchor,
                    trunk.source.facing,
                    lanes[ordinal],
                    trunk.sinks.iter().map(|(_, sink)| (sink.anchor, sink.facing)).collect::<Vec<_>>()
                );
            }
            if let Some(dir) = std::env::var_os("REDA_DUMP_FAILED_TRUNK") {
                dump_failed_trunk(
                    std::path::Path::new(&dir),
                    &world,
                    &egress,
                    &trunk,
                    lanes[ordinal],
                );
            }
        }
        let signal = trunk.signal.clone();
        let route = routed.map_err(|failure| PackedConnectionError::Route {
            signal: trunk.signal.clone(),
            source: trunk.source_id.clone(),
            failure,
        })?;
        for block in route.owned_blocks() {
            if !canvas.contains(block.at) {
                return Err(PackedConnectionError::RouteEscaped { at: block.at });
            }
        }
        let yielding = endpoints
            .iter()
            .copied()
            .map(PhysicalReservationOwner::Endpoint)
            .chain(fabric_plan.is_some().then_some(PhysicalReservationOwner::Route(route.id)))
            .collect::<Vec<_>>();
        for block in &route.cells {
            place_packed(&mut world, block.at, block.state.clone())?;
            if !reservations.commit_routed(
                block.at,
                PhysicalReservationOwner::Route(route.id),
                PhysicalReservationKind::Conductor(block.state.clone()),
                &yielding,
            ) {
                return Err(PackedConnectionError::Overlap { at: block.at });
            }
        }
        for block in &route.floors {
            place_packed_floor(&mut world, block.at, block.state.clone())?;
            if !reservations.commit_routed(
                block.at,
                PhysicalReservationOwner::RouteStair(route.id),
                PhysicalReservationKind::Floor(block.state.clone()),
                &yielding,
            ) {
                return Err(PackedConnectionError::Overlap { at: block.at });
            }
        }
        // **Banded trunks keep the lanes' distance.** Two lanes stood one
        // isolation pitch apart so no conductor of one trunk lay inside the
        // two-hop coupling ball around a conductor of another -- the same
        // reach a leaf halo claims. On a shared band layer nothing in the
        // geometry says that, so the committed trunk claims that ball as
        // keep-out, first-wins: a later trunk routes around it exactly as it
        // routes around a child's halo. Lane mode never needs it and is left
        // as it was.
        if band.is_some() {
            // The invariant itself, checked rather than trusted: no cell of
            // this trunk within two of an earlier trunk's, or of a cell a
            // later endpoint still holds for its own way out. A first-wins
            // map cannot promise that -- a pending path keeps a cell the
            // ball would otherwise claim -- so a breach is refused by type,
            // and the layout search moves on, rather than shipped.
            let within_two = |a: Anchor, b: Anchor| {
                a.x.abs_diff(b.x) + a.y.abs_diff(b.y) + a.z.abs_diff(b.z) <= 2
            };
            for block in &route.cells {
                for earlier in &routes {
                    if let Some(near) = earlier
                        .cells
                        .iter()
                        .find(|other| within_two(block.at, other.at))
                    {
                        return Err(PackedConnectionError::BandIsolation {
                            signal: signal.clone(),
                            at: block.at,
                            near: near.at,
                        });
                    }
                }
                for (pending_trunk, pending) in &egress {
                    if *pending_trunk <= ordinal {
                        continue;
                    }
                    if let Some(near) = pending.owned().find(|own| within_two(block.at, *own)) {
                        return Err(PackedConnectionError::BandIsolation {
                            signal: signal.clone(),
                            at: block.at,
                            near,
                        });
                    }
                }
            }
            for block in route.owned_blocks() {
                for at in two_hop_ball(block.at) {
                    if canvas.contains(at) {
                        reservations.reserve(
                            at,
                            PACKED_TRUNK_HALO_KEEP_OUT,
                            PhysicalReservationKind::KeepOut,
                        );
                    }
                }
            }
        }
        // Both ends are consumed. Their access columns go back to the parent
        // exactly as `compose` hands a routed trunk's runway back, so a later
        // trunk is not refused by a guard whose endpoint no longer exists.
        // The lateral guard and the coupling ring are *not* endpoint-owned and
        // are not touched here: they protect the child, not the endpoint, and
        // stand for the whole composition.
        trunk.release_source(
            &mut reservations,
            packed,
            guard_top,
            layered,
            &egress_of(trunk.source.endpoint),
        )?;
        for (sink_id, sink) in &trunk.sinks {
            let egress = egress_of(sink.endpoint);
            for at in sink_access(packed, sink_id.as_ref(), sink, guard_top, layered)?
                .into_iter()
                .chain(egress.owned())
                .chain(egress.closure.iter().copied())
            {
                reservations.release_endpoint_keep_out(at, sink.endpoint);
            }
        }
        // A single reservation cell can represent only one owner. If this
        // consumed endpoint owned a closure cell shared with a later
        // endpoint's closure, its release just opened that shared cell. Put
        // every still-pending closure back before the next search; cells
        // already occupied by another claim or this route remain untouched.
        let pending_egress = egress
            .iter()
            .filter(|(pending_trunk, _)| *pending_trunk > ordinal)
            .cloned()
            .collect::<Vec<_>>();
        reserve_packed_egress_closures(&mut reservations, &pending_egress);
        routes.push(route);
        signals.push(signal);
    }
    Ok(PackedTrunks {
        world,
        routes,
        signals,
        lanes,
    })
}

/// One lane per trunk, lowest first, shared by trunks whose plan-view shapes
/// do not meet: a climb box at each terminal and an L between source and
/// each sink.
///
/// A lane per trunk stacked every trunk of a dense node a pitch higher than
/// the last. Measured in `seven_segment`: a node with a dozen trunks put two
/// terminals ten cells apart on a lane at y30, and the search flooded every
/// layer below it until its queue ran out. Trunks that cannot meet need no
/// height between them; trunks that can still get it.
fn shared_lanes(resolved: &[ResolvedPackedTrunk], base: i32) -> Vec<Option<i32>> {
    // Plan-view boxes, inclusive: (lo x, lo z), (hi x, hi z).
    type Box2 = ((i32, i32), (i32, i32));
    let span = |a: (i32, i32), b: (i32, i32), grow: i32| -> Box2 {
        (
            (a.0.min(b.0) - grow, a.1.min(b.1) - grow),
            (a.0.max(b.0) + grow, a.1.max(b.1) + grow),
        )
    };
    let meet = |a: &Box2, b: &Box2| {
        a.0 .0 <= b.1 .0 && b.0 .0 <= a.1 .0 && a.0 .1 <= b.1 .1 && b.0 .1 <= a.1 .1
    };
    // A trunk on its lane: a climb within reach of every terminal, and one L
    // from the source to each sink -- along x first or along z first. Two
    // trunks whose boxes never meet can share a lane; the lane is a height
    // preference, so a crossing the L's did not foresee still routes, a level
    // off. Each end's climb box counts the runway it leaves by.
    let ends_grow = RISER_REACH + TERMINAL_RUNWAY_CELLS as i32 + 2;
    let leg_grow = 2;
    let shape = |trunk: &ResolvedPackedTrunk, x_first: bool| -> Vec<Box2> {
        let at = |terminal: &Terminal| (terminal.anchor.x, terminal.anchor.z);
        let source = at(&trunk.source);
        let mut boxes = vec![span(source, source, ends_grow)];
        for (_, sink) in &trunk.sinks {
            let sink = at(sink);
            let corner = if x_first { (sink.0, source.1) } else { (source.0, sink.1) };
            boxes.push(span(sink, sink, ends_grow));
            boxes.push(span(source, corner, leg_grow));
            boxes.push(span(corner, sink, leg_grow));
        }
        boxes
    };
    let mut layers: Vec<Vec<Box2>> = Vec::new();
    resolved
        .iter()
        .map(|trunk| {
            let fits = |layer: &Vec<Box2>, own: &Vec<Box2>| {
                own.iter().all(|mine| layer.iter().all(|other| !meet(mine, other)))
            };
            let (layer, own) = [true, false]
                .into_iter()
                .map(|x_first| shape(trunk, x_first))
                .map(|own| {
                    let layer = layers
                        .iter()
                        .position(|taken| fits(taken, &own))
                        .unwrap_or(layers.len());
                    (layer, own)
                })
                .min_by_key(|(layer, _)| *layer)
                .expect("two shapes");
            if layer == layers.len() {
                layers.push(Vec::new());
            }
            layers[layer].extend(own);
            let layer = i32::try_from(layer).unwrap_or(i32::MAX / PACKED_LANE_PITCH);
            Some(base.saturating_add(layer.saturating_mul(PACKED_LANE_PITCH)))
        })
        .collect()
}

/// Diagnostics only (`REDA_TRACE_FABRIC`): every cell a planned trunk would
/// take, or touch through its keep-out ring, that some other owner holds.
fn trace_fabric_refusal(
    signal: &str,
    route: RouteId,
    branches: &[PlannedBranch],
    reservations: &PhysicalReservations,
) {
    let own = |owner: PhysicalReservationOwner| {
        matches!(owner, PhysicalReservationOwner::Route(id) | PhysicalReservationOwner::RouteStair(id) if id == route)
            || matches!(owner, PhysicalReservationOwner::Endpoint(_))
    };
    for (k, branch) in branches.iter().enumerate() {
        for (index, cell) in branch.path.iter().enumerate() {
            let near = std::iter::once(*cell)
                .chain(std::iter::once(Anchor { y: cell.y - 1, ..*cell }))
                .chain(keep_out_typed(*cell));
            for at in near {
                if let Some(claim) = reservations.get(&at).filter(|claim| !own(claim.owner)) {
                    eprintln!(
                        "reda: fabric {signal} branch {k} step {index}/{} {cell:?}: {at:?} held by {:?} as {:?}",
                        branch.path.len(),
                        claim.owner,
                        claim.kind
                    );
                }
            }
        }
    }
}

/// Diagnostics only (`REDA_DUMP_FAILED_TRUNK=<dir>`): write the packed world
/// a trunk failed to route in as `<dir>/failed-<signal>.litematic`, for the
/// viewer's `debug:` entry. Every other pending terminal's held egress is
/// drawn as light blue glass and the failed trunk's own terminals as redstone
/// blocks; the world is otherwise exactly what the search saw. The first
/// failure per signal is kept.
fn dump_failed_trunk(
    dir: &std::path::Path,
    world: &World,
    egress: &[(usize, Egress)],
    trunk: &ResolvedPackedTrunk,
    lane: Option<i32>,
) {
    let path = dir.join(format!("failed-{}.litematic", trunk.signal));
    if path.exists() {
        return;
    }
    let own = std::iter::once(&trunk.source)
        .chain(trunk.sinks.iter().map(|(_, sink)| sink))
        .collect::<Vec<_>>();
    let block = |kind, name: &str| BlockState {
        kind,
        name: name.to_owned(),
        ..BlockState::air()
    };
    let glass = block(BlockKind::Glass, "minecraft:light_blue_stained_glass");
    let marker = block(BlockKind::RedstoneBlock, "minecraft:redstone_block");
    let mut shown = world.clone();
    let (size_x, size_y, size_z) = shown.size();
    let inside = |at: Anchor| {
        at.x >= 0 && at.y >= 0 && at.z >= 0 && at.x < size_x && at.y < size_y && at.z < size_z
    };
    let mut held_cells = 0usize;
    for (_, held) in egress {
        if own.iter().any(|end| end.endpoint == held.endpoint) {
            continue;
        }
        for at in held.owned() {
            if inside(at) && shown.get(at.x, at.y, at.z).kind == BlockKind::Air {
                shown.set(at.x, at.y, at.z, glass.clone());
                held_cells += 1;
            }
        }
    }
    eprintln!(
        "dumped failed trunk {} to {}: ends {:?}, lane {lane:?}, {held_cells} held egress cells",
        trunk.signal,
        path.display(),
        own.iter().map(|end| end.anchor).collect::<Vec<_>>(),
    );
    for end in &own {
        if inside(end.anchor) {
            shown.set(end.anchor.x, end.anchor.y, end.anchor.z, marker.clone());
        }
    }
    let _ = std::fs::create_dir_all(dir);
    // A plan-view text of every layer the trunk's ends span, for reading
    // without the viewer.
    let (lo, hi) = own.iter().fold(
        ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN)),
        |(lo, hi), end| {
            (
                (lo.0.min(end.anchor.x), lo.1.min(end.anchor.z)),
                (hi.0.max(end.anchor.x), hi.1.max(end.anchor.z)),
            )
        },
    );
    let mut text = String::new();
    for y in 0..size_y.min(lane.unwrap_or(16) + 4) {
        text.push_str(&format!("y={y} x={}..={} z={}..={}\n", lo.0 - 12, hi.0 + 12, lo.1 - 4, hi.1 + 4));
        for z in (lo.1 - 4).max(0)..=(hi.1 + 4).min(size_z - 1) {
            for x in (lo.0 - 12).max(0)..=(hi.0 + 12).min(size_x - 1) {
                let state = shown.get(x, y, z);
                text.push(match state.kind {
                    BlockKind::Air => '.',
                    BlockKind::RedstoneWire => 'w',
                    BlockKind::Repeater => 'r',
                    BlockKind::Glass if state.name.contains("light_blue") => 'g',
                    BlockKind::Glass => 'G',
                    BlockKind::RedstoneBlock => 'R',
                    BlockKind::Torch | BlockKind::WallTorch => 't',
                    _ => '#',
                });
            }
            text.push('\n');
        }
    }
    let _ = std::fs::write(dir.join(format!("failed-{}.txt", trunk.signal)), text);
    if let Err(error) = crate::formats::litematic::save(&path, &shown, &trunk.signal) {
        eprintln!("could not dump failed trunk {}: {error}", trunk.signal);
    }
}

struct ResolvedPackedTrunk {
    signal: String,
    strength: u8,
    source_id: Option<FreeLeafInterfaceId>,
    source: Terminal,
    /// `None` for a node-built output handover (`PackedOutputExport`).
    sinks: Vec<(Option<FreeLeafInterfaceId>, Terminal)>,
}

/// A sink's core columns: its leaf's declared access continued to `top`, or,
/// for a handover this node built, the plain core the source side uses.
fn sink_access(
    packed: &PackedFreeLeaves,
    id: Option<&FreeLeafInterfaceId>,
    terminal: &Terminal,
    top: i32,
    layered: bool,
) -> Result<Vec<Anchor>, PackedConnectionError> {
    match id {
        Some(id) => packed_terminal_access(packed, id, terminal, top),
        None => Ok(built_access(terminal, top, layered)),
    }
}

/// A node-built end's core columns: up from the floor to `top` when every
/// built end shares one layer, its own layer and one either side when they
/// are `layered`.
fn built_access(terminal: &Terminal, top: i32, layered: bool) -> Vec<Anchor> {
    let y = terminal.anchor.y;
    if layered {
        terminal_access_cells_from(terminal.anchor, terminal.facing, y - 1, y + 1)
    } else {
        terminal_access_cells(terminal.anchor, terminal.facing, top.max(y + 1))
    }
}

impl ResolvedPackedTrunk {
    fn source_access(
        &self,
        packed: &PackedFreeLeaves,
        top: i32,
        layered: bool,
    ) -> Result<Vec<Anchor>, PackedConnectionError> {
        match &self.source_id {
            Some(id) => packed_terminal_access(packed, id, &self.source, top),
            None => Ok(built_access(&self.source, top, layered)),
        }
    }

    fn release_source(
        &self,
        reservations: &mut PhysicalReservations,
        packed: &PackedFreeLeaves,
        top: i32,
        layered: bool,
        egress: &Egress,
    ) -> Result<(), PackedConnectionError> {
        for at in self
            .source_access(packed, top, layered)?
            .into_iter()
            .chain(egress.owned())
            .chain(egress.closure.iter().copied())
        {
            reservations.release_endpoint_keep_out(at, self.source.endpoint);
        }
        Ok(())
    }
}

fn packed_interface<'a>(
    packed: &'a PackedFreeLeaves,
    id: &FreeLeafInterfaceId,
) -> Result<(&'a PackedFreeLeaf, &'a ParentConnectableInterface, usize), PackedConnectionError> {
    let (index, (_, leaf)) = packed
        .placements
        .iter()
        .enumerate()
        .find(|(_, (chunk, _))| *chunk == &id.chunk)
        .ok_or_else(|| PackedConnectionError::MissingPlacement {
            chunk: id.chunk.clone(),
        })?;
    let interface =
        leaf.interfaces
            .get(id)
            .ok_or_else(|| PackedConnectionError::MissingInterface {
                chunk: id.chunk.clone(),
                endpoint: id.endpoint,
            })?;
    Ok((leaf, interface, index))
}

fn packed_terminal(
    _leaf: &PackedFreeLeaf,
    interface: &ParentConnectableInterface,
    child_index: usize,
    local: PhysicalEndpointId,
) -> Result<Terminal, PackedConnectionError> {
    let instance = InstanceId(
        u32::try_from(child_index).map_err(|_| PackedConnectionError::EndpointIndexOverflow)?,
    );
    let port = match (interface.role, local) {
        (PortRole::Output, PhysicalEndpointId::DeclaredOutput(port))
        | (PortRole::Input, PhysicalEndpointId::PrimaryInput(port)) => port,
        (role, endpoint) => {
            return Err(PackedConnectionError::InvalidLocalEndpoint { role, endpoint });
        }
    };
    let slot = u16::try_from(port.0).map_err(|_| PackedConnectionError::EndpointIndexOverflow)?;
    let handover = interface.pin.handover(interface.role);
    Ok(match interface.role {
        PortRole::Output => Terminal {
            endpoint: PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance,
                node: TopologyNodeId(slot),
            }),
            anchor: interface.pin.at,
            facing: interface.pin.toward,
            support: handover,
            target: None,
        },
        PortRole::Input => {
            let connection = ConnectionId::External {
                instance,
                input_index: slot,
            };
            Terminal {
                endpoint: PhysicalEndpointId::Landing(connection),
                anchor: interface.pin.at,
                facing: interface.pin.toward.opposite(),
                support: handover,
                target: Some(RouteTarget::Connection(connection)),
            }
        }
    })
}

#[derive(Debug, Clone, Copy)]
struct PackedCanvas {
    max: Anchor,
}

impl PackedCanvas {
    fn contains(self, at: Anchor) -> bool {
        at.x >= 0
            && at.y >= 0
            && at.z >= 0
            && at.x <= self.max.x
            && at.y <= self.max.y
            && at.z <= self.max.z
    }
}

fn packed_canvas(
    packed: &PackedFreeLeaves,
    terminals: &[&Terminal],
) -> Result<PackedCanvas, PackedConnectionError> {
    let mut cells = packed.halo.clone();
    for terminal in terminals {
        let exit = step(terminal.anchor, terminal.facing);
        cells.extend([
            terminal.anchor,
            terminal.support,
            exit,
            step(exit, terminal.facing),
            Anchor {
                y: terminal.anchor.y - 1,
                ..terminal.anchor
            },
        ]);
        // A root end's mouth may lie past the frame -- its runway leads out
        // to the caller -- and a cell outside the frame is not one the canvas
        // owes; only the ring cells inside it count toward the extent.
        cells.extend(
            mouth_ring(terminal)
                .into_iter()
                .filter(|at| at.x >= 0 && at.y >= 0 && at.z >= 0),
        );
    }
    let top = cells
        .iter()
        .map(|at| at.y)
        .max()
        .ok_or(PackedConnectionError::EmptyCanvas)?;
    for terminal in terminals {
        cells.extend(guard_cells(terminal, top));
    }
    if let Some(at) = cells
        .iter()
        .copied()
        .find(|at| at.x < 0 || at.y < 0 || at.z < 0)
    {
        return Err(PackedConnectionError::NegativeCanvas { at });
    }
    let mut cells = cells.into_iter();
    let first = cells.next().ok_or(PackedConnectionError::EmptyCanvas)?;
    let max = cells.fold(first, |max, at| Anchor {
        x: max.x.max(at.x),
        y: max.y.max(at.y),
        z: max.z.max(at.z),
    });
    Ok(PackedCanvas { max })
}

fn validate_packed_terminal_runway(
    packed: &PackedFreeLeaves,
    id: &FreeLeafInterfaceId,
    terminal: &Terminal,
    top: i32,
) -> Result<(), PackedConnectionError> {
    let owner = packed.placements.get(&id.chunk).ok_or_else(|| {
        PackedConnectionError::MissingPlacement {
            chunk: id.chunk.clone(),
        }
    })?;
    for at in guard_cells(terminal, top) {
        for sibling in packed.placements.values() {
            if sibling.chunk != owner.chunk && sibling.halo.contains(&at) {
                return Err(PackedConnectionError::EndpointRunwayBlocked {
                    endpoint: terminal.endpoint,
                    at,
                    sibling: sibling.chunk.clone(),
                });
            }
        }
    }
    Ok(())
}

/// The cells of `terminal`'s three core columns that its own leaf declared as
/// access: the leaf's authority, up to the leaf's own halo top and no higher.
fn declared_terminal_access(
    packed: &PackedFreeLeaves,
    id: &FreeLeafInterfaceId,
    terminal: &Terminal,
) -> Result<BTreeSet<Anchor>, PackedConnectionError> {
    let leaf = packed.placements.get(&id.chunk).ok_or_else(|| {
        PackedConnectionError::MissingPlacement {
            chunk: id.chunk.clone(),
        }
    })?;
    let axis = terminal_access_cells(terminal.anchor, terminal.facing, 0)
        .into_iter()
        .map(|at| (at.x, at.z))
        .collect::<BTreeSet<_>>();
    Ok(leaf
        .access
        .iter()
        .copied()
        .filter(|at| axis.contains(&(at.x, at.z)))
        .collect())
}

/// Every cell of `terminal`'s three core columns the parent opens for its
/// search: what the leaf declared, and the same columns continued from the
/// leaf's own top through `top`, the pack-wide lid the guard columns stand
/// to.
///
/// A leaf declares access only as high as its own halo; a taller sibling
/// lifts the pack's lid above that, and the guard column stands through the
/// lid. Without the continuation the cells between the two tops are guard
/// keep-out that no release ever reaches, and a trunk from the shorter leaf
/// that has to climb -- to a lane, or over the sibling -- is walled in at its
/// own terminal. The continuation is cut from the same geometry as the guard,
/// so it is exactly the core of that column and nothing beside it; a terminal
/// whose leaf declared no access at all still gets nothing, and stays refused
/// by [`validate_packed_terminal_access`].
fn packed_terminal_access(
    packed: &PackedFreeLeaves,
    id: &FreeLeafInterfaceId,
    terminal: &Terminal,
    top: i32,
) -> Result<Vec<Anchor>, PackedConnectionError> {
    let declared = declared_terminal_access(packed, id, terminal)?;
    let Some(leaf_top) = declared.iter().map(|at| at.y).max() else {
        return Ok(Vec::new());
    };
    let mut cells = declared.into_iter().collect::<Vec<_>>();
    if top > leaf_top {
        cells.extend(terminal_access_cells_from(
            terminal.anchor,
            terminal.facing,
            leaf_top + 1,
            top,
        ));
    }
    Ok(cells)
}

fn validate_packed_terminal_access(
    packed: &PackedFreeLeaves,
    id: &FreeLeafInterfaceId,
    terminal: &Terminal,
) -> Result<(), PackedConnectionError> {
    let access = declared_terminal_access(packed, id, terminal)?;
    for at in terminal_access_cells(terminal.anchor, terminal.facing, terminal.anchor.y)
        .into_iter()
        .filter(|at| at.y == terminal.anchor.y)
    {
        if !access.contains(&at) {
            return Err(PackedConnectionError::EndpointAccessMissing {
                endpoint: terminal.endpoint,
                at,
            });
        }
    }
    Ok(())
}

/// The mouth ring past `terminal`'s runway, in the parent frame.
fn mouth_ring(terminal: &Terminal) -> [Anchor; 3] {
    terminal_mouth_ring(terminal.anchor, terminal.facing)
}

/// One packed terminal's way out to its trunk's lane, clipped to the canvas.
///
/// `mouth` is the three cells the route must enter one of; `path` the
/// straight staircase from the mouth to `lane` it may climb; `clearance` the
/// riser and headroom cells that climb needs clear of every claim; `closure`
/// the coupling closure of runway, mouth and path, which needs only to hold
/// no foreign conductor. The first three are cells the owning route may
/// occupy and so are held as its own, releasable keep-out; the closure is
/// held the same way only where nothing holds it already.
#[derive(Debug, Clone)]
struct Egress {
    endpoint: PhysicalEndpointId,
    /// The terminal's own anchor and direction, which the egress leaves along.
    anchor: Anchor,
    facing: Facing,
    mouth: Vec<Anchor>,
    path: Vec<Anchor>,
    clearance: Vec<Anchor>,
    closure: BTreeSet<Anchor>,
}

impl Egress {
    /// Every cell the owning route may have to occupy, in reservation order.
    fn owned(&self) -> impl Iterator<Item = Anchor> + '_ {
        self.mouth
            .iter()
            .chain(self.path.iter())
            .chain(self.clearance.iter())
            .copied()
    }
}

/// `terminal`'s [`Egress`] toward `lane`, or to nowhere past its mouth when
/// this composition assigned no lane. Cells outside `canvas` are dropped:
/// the shell holds them and no route may stand there.
fn packed_egress(terminal: &Terminal, lane: Option<i32>, canvas: PackedCanvas) -> Egress {
    let top = lane.unwrap_or(terminal.anchor.y);
    let inside = |cells: Vec<Anchor>| {
        cells
            .into_iter()
            .filter(|at| canvas.contains(*at))
            .collect::<Vec<_>>()
    };
    Egress {
        endpoint: terminal.endpoint,
        anchor: terminal.anchor,
        facing: terminal.facing,
        mouth: inside(mouth_ring(terminal).to_vec()),
        path: inside(terminal_egress_path(terminal.anchor, terminal.facing, top)),
        clearance: inside(terminal_egress_clearance(
            terminal.anchor,
            terminal.facing,
            top,
        )),
        closure: terminal_egress_closure(terminal.anchor, terminal.facing, top)
            .into_iter()
            .filter(|at| canvas.contains(*at))
            .collect(),
    }
}

/// Replace `held`'s canonical straight staircase with a compact climb planned
/// around what `reservations` already hold ([`plan_riser`]), when the column
/// around its mouth has one. The canonical ramp runs as far sideways as it
/// climbs; this stays within [`RISER_REACH`] of the mouth. When no such climb
/// exists the ramp is kept, so a terminal is never left with less than before.
///
/// Planned in egress order against reservations that already hold every
/// earlier terminal's egress, so later climbs coil around earlier ones.
fn coil_egress(reservations: &PhysicalReservations, held: &mut Egress, lane: i32, canvas: PackedCanvas) {
    let Some(&mouth) = held.path.first() else {
        return;
    };
    if lane <= mouth.y {
        return;
    }
    let endpoint = held.endpoint;
    let ours_or_free = |at: Anchor| {
        canvas.contains(at)
            && reservations.get(&at).is_none_or(|claim| {
                claim.owner == PhysicalReservationOwner::Endpoint(endpoint)
            })
    };
    let open = |at: Anchor| {
        ours_or_free(at)
            && keep_out_typed(at).into_iter().all(|near| {
                reservations.get(&near).is_none_or(|claim| {
                    claim.owner == PhysicalReservationOwner::Endpoint(endpoint)
                        || !matches!(claim.kind, PhysicalReservationKind::Conductor(_))
                })
            })
    };
    let request = RiserRequest {
        mouth,
        entered: held.facing,
        top: lane,
        flow: RiserFlow::Up,
        reach: RISER_REACH,
        max_nodes: RISER_NODES,
        // The router lays this climb as dust; torch parts and rungs wait for
        // a parent that builds its risers itself.
        torches: false,
        ladders: false,
    };
    let Some(climb) = plan_riser(&request, open, ours_or_free) else {
        return;
    };
    let path = std::iter::once(mouth)
        .chain(climb.into_iter().filter_map(|part| match part {
            RiserStep::Dust(cell) | RiserStep::Rung(cell) => Some(cell),
            RiserStep::TorchPair { .. } | RiserStep::CascadePair { .. } => None,
        }))
        .collect::<Vec<_>>();
    let mut clearance = BTreeSet::new();
    for pair in path.windows(2) {
        clearance.extend(staircase_clearance(pair[0], pair[1]));
        clearance.insert(Anchor {
            y: pair[1].y - 1,
            ..pair[1]
        });
    }
    let mut own = crate::compile::fragment_synth::terminal_geometry::runway_core(held.anchor, held.facing);
    own.extend(held.mouth.iter().copied());
    own.extend(path.iter().copied());
    let own = own.into_iter().collect::<BTreeSet<_>>();
    clearance.retain(|at| !own.contains(at) && canvas.contains(*at));
    held.closure = own
        .iter()
        .flat_map(|at| keep_out_typed(*at))
        .filter(|at| !own.contains(at) && canvas.contains(*at))
        .collect();
    held.clearance = clearance.into_iter().collect();
    held.path = path;
}

/// How far sideways a planned climb may reach from its terminal's mouth.
const RISER_REACH: i32 = 3;
/// Node budget for one planned climb.
const RISER_NODES: usize = 50_000;

/// Hold every pending terminal's routeable egress, first-wins.
///
/// **Phase one, the cells a route occupies.** Every terminal's mouth, path
/// and clearance, in trunk order, each cell held by its endpoint where the
/// cell is free. A cell a guard, a halo, a child or another endpoint already
/// holds is left with its holder: the halo is a certified child's claim and
/// a guard another terminal's, and neither is the parent's to reassign. So is
/// a cell another trunk's endpoint already holds: its egress was reserved
/// first and keeps it. The later terminal's canonical staircase is then not
/// promised whole, only its search -- it may land beside the earlier trunk
/// or be refused by the router, but it is never certified through a cell
/// another route took. Refusing the layout outright instead refused every
/// packing in `seven_segment` whose two children face each other across a
/// narrow gap on one portal row, where two staircases always meet.
///
/// Coupling closures are reserved separately, after every path and persistent
/// root egress, so a closure cannot seal a path it was meant to protect.
///
/// Held cells are released only by the owning endpoint's own release, by the parent at a source's attempt and at
/// consumption, and by the router at each sink's own branch; a rolled-back
/// attempt restores them with everything else.
fn reserve_packed_egress(reservations: &mut PhysicalReservations, egress: &[(usize, Egress)]) {
    for (_, egress) in egress {
        for at in egress.owned() {
            reservations.reserve(
                at,
                PhysicalReservationOwner::Endpoint(egress.endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
}

/// Reserve coupling closures only after every routeable path and persistent
/// root egress has claimed its cells.
fn reserve_packed_egress_closures(
    reservations: &mut PhysicalReservations,
    egress: &[(usize, Egress)],
) {
    for (_, egress) in egress {
        for at in egress.closure.iter().copied() {
            reservations.reserve(
                at,
                PhysicalReservationOwner::Endpoint(egress.endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
}

/// How this node's trunks cross the bands the packer left them.
///
/// One entry per resolved trunk, in trunk order: the height its route is
/// guided to -- its sink's terminal height for the first layer, one
/// [`PACKED_LANE_PITCH`] higher for the second -- and the `x` of the first
/// column of its sink's own halo, where the band ends and the sink's
/// corridor across its own child begins.
struct BandPlan {
    layers: Vec<i32>,
    corridor_edge: Vec<i32>,
}

/// **Which band layer each crossing takes.**
///
/// Taken in source-row order, two crossings can share a layer exactly when
/// their sinks come in the same order: a crossing whose sink lies below an
/// earlier crossing's sink has to cross it somewhere in the band, and only
/// height separates two conductors that cross. So the layers are a partition
/// of the source-to-sink permutation into increasing runs, and the fewest
/// such runs is the length of its longest decreasing subsequence. Patience
/// first-fit -- each crossing goes on the first layer whose last sink is
/// below its own -- produces exactly that many, deterministically, with ties
/// broken by sink row and then by position. Nothing caps the count: the
/// geometry the band can hold is judged by [`band_min_width`] and the cost
/// gate, not here.
///
/// One rule, read by the packer's `seam_bands` to size the band and by
/// [`band_plan`] to place the trunks, so the two never disagree.
pub(crate) fn band_layers(pairs: &[(i32, i32)]) -> Vec<usize> {
    let mut order = (0..pairs.len()).collect::<Vec<_>>();
    order.sort_by_key(|&i| (pairs[i].0, pairs[i].1, i));
    let mut tops: Vec<i32> = Vec::new();
    let mut layers = vec![0usize; pairs.len()];
    for i in order {
        let sink = pairs[i].1;
        match tops.iter().position(|top| *top < sink) {
            Some(layer) => {
                tops[layer] = sink;
                layers[i] = layer;
            }
            None => {
                tops.push(sink);
                layers[i] = tops.len() - 1;
            }
        }
    }
    layers
}

/// **Does the band pay for itself?**
///
/// What the band replaces is a climb: with lanes, every trunk climbs from
/// terminal height over the lid and back down, at least `2 * (lid_y -
/// terminal_y)` cells. What the band costs every trunk is its own width
/// across, `band_min_width` for the layers the crossings need. The band is
/// taken only when that width is strictly less than the climb it saves the
/// *cheapest* trunk, so no trunk is made worse. Both heights are read off the
/// layout that was actually built; nothing here is a setting. Checked
/// arithmetic; an overflow is a `None`, which the callers treat as no band.
pub(crate) fn band_pays(layers: &[usize], lid_y: i32, terminal_y: i32) -> Option<bool> {
    let count = i32::try_from(layers.iter().copied().max().map_or(0, |top| top + 1)).ok()?;
    let width = i64::from(band_min_width(count)?);
    let climb = i64::from(lid_y)
        .checked_sub(i64::from(terminal_y))?
        .checked_mul(2)?;
    Some(width < climb)
}

/// The empty columns a crossing of `layers` layers needs between two halos:
/// a runway and a mouth on each side, plus one pitch per extra layer to
/// climb in. The same figure `seam_bands` sizes the band from.
pub(crate) fn band_min_width(layers: i32) -> Option<i32> {
    let runway = i32::try_from(TERMINAL_RUNWAY_CELLS).ok()?;
    let one = runway.checked_add(1)?.checked_mul(2)?;
    one.checked_add(layers.checked_sub(1)?.checked_mul(PACKED_LANE_PITCH)?)
}

/// The band plan for this node, or `None` when it is not a banded node.
///
/// **Atomic.** Every trunk must be one source to one sink in a different
/// child, leaving east or west with the sink entering from the opposite
/// side, and the two halos must stand apart along that axis by at least the
/// band the seam's crossings need -- one layer's worth, or two when any pair
/// of crossings between those two children inverts its `z` order. One trunk
/// short of that and the whole node keeps its lanes; the band is a promise
/// the packer either made for everyone or did not make.
///
/// Layers are assigned per seam from the crossing order: the longest run of
/// crossings whose sink order agrees with their source order lies on the
/// first layer, everything else on the second. Ties are broken by trunk
/// order, so two runs agree.
fn band_plan(packed: &PackedFreeLeaves, resolved: &[ResolvedPackedTrunk]) -> Option<BandPlan> {
    if resolved.is_empty() {
        return None;
    }
    let x_span = |chunk: &ChunkId| {
        let halo = &packed.placements.get(chunk)?.halo;
        Some((
            halo.iter().map(|at| at.x).min()?,
            halo.iter().map(|at| at.x).max()?,
        ))
    };
    // Per unordered pair: (trunk ordinal, source z, sink z), plus the gap.
    let mut seams: BTreeMap<(ChunkId, ChunkId), (i32, Vec<(usize, i32, i32)>)> = BTreeMap::new();
    let mut corridor_edge = Vec::with_capacity(resolved.len());
    for (ordinal, trunk) in resolved.iter().enumerate() {
        let [(sink_id, sink)] = trunk.sinks.as_slice() else {
            return None;
        };
        let (source_chunk, sink_chunk) = (&trunk.source_id.as_ref()?.chunk, &sink_id.as_ref()?.chunk);
        if source_chunk == sink_chunk {
            return None;
        }
        let out = trunk.source.facing;
        if !matches!(out, Facing::East | Facing::West) || sink.facing != out.opposite() {
            return None;
        }
        let (source_x, sink_x) = (x_span(source_chunk)?, x_span(sink_chunk)?);
        let (gap, edge, seam) = match out {
            Facing::East => (
                sink_x.0.checked_sub(source_x.1)?.checked_sub(1)?,
                sink_x.0,
                (source_x.1, sink_x.0),
            ),
            _ => (
                source_x.0.checked_sub(sink_x.1)?.checked_sub(1)?,
                sink_x.1,
                (sink_x.1, source_x.0),
            ),
        };
        if gap < 0 {
            return None;
        }
        // The band is the seam between *these two* halos: a third child
        // standing in it is a halo to fly over, not a band to cross.
        let between = packed.placements.iter().any(|(chunk, leaf)| {
            chunk != source_chunk
                && chunk != sink_chunk
                && leaf.halo.iter().any(|at| at.x > seam.0 && at.x < seam.1)
        });
        if between {
            return None;
        }
        let key = if source_chunk <= sink_chunk {
            (source_chunk.clone(), sink_chunk.clone())
        } else {
            (sink_chunk.clone(), source_chunk.clone())
        };
        let seam = seams.entry(key).or_insert((gap, Vec::new()));
        seam.0 = seam.0.min(gap);
        seam.1.push((ordinal, trunk.source.anchor.z, sink.anchor.z));
        corridor_edge.push(edge);
    }
    let mut layers = vec![0; resolved.len()];
    for (gap, crossings) in seams.values_mut() {
        crossings.sort_by_key(|(ordinal, source_z, _)| (*source_z, *ordinal));
        // Terminals on one seam must stand a pitch apart on their own side:
        // two sinks on one row would put one trunk's corridor across the
        // other's runway, which only height can separate.
        if !rows_a_pitch_apart(crossings.iter().map(|(_, source_z, _)| *source_z))
            || !rows_a_pitch_apart(crossings.iter().map(|(_, _, sink_z)| *sink_z))
        {
            return None;
        }
        let assigned = band_layers(
            &crossings
                .iter()
                .map(|(_, source_z, sink_z)| (*source_z, *sink_z))
                .collect::<Vec<_>>(),
        );
        let layer_count = i32::try_from(assigned.iter().copied().max().unwrap_or(0)).ok()? + 1;
        let needed = band_min_width(layer_count)?;
        if *gap < needed {
            return None;
        }
        // The same cost gate the packer applied, on the same geometry: the
        // lid is the halo top and the terminal height is the sinks' own.
        let lid_y = packed.halo.iter().map(|at| at.y).max()?;
        let terminal_y = crossings
            .iter()
            .map(|(ordinal, _, _)| resolved[*ordinal].sinks[0].1.anchor.y)
            .max()?;
        if !band_pays(&assigned, lid_y, terminal_y)? {
            return None;
        }
        for ((ordinal, _, _), layer) in crossings.iter().zip(assigned) {
            let base = resolved[*ordinal].sinks[0].1.anchor.y;
            layers[*ordinal] =
                base.checked_add(i32::try_from(layer).ok()?.checked_mul(PACKED_LANE_PITCH)?)?;
        }
    }
    Some(BandPlan {
        layers,
        corridor_edge,
    })
}

/// Whether every pair of rows is at least [`PACKED_LANE_PITCH`] apart.
fn rows_a_pitch_apart(rows: impl Iterator<Item = i32>) -> bool {
    let mut rows = rows.collect::<Vec<_>>();
    rows.sort_unstable();
    rows.windows(2).all(|pair| {
        pair[1]
            .checked_sub(pair[0])
            .is_some_and(|gap| gap >= PACKED_LANE_PITCH)
    })
}

/// Extend a banded sink's egress with its corridor across its own child:
/// the cells at its layer's height, on its own row, from the first column of
/// its halo to the top of its egress path. The route must occupy them to
/// reach the mouth, so they are held for the endpoint like the path, and
/// their coupling closure like the closure, until its own branch opens them.
fn extend_with_band_corridor(
    egress: &mut Egress,
    terminal: &Terminal,
    layer: i32,
    edge: i32,
    canvas: PackedCanvas,
) {
    use crate::compile::routing::keep_out_typed;

    let Some(top) = egress.path.last().copied() else {
        return;
    };
    // Only the run *inside* the sink's own halo, strictly between its first
    // column and the top of the egress path. A mouth that already stands
    // outside the halo, in the band, has no corridor: the band is shared and
    // is nobody's to hold.
    let inside = match terminal.facing {
        Facing::West => (edge + 1)..top.x,
        Facing::East => (top.x + 1)..edge,
        _ => 0..0,
    };
    let corridor = inside
        .map(|x| Anchor {
            x,
            y: layer,
            z: terminal.anchor.z,
        })
        .filter(|at| canvas.contains(*at) && !egress.path.contains(at))
        .collect::<Vec<_>>();
    let own = egress
        .path
        .iter()
        .chain(egress.mouth.iter())
        .chain(corridor.iter())
        .copied()
        .collect::<BTreeSet<_>>();
    for at in &corridor {
        for near in keep_out_typed(*at) {
            if canvas.contains(near) && !own.contains(&near) {
                egress.closure.insert(near);
            }
        }
    }
    egress.path.extend(corridor);
}

/// The owner of the two-hop coupling ball a committed banded trunk claims
/// around itself, so the next trunk on the same layer keeps the distance the
/// lanes used to guarantee by height.
const PACKED_TRUNK_HALO_KEEP_OUT: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 7);

/// Every cell within L1 distance one or two of `at`, excluding `at`: the
/// leaf halo's coupling authority, applied to one parent-laid cell.
fn two_hop_ball(at: Anchor) -> impl Iterator<Item = Anchor> {
    (-2..=2_i32)
        .flat_map(|dx| (-2..=2_i32).flat_map(move |dy| (-2..=2_i32).map(move |dz| (dx, dy, dz))))
        .filter(|(dx, dy, dz)| (1..=2).contains(&(dx.abs() + dy.abs() + dz.abs())))
        .map(move |(dx, dy, dz)| Anchor {
            x: at.x.saturating_add(dx),
            y: at.y.saturating_add(dy),
            z: at.z.saturating_add(dz),
        })
}

/// The owner of the one-cell shell around the packed canvas.
const PACKED_CANVAS_SHELL: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 6);

/// Say where the parent's world ends, in the map the search reads.
///
/// [`PackedCanvas::contains`] is checked after a route comes back, and a route
/// that climbed out of the frame is reported as
/// [`RouteEscaped`](PackedConnectionError::RouteEscaped). That is the right
/// backstop and the wrong first line: the router's own search box reaches six
/// cells above its endpoints, so without a stated ceiling the cheapest route
/// over a congested packing is one that goes over the top of the frame -- and
/// the caller is told its trunk escaped rather than being given the route that
/// fits underneath.
///
/// One cell thick is enough for a search that moves one cell at a time, and
/// keep-out rather than anything heavier because this is the absence of world,
/// not a conductor: nothing may stand here, and nothing beside it is affected.
fn reserve_packed_canvas_shell(reservations: &mut PhysicalReservations, canvas: PackedCanvas) {
    for y in -1..=canvas.max.y.saturating_add(1) {
        for z in -1..=canvas.max.z.saturating_add(1) {
            for x in -1..=canvas.max.x.saturating_add(1) {
                let at = Anchor { x, y, z };
                if !canvas.contains(at) {
                    reservations.reserve(at, PACKED_CANVAS_SHELL, PhysicalReservationKind::KeepOut);
                }
            }
        }
    }
}

fn expanded_packed_world(
    children: &PackedChildWorld,
    canvas: PackedCanvas,
) -> Result<World, PackedConnectionError> {
    let size = (
        canvas.max.x.checked_add(1),
        canvas.max.y.checked_add(1),
        canvas.max.z.checked_add(1),
    );
    let (Some(size_x), Some(size_y), Some(size_z)) = size else {
        return Err(PackedConnectionError::RouteEscaped { at: canvas.max });
    };
    let mut world = World::new(size_x, size_y, size_z);
    let (width, height, depth) = children.world.size();
    for y in 0..height {
        for z in 0..depth {
            for x in 0..width {
                let state = children.world.get(x, y, z);
                if state.kind != BlockKind::Air {
                    let at = Anchor { x, y, z };
                    if !canvas.contains(at) {
                        return Err(PackedConnectionError::RouteEscaped { at });
                    }
                    world.set(x, y, z, state.clone());
                }
            }
        }
    }
    Ok(world)
}

fn reserve_packed_children(
    reservations: &mut PhysicalReservations,
    children: &PackedChildWorld,
    packed: &PackedFreeLeaves,
) -> Result<(), PackedConnectionError> {
    let mut seen = BTreeSet::new();
    for (index, leaf) in packed.placements.values().enumerate() {
        for at in &leaf.occupied {
            if !seen.insert(*at) || !children.occupied.contains(at) {
                return Err(PackedConnectionError::ChildWorldMismatch);
            }
            let Some(state) = children
                .world
                .index(at.x, at.y, at.z)
                .map(|_| children.world.get(at.x, at.y, at.z).clone())
            else {
                return Err(PackedConnectionError::ChildWorldMismatch);
            };
            if state.kind == BlockKind::Air {
                return Err(PackedConnectionError::ChildWorldMismatch);
            }
            reservations.reserve(
                *at,
                PhysicalReservationOwner::KeepOut(
                    u32::try_from(index)
                        .map_err(|_| PackedConnectionError::EndpointIndexOverflow)?,
                ),
                PhysicalReservationKind::Conductor(state),
            );
        }
    }
    if seen != children.occupied {
        return Err(PackedConnectionError::ChildWorldMismatch);
    }
    Ok(())
}

fn place_packed(
    world: &mut World,
    at: Anchor,
    state: BlockState,
) -> Result<(), PackedConnectionError> {
    if world.index(at.x, at.y, at.z).is_none() {
        return Err(PackedConnectionError::RouteEscaped { at });
    }
    if world.get(at.x, at.y, at.z).kind != BlockKind::Air {
        return Err(PackedConnectionError::Overlap { at });
    }
    world.set(at.x, at.y, at.z, state);
    Ok(())
}

fn place_packed_floor(
    world: &mut World,
    at: Anchor,
    state: BlockState,
) -> Result<(), PackedConnectionError> {
    if world.index(at.x, at.y, at.z).is_none() {
        return Err(PackedConnectionError::RouteEscaped { at });
    }
    let existing = world.get(at.x, at.y, at.z);
    if existing.kind == BlockKind::Air {
        world.set(at.x, at.y, at.z, state);
        return Ok(());
    }
    if existing != &state {
        return Err(PackedConnectionError::InvalidFloor {
            at,
            kind: existing.kind,
        });
    }
    Ok(())
}

fn reserve_box(
    reservations: &mut PhysicalReservations,
    prism: &Prism,
    owner: PhysicalReservationOwner,
) {
    for y in prism.min.y..=prism.max.y {
        for z in prism.min.z..=prism.max.z {
            for x in prism.min.x..=prism.max.x {
                reservations.reserve(Anchor { x, y, z }, owner, PhysicalReservationKind::KeepOut);
            }
        }
    }
}

fn step(at: Anchor, facing: Facing) -> Anchor {
    let next = Position::new(at.x, at.y, at.z).offset(facing);
    Anchor {
        x: next.x,
        y: next.y,
        z: next.z,
    }
}

/// The cells a literal root pin keeps for the caller: its own cell and every
/// neighbour of it except the one handover cell this contract builds in.
fn pin_isolation(pin: &crate::compile::planner::PortPin, role: PortRole) -> Vec<Anchor> {
    let handover = pin.handover(role);
    std::iter::once(pin.at)
        .chain(
            [
                Facing::North,
                Facing::South,
                Facing::East,
                Facing::West,
                Facing::Up,
                Facing::Down,
            ]
            .into_iter()
            .map(|facing| step(pin.at, facing)),
        )
        // `x = 0` is the enclosing parent's column, not this contract's to
        // claim -- and nothing here can reach it anyway: the corridor, the
        // access region and every halo start at `x = 1`.
        .filter(|at| *at != handover && at.x >= 1 && at.y >= 0 && at.z >= 0)
        .collect()
}

/// The owner of the blanket keep-out over the corridor's access bands.
///
/// One owner for the whole band, distinct from any child halo index and from
/// the caller row, so a trunk that releases its own window through the band
/// can name exactly what it is allowed to build over.
const ACCESS_BAND_KEEP_OUT: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 1);

/// The owner of a landed root's access region: the parent-owned space between
/// literal pins and the corridor.
///
/// Soft, like the access band: each trunk releases its own endpoint windows
/// through it and nothing else, so two root trunks never share the approach.
const LANDED_ACCESS_KEEP_OUT: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 2);

/// The owner of the cells around a literal root pin that nothing may ever take:
/// the caller's own cell and every neighbour of it but the handover.
///
/// Never released and never in a commit's yielding set, so a landed pin keeps
/// its isolation for the whole composition -- the caller's cell is hardware
/// this contract does not own and must not touch.
const ROOT_PIN_ISOLATION: PhysicalReservationOwner =
    PhysicalReservationOwner::KeepOut(u32::MAX - 3);

fn lane_track(corridor: &Corridor, index: u32, signal: &str) -> Result<i32, ComposeError> {
    corridor
        .lane_track(index)
        .ok_or_else(|| ComposeError::CorridorLanesExhausted {
            signal: signal.to_owned(),
            lane: index,
            lanes: corridor.lane_capacity(),
            band: corridor.lane_band(),
        })
}

fn guided_endpoints(source: &Terminal, sinks: &[Terminal]) -> (Anchor, Vec<Anchor>) {
    (
        source.anchor,
        sinks
            .iter()
            .map(|sink| step(sink.anchor, sink.facing))
            .collect(),
    )
}

fn release_access_band(
    reservations: &mut PhysicalReservations,
    corridor: &Corridor,
    far: Anchor,
    guidance: RouteGuidance,
    source: &Terminal,
    sinks: &[Terminal],
) {
    let (start, goals) = guided_endpoints(source, sinks);
    for z in corridor.access_bands() {
        for y in corridor.region.min.y..=far.y {
            for x in corridor.region.min.x..=far.x {
                let at = Anchor { x, y, z };
                if goals.iter().any(|goal| guidance.allows(at, start, *goal)) {
                    reservations.release_keep_out(at, ACCESS_BAND_KEEP_OUT);
                }
            }
        }
    }
}

/// Open this trunk's own windows through a landed root's access region.
///
/// The same rule the access band uses: a cell the guidance admits for this
/// trunk's source or one of its sinks is released, every other cell of the
/// region stays held, so two root trunks never share the approach even though
/// they cross the same parent-owned space.
fn release_landed_access(
    reservations: &mut PhysicalReservations,
    region: &Prism,
    guidance: RouteGuidance,
    source: &Terminal,
    sinks: &[Terminal],
) {
    let (start, goals) = guided_endpoints(source, sinks);
    for z in region.min.z..=region.max.z {
        for y in region.min.y..=region.max.y {
            for x in region.min.x..=region.max.x {
                let at = Anchor { x, y, z };
                if goals.iter().any(|goal| guidance.allows(at, start, *goal)) {
                    reservations.release_keep_out(at, LANDED_ACCESS_KEEP_OUT);
                }
            }
        }
    }
}

fn guard_cells(terminal: &Terminal, top: i32) -> Vec<Anchor> {
    terminal_guard_cells(terminal.anchor, terminal.facing, top)
}

/// Compose `artifacts` under `plan` and route every trunk with `router`.
pub fn compose(
    plan: &AllocationPlan,
    artifacts: &[LeafArtifact],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
) -> Result<ComposedCircuit, ComposeError> {
    let mut by_id: BTreeMap<&ChunkId, &LeafArtifact> = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for artifact in artifacts {
        if by_id.insert(&artifact.chunk, artifact).is_some() {
            duplicates.insert(artifact.chunk.clone());
        }
    }
    if let Some(chunk) = duplicates.into_iter().next() {
        return Err(ComposeError::DuplicateArtifact { chunk });
    }
    let leaves: Vec<&LeafArtifact> = plan
        .children
        .iter()
        .map(|child| {
            by_id
                .remove(&child.chunk)
                .ok_or_else(|| ComposeError::MissingArtifact {
                    chunk: child.chunk.clone(),
                })
        })
        .collect::<Result<_, _>>()?;
    if let Some(chunk) = by_id.keys().next() {
        return Err(ComposeError::UnexpectedArtifact {
            chunk: (*chunk).clone(),
        });
    }
    let ends: Vec<(Terminal, Vec<Terminal>)> = plan
        .trunks
        .iter()
        .map(|trunk| {
            Ok((
                resolve(plan, &trunk.signal, &trunk.source)?,
                trunk
                    .sinks
                    .iter()
                    .map(|end| resolve(plan, &trunk.signal, end))
                    .collect::<Result<_, _>>()?,
            ))
        })
        .collect::<Result<_, ComposeError>>()?;

    let corridor_plan = &plan.corridor;
    let corridor = corridor_plan.region;
    let corridor_depth = corridor.max.z - corridor.min.z + 1;
    // Below six rows the root and child terminal runways overlap, leaving at
    // least one endpoint permanently guarded by the other.
    if corridor_depth < MIN_CORRIDOR_DEPTH {
        return Err(ComposeError::CorridorTooShallow {
            depth: corridor_depth,
            minimum: MIN_CORRIDOR_DEPTH,
        });
    }
    // The same corner an enclosing parent allocated for this plan, read from
    // the plan rather than recomputed, so the two cannot drift apart.
    let far = plan.local_extent();
    let mut world = World::new(
        far.x
            .checked_add(1)
            .ok_or(ComposeError::OutOfBounds { at: far })?,
        far.y
            .checked_add(1)
            .ok_or(ComposeError::OutOfBounds { at: far })?,
        far.z
            .checked_add(1)
            .ok_or(ComposeError::OutOfBounds { at: far })?,
    );
    let mut reservations = PhysicalReservations::new();
    reservations.treat_keep_out_as_air(AirKeepOut::default());

    // 1. Children: translate every occupied cell and reserve it as the exact
    //    conductor it is, so no trunk may touch or neighbour it.
    for (index, (child, leaf)) in plan.children.iter().zip(&leaves).enumerate() {
        let (sx, sy, sz) = leaf.world.size();
        let overflow = || ComposeError::CoordinateOverflow {
            chunk: child.chunk.clone(),
        };
        for y in 0..sy {
            for z in 0..sz {
                for x in 0..sx {
                    let state = leaf.world.get(x, y, z);
                    if state.kind == BlockKind::Air {
                        continue;
                    }
                    let at = Anchor {
                        x: x.checked_add(child.origin.x).ok_or_else(overflow)?,
                        y: y.checked_add(child.origin.y).ok_or_else(overflow)?,
                        z: z.checked_add(child.origin.z).ok_or_else(overflow)?,
                    };
                    if !child.region.contains(at) {
                        return Err(ComposeError::Escape {
                            chunk: child.chunk.clone(),
                            at,
                        });
                    }
                    place(&mut world, at, state.clone())?;
                    reservations.reserve(
                        at,
                        PhysicalReservationOwner::KeepOut(index as u32),
                        PhysicalReservationKind::Conductor(state.clone()),
                    );
                }
            }
        }
    }

    // 2. Root inputs: parent-owned handover repeater on a floor, as the seed
    //    builds for a pinned input.  Root outputs get their terminal repeater
    //    from the router.
    for port in &plan.root_ports {
        if port.role == PortRole::Input {
            let handover = port.pin.handover(PortRole::Input);
            let floor = Anchor {
                y: handover
                    .y
                    .checked_sub(1)
                    .ok_or(ComposeError::OutOfBounds { at: handover })?,
                ..handover
            };
            place(&mut world, floor, stone())?;
            place(&mut world, handover, repeater(port.pin.toward))?;
            reservations.reserve(
                floor,
                PhysicalReservationOwner::KeepOut(u32::MAX),
                PhysicalReservationKind::Floor(stone()),
            );
            reservations.reserve(
                handover,
                PhysicalReservationOwner::KeepOut(u32::MAX),
                PhysicalReservationKind::Conductor(repeater(port.pin.toward)),
            );
        }
    }

    // 2b. A landed root's pins are the caller's hardware standing inside this
    //     world. Their own cells and every neighbour but the handover are taken
    //     now, before any endpoint guard, so nothing releases them later.
    let landed = plan.root_placement.landed_region();
    if landed.is_some() {
        for port in &plan.root_ports {
            for at in pin_isolation(&port.pin, port.role) {
                reservations.reserve(at, ROOT_PIN_ISOLATION, PhysicalReservationKind::KeepOut);
            }
        }
    }

    // 3. Every trunk end's anchor, exit/approach and ring are held by the end
    //    itself until its trunk runs, so no earlier trunk can occupy or
    //    neighbour them.
    for (source, sinks) in &ends {
        for terminal in std::iter::once(source).chain(sinks) {
            for at in guard_cells(terminal, far.y) {
                reservations.reserve(
                    at,
                    PhysicalReservationOwner::Endpoint(terminal.endpoint),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }

    // 4. The two rows beside each portal row are an access band: a trunk
    //    crosses them only in its own guarded column, so no trunk can wander
    //    along a portal row and box in a later trunk's exit.
    for z in corridor_plan.access_bands() {
        reserve_box(
            &mut reservations,
            &Prism {
                min: Anchor { x: 0, y: 0, z },
                max: Anchor {
                    x: far.x,
                    y: far.y,
                    z,
                },
            },
            ACCESS_BAND_KEEP_OUT,
        );
    }

    // 5. Child halos and the external root row are guarded until their own
    // endpoint is consumed; the post-route corridor check remains authority.
    for (index, child) in plan.children.iter().enumerate() {
        reserve_box(
            &mut reservations,
            &child.halo,
            PhysicalReservationOwner::KeepOut(index as u32),
        );
    }
    // A landed root's access region is soft: a trunk opens its own windows
    // through it and nothing more. It is claimed before the caller row so the
    // row it ends on belongs to the access, and the caller-row box takes only
    // what is left either side of it.
    if let Some(region) = landed {
        reserve_box(&mut reservations, &region, LANDED_ACCESS_KEEP_OUT);
    }
    reserve_box(
        &mut reservations,
        &Prism {
            min: Anchor {
                x: 0,
                y: 0,
                z: plan.caller_row_z(),
            },
            max: Anchor {
                x: far.x,
                y: far.y,
                z: plan.caller_row_z(),
            },
        },
        PhysicalReservationOwner::KeepOut(u32::MAX),
    );

    // 6. Trunks in plan (signal-name) order, each seeing every earlier one.
    let mut trunks = Vec::with_capacity(plan.trunks.len());
    for (index, (trunk, (source, sinks))) in plan.trunks.iter().zip(&ends).enumerate() {
        let route = RouteId(index as u32);
        let route_sinks = sinks
            .iter()
            .enumerate()
            .map(|(ordinal, sink)| RouteSink {
                id: RoutedSinkId {
                    route,
                    ordinal: ordinal as u16,
                },
                endpoint: sink.endpoint,
                anchor: sink.anchor,
                allowed_entry: sink.facing,
                terminal: TerminalContract::Sink {
                    target: sink.target.expect("sinks resolve with a target"),
                    support: sink.support,
                    requirement: match sink.target {
                        Some(RouteTarget::DeclaredOutput(_)) => {
                            TerminalRequirement::Exact(RouteTerminalKind::OutputTerminalRepeater)
                        }
                        _ => TerminalRequirement::Repeater,
                    },
                },
            })
            .collect();
        let route_sinks =
            NonEmptyRouteSinks::new(route_sinks).map_err(|_| ComposeError::NoSinks {
                signal: trunk.signal.clone(),
            })?;
        // Every trunk, root-ended or not, crosses the corridor on the shared
        // lane allocation coloured it into.
        let track = lane_track(&plan.corridor, trunk.lane, &trunk.signal)?;
        let lane_guidance = RouteGuidance {
            origin: Anchor { x: 0, y: 0, z: 0 },
            lateral: Facing::South,
            track,
            half_width: 0,
            access_half_width: ACCESS_HALF_WIDTH,
            preferred_y: Some(3),
            access_y: Some(1),
            hard: true,
            penalty_per_block: 8,
        };
        // A trunk may leave the corridor only at its own terminal cells and
        // the parent-owned child halos that hold their approach hardware.
        let allowed = |at: Anchor| {
            corridor.contains(at)
                || landed.is_some_and(|region| region.contains(at))
                || plan.children.iter().any(|child| child.in_halo(at))
                || std::iter::once(source).chain(sinks).any(|terminal| {
                    at == terminal.anchor
                        || at
                            == Anchor {
                                y: terminal.anchor.y - 1,
                                ..terminal.anchor
                            }
                })
        };
        let mut route_attempt = |guidance| {
            // A temporary view of the master map rather than a copy of it:
            // releases are journalled and rolled back before the route result
            // can leave this closure.
            reservations.begin_attempt();
            for at in guard_cells(source, far.y) {
                reservations.release_endpoint_keep_out(at, source.endpoint);
            }
            release_access_band(
                &mut reservations,
                &plan.corridor,
                far,
                guidance,
                source,
                sinks,
            );
            if let Some(region) = landed {
                release_landed_access(&mut reservations, &region, guidance, source, sinks);
            }
            let routed = router.route_guided(
                RouteRequest {
                    id: route,
                    source: RouteEndpoint {
                        id: source.endpoint,
                        anchor: source.anchor,
                        allowed_exit: source.facing,
                        terminal: TerminalContract::Source {
                            signal_strength: MAX_SIGNAL_STRENGTH,
                        },
                    },
                    sinks: &route_sinks,
                    reservations: &reservations,
                    limits,
                },
                Some(guidance),
            );
            reservations.rollback_attempt();
            routed
        };
        let routed = route_attempt(lane_guidance);
        let tree = routed.map_err(|failure| ComposeError::Route {
            signal: trunk.signal.clone(),
            failure,
        })?;
        for block in tree.owned_blocks() {
            if !allowed(block.at) {
                return Err(ComposeError::RouteEscaped {
                    signal: trunk.signal.clone(),
                    at: block.at,
                });
            }
        }
        // This endpoint is now consumed. Release all of its temporary runway
        // and replace it with clearance around the route that was actually laid.
        for terminal in std::iter::once(source).chain(sinks) {
            for at in guard_cells(terminal, far.y) {
                reservations.release_endpoint_keep_out(at, terminal.endpoint);
            }
        }
        // Exactly the soft keep-outs this trunk was routed through: the access
        // band window it released, and its own ends' runways.  Every other
        // keep-out -- a child halo, the caller row, a later trunk's endpoint,
        // an earlier route's clearance -- stood during the search and must
        // still refuse this commit.
        let yielding: Vec<PhysicalReservationOwner> = std::iter::once(ACCESS_BAND_KEEP_OUT)
            .chain(landed.map(|_| LANDED_ACCESS_KEEP_OUT))
            .chain(
                std::iter::once(source)
                    .chain(sinks)
                    .map(|terminal| PhysicalReservationOwner::Endpoint(terminal.endpoint)),
            )
            .collect();
        for block in &tree.cells {
            place(&mut world, block.at, block.state.clone())?;
            // The search ran against the released view, where this trunk's keep-outs
            // were released; the master map still holds them, so the commit
            // must take ownership of the cell rather than be dropped and leave
            // a later trunk free to route straight through this conductor.
            // Only the keep-outs this trunk was actually given give way.
            if !reservations.commit_routed(
                block.at,
                PhysicalReservationOwner::Route(route),
                PhysicalReservationKind::Conductor(block.state.clone()),
                &yielding,
            ) {
                return Err(ComposeError::Overlap { at: block.at });
            }
        }
        for block in &tree.floors {
            if world.index(block.at.x, block.at.y, block.at.z).is_none() {
                return Err(ComposeError::OutOfBounds { at: block.at });
            }
            let existing = world.get(block.at.x, block.at.y, block.at.z);
            if existing.kind == BlockKind::Air {
                world.set(block.at.x, block.at.y, block.at.z, block.state.clone());
            } else if existing != &block.state {
                return Err(ComposeError::InvalidFloor {
                    at: block.at,
                    kind: existing.kind,
                });
            }
            if !reservations.commit_routed(
                block.at,
                PhysicalReservationOwner::RouteStair(route),
                PhysicalReservationKind::Floor(block.state.clone()),
                &yielding,
            ) {
                return Err(ComposeError::Overlap { at: block.at });
            }
        }
        // No clearance ring around the laid route: every cell it laid is
        // reserved as the exact conductor it is, and the router's own
        // adjacency rule refuses any later conductor beside, above or below
        // one.  A ring of keep-outs said the same thing about the cells it
        // could reach, and also forbade the floor a crossing lane needs.
        trunks.push(tree);
    }

    Ok(ComposedCircuit { world, trunks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::{
        allocate, allocate_with_root_ports, root_placement, AllocationError, AllocationLimits,
        RootAccess,
    };
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::leaf::{synthesise_free_leaf, LEAF_PITCHES};
    use crate::compile::fragment_synth::packing::{
        compose_packed_free_leaf_worlds, pack_free_leaves, search_ranked_layouts_in_order,
        LayoutVerdict, PackingBudget, SeamBands,
    };
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::fragment_synth::schedule::synthesise_children;
    use crate::compile::fragment_synth::terminal_geometry::runway_core;
    use crate::compile::planner::PortPlacements;
    use crate::compile::routing::{DurablePhysicalRouter, PlacedBlock};
    use crate::compile::topology::SignalPolarity;
    use crate::compile::{drive_caller_cell, probe_caller_cell, Gate, Netlist};
    use crate::redstone::simulator::Simulator;

    const LIMITS: AllocationLimits = AllocationLimits {
        delay_budget_ticks: 4,
        corridor_capacity: 8,
    };

    /// The two-leaf fixture, its packed world, and the interface identities of
    /// one internal boundary signal.
    fn packed_chain(
        net: &Netlist,
    ) -> (
        Vec<crate::compile::fragment_synth::leaf::FreeLeafArtifact>,
        PackedFreeLeaves,
        PackedChildWorld,
    ) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), 1).unwrap();
        let contract = crate::compile::fragment_synth::allocation::SignalContract {
            polarity: SignalPolarity::Positive,
            strength: MAX_SIGNAL_STRENGTH,
            delay_budget_ticks: 4,
        };
        let leaves = chunks
            .iter()
            .map(|chunk| synthesise_free_leaf(chunk, contract, &SearchConfig::checked_defaults(), &LEAF_PITCHES))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let packed = pack_free_leaves(&leaves).unwrap();
        let children = compose_packed_free_leaf_worlds(&leaves, &packed).unwrap();
        (leaves, packed, children)
    }

    fn packed_endpoint(
        packed: &PackedFreeLeaves,
        role: PortRole,
        signal: &str,
    ) -> (FreeLeafInterfaceId, ParentConnectableInterface) {
        packed
            .placements
            .values()
            .flat_map(|leaf| leaf.interfaces.iter())
            .find(|(_, interface)| interface.role == role && interface.signal == signal)
            .map(|(id, interface)| (id.clone(), interface.clone()))
            .unwrap_or_else(|| panic!("packed leaves expose {role:?} {signal}"))
    }

    /// The direction a parent route leaves or enters an interface along: out of
    /// an output, into an input. The same rule `leaf` builds its access columns
    /// with.
    fn interface_direction(interface: &ParentConnectableInterface) -> Facing {
        match interface.role {
            PortRole::Output => interface.pin.toward,
            PortRole::Input => interface.pin.toward.opposite(),
        }
    }

    /// The truth of the whole packed artifact, read where a caller would read
    /// it: drive the free input pin, hang a lamp in the free output pin.
    fn observe_packed(world: &World, input: Anchor, output: Anchor, bit: bool) -> bool {
        let mut world = world.clone();
        drive_caller_cell(&mut world, (input.x, input.y, input.z), bit);
        probe_caller_cell(&mut world, (output.x, output.y, output.z));
        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(400).unwrap();
        simulator.world().get(output.x, output.y, output.z).lit
    }

    #[test]
    fn packed_free_leaf_chain_routes_its_boundary_and_simulates_both_cases() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let (_, packed, children) = packed_chain(&net);
        assert_eq!(packed.placements.len(), 2, "the fixture must be two leaves");
        let (source_id, source_interface) = packed_endpoint(&packed, PortRole::Output, "a");
        let (sink_id, sink_interface) = packed_endpoint(&packed, PortRole::Input, "a");
        assert_ne!(
            source_id.chunk, sink_id.chunk,
            "the boundary must cross leaves"
        );
        assert_ne!(source_interface.pin.at, sink_interface.pin.at);

        let mut connection = route_packed_trunks(
            &children,
            &packed,
            &[PackedTrunkRequest {
                signal: "a".into(),
                source: source_id.clone(),
                sinks: vec![sink_id.clone()],
            }],
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .expect("a packed boundary routes under the forced runway contract");
        let route = connection
            .routes
            .pop()
            .expect("one packed request returns one route");
        assert!(connection.routes.is_empty());

        // The contract, read off the tree: the route leaves the source down
        // its own straight runway and arrives on the sink's mirror image.
        let path = &route.branches[0].path;
        let source_core = runway_core(
            source_interface.pin.at,
            interface_direction(&source_interface),
        );
        let mut sink_suffix =
            runway_core(sink_interface.pin.at, interface_direction(&sink_interface));
        sink_suffix.reverse();
        assert_eq!(path[..source_core.len()], source_core[..]);
        assert_eq!(path[path.len() - sink_suffix.len()..], sink_suffix[..]);

        // Each fixed runway cell is one cell of the realised tree, not a cell
        // the contract added twice or left unrealised.
        for cell in source_core.iter().chain(&sink_suffix) {
            assert_eq!(path.iter().filter(|at| *at == cell).count(), 1);
            assert_eq!(
                route.cells.iter().filter(|block| block.at == *cell).count(),
                1,
                "{cell:?} must be realised exactly once"
            );
            assert!(
                route
                    .floors
                    .iter()
                    .filter(|block| block.at == *cell)
                    .count()
                    <= 1
            );
        }

        // And the artifact works: `b` is `NOR(NOR(x))`, so it follows `x`.
        let (_, input) = packed_endpoint(&packed, PortRole::Input, "x");
        let (_, output) = packed_endpoint(&packed, PortRole::Output, "b");
        for bit in [false, true] {
            assert_eq!(
                observe_packed(&connection.world, input.pin.at, output.pin.at, bit),
                bit,
                "a chain of two inverters must reproduce its input"
            );
        }
    }

    /// The lateral guard and the coupling ring are not the endpoint's to
    /// release, so they stand while a sibling's trunk is routed and after it is
    /// committed. Only the three straight core columns ever open.
    #[test]
    fn packed_trunks_route_without_opening_a_sibling_lateral_guard() {
        let net = netlist(
            &["x", "y"],
            &["b", "d"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["y"]),
                Gate::nor("d", &["c"]),
            ],
        );
        let (_, packed, children) = packed_chain(&net);
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        let limits = SearchConfig::checked_defaults().router_limits;

        let routed = route_packed_trunks(
            &children,
            &packed,
            &[request("c"), request("a")],
            &DurablePhysicalRouter,
            limits,
        )
        .expect("two independent packed boundaries route");
        assert_eq!(routed.routes.len(), 2);

        // Nothing either trunk laid may stand in a guard cell that is not one
        // of the three core columns of one of its own four terminals.
        let top = packed.halo.iter().map(|at| at.y).max().unwrap();
        let mut core = BTreeSet::new();
        let mut guard = BTreeSet::new();
        for signal in ["a", "c"] {
            for (role, _) in [(PortRole::Output, ()), (PortRole::Input, ())] {
                let (_, interface) = packed_endpoint(&packed, role, signal);
                let direction = interface_direction(&interface);
                core.extend(terminal_access_cells(interface.pin.at, direction, top));
                guard.extend(terminal_guard_cells(interface.pin.at, direction, top));
            }
        }
        for route in &routed.routes {
            for block in route.owned_blocks() {
                assert!(
                    !guard.contains(&block.at) || core.contains(&block.at),
                    "{:?} stands in a lateral or ring guard cell",
                    block.at
                );
            }
        }
    }

    /// Input order is not an input: the routes and the world are keyed by the
    /// stable signal and interface identities the requests carry.
    #[test]
    fn packed_trunk_request_order_does_not_change_the_composition() {
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;

        let net = netlist(
            &["x", "y"],
            &["b", "d"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["y"]),
                Gate::nor("d", &["c"]),
            ],
        );
        let (_, packed, children) = packed_chain(&net);
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        let limits = SearchConfig::checked_defaults().router_limits;
        let route = |requests: &[PackedTrunkRequest]| {
            route_packed_trunks(&children, &packed, requests, &DurablePhysicalRouter, limits)
                .unwrap()
        };

        let forward = route(&[request("a"), request("c")]);
        let reverse = route(&[request("c"), request("a")]);
        assert_eq!(
            canonical_world_fingerprint(&forward.world),
            canonical_world_fingerprint(&reverse.world)
        );
        assert_eq!(forward.routes, reverse.routes);

        // Two trunks, and no cell of one is a neighbour of a cell of the
        // other: independent signals stay independent in the shared world.
        let cells = |tree: &RealisedRouteTree| {
            tree.cells
                .iter()
                .map(|block| block.at)
                .collect::<BTreeSet<_>>()
        };
        let first = cells(&forward.routes[0]);
        let second = cells(&forward.routes[1]);
        assert!(!first.is_empty() && !second.is_empty());
        for at in &first {
            for facing in [
                Facing::North,
                Facing::South,
                Facing::East,
                Facing::West,
                Facing::Up,
                Facing::Down,
            ] {
                assert!(
                    !second.contains(&step(*at, facing)),
                    "{at:?} couples one trunk to the other"
                );
            }
        }

        // Both chains still compute, in the one shared world.
        for (input, output) in [("x", "b"), ("y", "d")] {
            let input = packed_endpoint(&packed, PortRole::Input, input).1;
            let output = packed_endpoint(&packed, PortRole::Output, output).1;
            for bit in [false, true] {
                assert_eq!(
                    observe_packed(&forward.world, input.pin.at, output.pin.at, bit),
                    bit
                );
            }
        }
    }

    /// Every interface of one role and signal, in stable placement order.
    fn packed_endpoints(
        packed: &PackedFreeLeaves,
        role: PortRole,
        signal: &str,
    ) -> Vec<(FreeLeafInterfaceId, ParentConnectableInterface)> {
        packed
            .placements
            .values()
            .flat_map(|leaf| leaf.interfaces.iter())
            .filter(|(_, interface)| interface.role == role && interface.signal == signal)
            .map(|(id, interface)| (id.clone(), interface.clone()))
            .collect()
    }

    /// Records, for each call, which endpoints' access cells the handed map
    /// still holds, then routes for real.
    struct WatchingAccess {
        held: std::cell::RefCell<Vec<BTreeSet<PhysicalEndpointId>>>,
        probes: Vec<(PhysicalEndpointId, Anchor)>,
    }

    impl PhysicalRouter for WatchingAccess {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.route_with_runways(request, ForcedTerminalRunways::default())
        }

        fn route_with_runways(
            &self,
            request: RouteRequest<'_>,
            runways: ForcedTerminalRunways,
        ) -> Result<RealisedRouteTree, RouterFailure> {
            self.held.borrow_mut().push(
                self.probes
                    .iter()
                    .filter(|(endpoint, at)| {
                        request.reservations.get(at).is_some_and(|claim| {
                            claim.owner == PhysicalReservationOwner::Endpoint(*endpoint)
                                && claim.kind == PhysicalReservationKind::KeepOut
                        })
                    })
                    .map(|(endpoint, _)| *endpoint)
                    .collect(),
            );
            DurablePhysicalRouter.route_with_runways(request, runways)
        }
    }

    /// Records which of `cells` the router was *not* handed as keep-out.
    struct WatchingGuards {
        cells: BTreeSet<Anchor>,
        unheld: std::cell::RefCell<BTreeSet<Anchor>>,
    }

    impl PhysicalRouter for WatchingGuards {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.route_with_runways(request, ForcedTerminalRunways::default())
        }

        fn route_with_runways(
            &self,
            request: RouteRequest<'_>,
            runways: ForcedTerminalRunways,
        ) -> Result<RealisedRouteTree, RouterFailure> {
            self.unheld.borrow_mut().extend(
                self.cells
                    .iter()
                    .filter(|at| {
                        !request
                            .reservations
                            .get(at)
                            .is_some_and(|claim| claim.kind == PhysicalReservationKind::KeepOut)
                    })
                    .copied(),
            );
            DurablePhysicalRouter.route_with_runways(request, runways)
        }
    }

    /// A root end belongs to whoever packs this node next, so its guard column
    /// stands for the whole run -- through the canvas top, not merely to its
    /// own leaf's halo top, which is where that leaf's mask stops.
    ///
    /// Every NOR leaf this crate builds today packs to the same height, so the
    /// two tops coincide and the halo reservation alone would pass. The
    /// fixture therefore shortens the root leaf's mask by one layer, which is
    /// exactly the gap a shorter leaf packed beside a taller one leaves, and
    /// checks both directions: with the root declared those cells are keep-out
    /// in the map the router is handed, and without it they are open.
    #[test]
    fn a_root_end_holds_its_guard_column_above_its_own_leaf_halo() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let (_, packed, children) = packed_chain(&net);
        let request = PackedTrunkRequest {
            signal: "a".into(),
            source: packed_endpoint(&packed, PortRole::Output, "a").0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, "a").0],
        };
        let (root_in, in_interface) = packed_endpoint(&packed, PortRole::Input, "x");
        let limits = SearchConfig::checked_defaults().router_limits;

        let routed = route_packed_trunks_with_root_guards(
            &children,
            &packed,
            &[request.clone()],
            std::slice::from_ref(&root_in),
            &DurablePhysicalRouter,
            limits,
        )
        .expect("a guarded root boundary still routes its internal trunk");
        let top = routed.world.size().1 - 1;

        // The shorter-leaf packing: the root's own mask now stops one layer
        // below the canvas top the taller sibling still sets.
        let mut shortened = packed.clone();
        shortened
            .placements
            .get_mut(&root_in.chunk)
            .expect("the root's leaf is packed")
            .halo
            .retain(|at| at.y < top);
        shortened.halo = shortened
            .placements
            .values()
            .flat_map(|leaf| leaf.halo.iter().copied())
            .collect();

        let band =
            terminal_guard_cells(in_interface.pin.at, interface_direction(&in_interface), top)
                .into_iter()
                .filter(|at| at.y == top && !shortened.halo.contains(at))
                .collect::<BTreeSet<_>>();
        assert!(
            !band.is_empty(),
            "the shortened mask must leave a band for the root guard to hold"
        );

        let watch = |roots: &[FreeLeafInterfaceId]| {
            let watcher = WatchingGuards {
                cells: band.clone(),
                unheld: std::cell::RefCell::new(BTreeSet::new()),
            };
            let routed = route_packed_trunks_with_root_guards(
                &children,
                &shortened,
                &[request.clone()],
                roots,
                &watcher,
                limits,
            )
            .expect("the shortened mask still routes its internal trunk");
            (watcher.unheld.into_inner(), routed)
        };

        let (open, guarded) = watch(std::slice::from_ref(&root_in));
        assert_eq!(
            open,
            BTreeSet::new(),
            "a declared root end must hold its whole column to the canvas top"
        );
        for tree in &guarded.routes {
            for block in tree.owned_blocks() {
                assert!(
                    !band.contains(&block.at),
                    "{:?} stands in a root end's guard column",
                    block.at
                );
            }
        }

        // And the reservation is this code's doing, not the halo's: undeclare
        // the root and the same cells are open again.
        let (open, _) = watch(&[]);
        assert_eq!(
            open, band,
            "without a declared root the band is the caller's to cross"
        );
    }

    /// The two-leaf chain with the leaf that owns `signal`'s source packed one
    /// layer shorter than its sibling: its halo and its declared access both
    /// stop below the canvas top the taller sibling still sets. The shape a
    /// shorter leaf beside a taller one leaves, made deliberately.
    ///
    /// Returns the shortened packing and the short leaf's own top.
    fn shorten_source_leaf(
        packed: &PackedFreeLeaves,
        source: &FreeLeafInterfaceId,
    ) -> (PackedFreeLeaves, i32) {
        let top = packed.halo.iter().map(|at| at.y).max().unwrap();
        let mut shortened = packed.clone();
        let leaf = shortened
            .placements
            .get_mut(&source.chunk)
            .expect("the source's leaf is packed");
        leaf.halo.retain(|at| at.y < top);
        leaf.access.retain(|at| at.y < top);
        let short_top = leaf.access.iter().map(|at| at.y).max().unwrap();
        shortened.halo = shortened
            .placements
            .values()
            .flat_map(|leaf| leaf.halo.iter().copied())
            .collect();
        (shortened, short_top)
    }

    /// A terminal's released core column reaches the pack-wide guard top,
    /// not merely its own leaf's: the same lid every guard column stands to.
    ///
    /// The shortened source leaf's declared access stops one layer below the
    /// canvas top its taller sibling sets, and the test first proves that gap
    /// is real. Under the old rule the parent opened only what the leaf
    /// declared, so the top layer of the column stayed guard keep-out that no
    /// release reached; now every core column of every terminal, short leaf
    /// or tall, is opened through one and the same top.
    #[test]
    fn a_short_leafs_released_core_column_reaches_the_pack_wide_guard_top() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let (_, packed, _) = packed_chain(&net);
        let (source_id, _) = packed_endpoint(&packed, PortRole::Output, "a");
        let (sink_id, _) = packed_endpoint(&packed, PortRole::Input, "a");
        let (shortened, short_top) = shorten_source_leaf(&packed, &source_id);

        let terminal = |id: &FreeLeafInterfaceId| {
            let (leaf, interface, index) = packed_interface(&shortened, id).unwrap();
            packed_terminal(leaf, interface, index, id.endpoint).unwrap()
        };
        let source = terminal(&source_id);
        let sink = terminal(&sink_id);
        let canvas = packed_canvas(&shortened, &[&source, &sink]).unwrap();
        assert!(
            short_top < canvas.max.y,
            "the fixture must leave a gap: short leaf top {short_top}, canvas top {}",
            canvas.max.y
        );
        eprintln!(
            "asymmetric packing: short leaf top {short_top}, canvas top {}",
            canvas.max.y
        );

        for (id, terminal) in [(&source_id, &source), (&sink_id, &sink)] {
            let access = packed_terminal_access(&shortened, id, terminal, canvas.max.y)
                .unwrap()
                .into_iter()
                .collect::<BTreeSet<_>>();
            let declared = declared_terminal_access(&shortened, id, terminal).unwrap();
            let bottom = declared.iter().map(|at| at.y).min().unwrap();
            // Every core column, whole, from the leaf's own floor through the
            // one pack-wide top.
            for core in runway_core(terminal.anchor, terminal.facing) {
                for y in bottom..=canvas.max.y {
                    assert!(
                        access.contains(&Anchor { y, ..core }),
                        "{:?} column {:?} is not open at y = {y} (top {})",
                        terminal.endpoint,
                        (core.x, core.z),
                        canvas.max.y
                    );
                }
            }
            // And nothing else: no cell beside the columns, none above the
            // top, and every cell the leaf declared is still there.
            let columns = runway_core(terminal.anchor, terminal.facing)
                .into_iter()
                .map(|at| (at.x, at.z))
                .collect::<BTreeSet<_>>();
            for at in &access {
                assert!(columns.contains(&(at.x, at.z)), "{at:?} is off the core");
                assert!(at.y <= canvas.max.y, "{at:?} is above the guard top");
            }
            assert!(declared.is_subset(&access));
        }
    }

    /// End to end: a trunk whose source sits in the shorter of two packed
    /// leaves climbs out through its own core column and routes past the
    /// taller sibling. Two trunks, so each is guided to a lane above the
    /// whole canvas and the climb is not optional.
    ///
    /// Under the old rule the source's core cells between its own leaf's top
    /// and the canvas top were guard keep-out in every map the router saw,
    /// and the trunk had no way up; the watcher records that they are open
    /// for the source's own search now, and the routes then prove the world.
    #[test]
    fn a_trunk_from_a_shorter_leaf_climbs_past_its_taller_sibling() {
        let net = netlist(
            &["x", "y"],
            &["b", "d"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["y"]),
                Gate::nor("d", &["c"]),
            ],
        );
        let (_, packed, children) = packed_chain(&net);
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        let (source_id, source_interface) = packed_endpoint(&packed, PortRole::Output, "a");
        let (shortened, short_top) = shorten_source_leaf(&packed, &source_id);
        let canvas_top = shortened.halo.iter().map(|at| at.y).max().unwrap();
        assert!(short_top < canvas_top, "the fixture must leave a gap");
        eprintln!("asymmetric packing: source leaf top {short_top}, canvas top {canvas_top}");

        // The source's core column above its own leaf: the cells the old rule
        // never opened.
        let band = terminal_access_cells_from(
            source_interface.pin.at,
            interface_direction(&source_interface),
            short_top + 1,
            canvas_top,
        )
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert!(!band.is_empty());
        let watcher = WatchingGuards {
            cells: band.clone(),
            unheld: std::cell::RefCell::new(BTreeSet::new()),
        };
        let routed = route_packed_trunks(
            &children,
            &shortened,
            &[request("a"), request("c")],
            &watcher,
            SearchConfig::checked_defaults().router_limits,
        )
        .expect("a trunk from the shorter leaf routes past its taller sibling");
        assert_eq!(routed.routes.len(), 2);
        assert_eq!(
            watcher.unheld.into_inner(),
            band,
            "the source's core column above its own leaf must open for its search"
        );

        // The trunk from the short leaf did climb above that leaf's own top
        // -- routes come back in sorted signal order, so `a` is first -- and
        // both chains still compute in the one world.
        let a = &routed.routes[0];
        let highest = a.owned_blocks().map(|block| block.at.y).max().unwrap();
        assert!(
            highest > short_top,
            "the short leaf's trunk never rose above its own leaf: highest {highest}, \
             leaf top {short_top}"
        );
        for (input, output) in [("x", "b"), ("y", "d")] {
            let input = packed_endpoint(&packed, PortRole::Input, input).1;
            let output = packed_endpoint(&packed, PortRole::Output, output).1;
            for bit in [false, true] {
                assert_eq!(
                    observe_packed(&routed.world, input.pin.at, output.pin.at, bit),
                    bit
                );
            }
        }
    }

    /// The two-trunk fixture: two independent chains, so two trunks that
    /// share one packed world and route one after the other.
    fn two_trunk_net() -> Netlist {
        netlist(
            &["x", "y"],
            &["b", "d"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["y"]),
                Gate::nor("d", &["c"]),
            ],
        )
    }

    /// The parent-frame terminal of one packed interface.
    fn terminal_of(packed: &PackedFreeLeaves, id: &FreeLeafInterfaceId) -> Terminal {
        let (leaf, interface, index) = packed_interface(packed, id).unwrap();
        packed_terminal(leaf, interface, index, id.endpoint).unwrap()
    }

    /// The cells of `terminal`'s mouth ring that nothing but the ring claims:
    /// inside the frame, in no halo, in no terminal's guard column. A probe
    /// on any other ring cell would watch a halo or a guard, not the ring.
    fn ring_cells_only_the_ring_holds(
        packed: &PackedFreeLeaves,
        terminals: &[Terminal],
        terminal: &Terminal,
        world_size: (i32, i32, i32),
    ) -> Vec<Anchor> {
        let top = packed.halo.iter().map(|at| at.y).max().unwrap() + 3;
        let guards = terminals
            .iter()
            .flat_map(|t| terminal_guard_cells(t.anchor, t.facing, top))
            .collect::<BTreeSet<_>>();
        mouth_ring(terminal)
            .into_iter()
            .filter(|at| {
                at.x >= 0
                    && at.y >= 0
                    && at.z >= 0
                    && at.x < world_size.0
                    && at.y < world_size.1
                    && at.z < world_size.2
                    && !packed.halo.contains(at)
                    && !guards.contains(at)
            })
            .collect()
    }

    /// Routes for real, and then lays one extra dust cell on `victim` as part
    /// of route 0: the earlier trunk sealing a later terminal's mouth, done
    /// deliberately rather than waiting for a packing that does it by chance.
    struct SealingRouter {
        victim: Anchor,
    }

    impl PhysicalRouter for SealingRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.route_guided_with_runways(request, None, ForcedTerminalRunways::default())
        }

        fn route_with_runways(
            &self,
            request: RouteRequest<'_>,
            runways: ForcedTerminalRunways,
        ) -> Result<RealisedRouteTree, RouterFailure> {
            self.route_guided_with_runways(request, None, runways)
        }

        fn route_guided_with_runways(
            &self,
            request: RouteRequest<'_>,
            guidance: Option<RouteGuidance>,
            runways: ForcedTerminalRunways,
        ) -> Result<RealisedRouteTree, RouterFailure> {
            let id = request.id;
            let mut tree =
                DurablePhysicalRouter.route_guided_with_runways(request, guidance, runways)?;
            if id == RouteId(0) {
                tree.cells.push(PlacedBlock {
                    at: self.victim,
                    state: crate::compile::dust(),
                });
            }
            Ok(tree)
        }
    }

    /// An earlier trunk cannot seal a later terminal's mouth.
    ///
    /// The mechanism measured on `segment_a`: the first trunk laid dust on
    /// the cell just past the second trunk's source runway, every core cell
    /// of that source was open and every guard stood, and the second search
    /// died on its own runway because the router will not enter a cell whose
    /// coupling ball holds a foreign conductor. Here the first trunk is made
    /// to lay exactly that dust. The mouth is the later endpoint's own now,
    /// so the commit is refused at that cell, by type, instead of the later
    /// trunk being reported unroutable.
    #[test]
    fn an_earlier_trunk_cannot_seal_a_later_terminals_mouth() {
        let net = two_trunk_net();
        let (_, packed, children) = packed_chain(&net);
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        // Sorted order routes `a` first, so `c`'s source is the later terminal.
        let (later_id, _) = packed_endpoint(&packed, PortRole::Output, "c");
        let later = terminal_of(&packed, &later_id);
        let victim = mouth_ring(&later)[1];
        // The target cell is genuinely nobody's but the mouth's: not a core
        // or access cell, not in any halo, so nothing else would have refused
        // the dust.
        assert!(!runway_core(later.anchor, later.facing).contains(&victim));
        assert!(!declared_terminal_access(&packed, &later_id, &later)
            .unwrap()
            .contains(&victim));
        assert!(!packed.halo.contains(&victim), "{victim:?} is a halo cell");

        let outcome = route_packed_trunks(
            &children,
            &packed,
            &[request("a"), request("c")],
            &SealingRouter { victim },
            SearchConfig::checked_defaults().router_limits,
        );
        match outcome {
            Err(PackedConnectionError::Overlap { at }) => assert_eq!(at, victim),
            other => panic!(
                "the sealing dust must be refused at the later terminal's mouth, got {:?}",
                other.map(|routed| routed.routes.len())
            ),
        }
    }

    /// A terminal's mouth ring is held for it until its own trunk searches,
    /// and opens then: the first trunk sees the second's mouth as the second
    /// endpoint's keep-out, the second trunk sees it open. And it is a ring,
    /// not more access: none of its cells is a core cell.
    #[test]
    fn a_terminals_mouth_ring_opens_only_for_its_own_trunk() {
        let net = two_trunk_net();
        let (_, packed, children) = packed_chain(&net);
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        let probes = ["a", "c"]
            .into_iter()
            .map(|signal| {
                let (id, _) = packed_endpoint(&packed, PortRole::Output, signal);
                let terminal = terminal_of(&packed, &id);
                let ring = mouth_ring(&terminal);
                let top = packed.halo.iter().map(|at| at.y).max().unwrap();
                let access = packed_terminal_access(&packed, &id, &terminal, top).unwrap();
                for at in ring {
                    assert!(
                        !access.contains(&at),
                        "{signal}: {at:?} is access, not ring"
                    );
                }
                (terminal.endpoint, ring[1])
            })
            .collect::<Vec<_>>();
        let (a_source, c_source) = (probes[0].0, probes[1].0);
        let watcher = WatchingAccess {
            held: std::cell::RefCell::new(Vec::new()),
            probes,
        };
        route_packed_trunks(
            &children,
            &packed,
            &[request("a"), request("c")],
            &watcher,
            SearchConfig::checked_defaults().router_limits,
        )
        .expect("two independent packed boundaries route");
        let held = watcher.held.into_inner();
        assert_eq!(held.len(), 2, "one call per trunk");
        assert!(
            !held[0].contains(&a_source),
            "a's own mouth must be open for a's search"
        );
        assert!(
            held[0].contains(&c_source),
            "c's mouth must be c's keep-out while a searches"
        );
        assert!(
            !held[1].contains(&c_source),
            "c's own mouth must be open for c's search"
        );
        assert!(
            !held[1].contains(&a_source),
            "a's mouth goes back to the parent once a is consumed"
        );
    }

    /// In a fanout, an earlier branch cannot occupy a later sink's mouth: the
    /// source's ring opens with its access, every sink's ring stays that
    /// sink's keep-out until the router starts that sink's own branch.
    #[test]
    fn a_fanout_branch_cannot_occupy_a_later_sinks_mouth() {
        let net = netlist(
            &["x"],
            &["b", "c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        );
        let (_, packed, children) = packed_chain(&net);
        let sources = packed_endpoints(&packed, PortRole::Output, "a");
        let sinks = packed_endpoints(&packed, PortRole::Input, "a");
        assert_eq!(sources.len(), 1);
        assert_eq!(sinks.len(), 2, "the fixture must be fanout");
        let request = PackedTrunkRequest {
            signal: "a".into(),
            source: sources[0].0.clone(),
            sinks: sinks.iter().map(|(id, _)| id.clone()).collect(),
        };
        // One probe per endpoint, on a ring cell only the ring holds -- a
        // ring cell under a halo or a guard would be watching those instead.
        let baseline = route_packed_trunks(
            &children,
            &packed,
            &[request.clone()],
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .expect("the fanout routes");
        let all = std::iter::once(&sources[0].0)
            .chain(sinks.iter().map(|(id, _)| id))
            .map(|id| terminal_of(&packed, id))
            .collect::<Vec<_>>();
        let probe = |id: &FreeLeafInterfaceId| {
            let terminal = terminal_of(&packed, id);
            let cells =
                ring_cells_only_the_ring_holds(&packed, &all, &terminal, baseline.world.size());
            let mouth = *cells
                .first()
                .unwrap_or_else(|| panic!("{:?} has no ring cell only the ring holds", id));
            assert!(!runway_core(terminal.anchor, terminal.facing).contains(&mouth));
            (terminal.endpoint, mouth)
        };
        let watcher = WatchingAccess {
            held: std::cell::RefCell::new(Vec::new()),
            probes: std::iter::once(probe(&sources[0].0))
                .chain(sinks.iter().map(|(id, _)| probe(id)))
                .collect(),
        };
        let source_endpoint = watcher.probes[0].0;
        let sink_endpoints = watcher.probes[1..]
            .iter()
            .map(|(endpoint, _)| *endpoint)
            .collect::<BTreeSet<_>>();

        route_packed_trunks(
            &children,
            &packed,
            &[request],
            &watcher,
            SearchConfig::checked_defaults().router_limits,
        )
        .expect("a packed fanout routes under the forced runway contract");
        let held = watcher.held.into_inner();
        assert_eq!(held.len(), 1, "one request is one call");
        assert!(
            !held[0].contains(&source_endpoint),
            "the source's mouth must be open for its search"
        );
        assert_eq!(
            held[0], sink_endpoints,
            "every sink's mouth must still be that sink's keep-out when the router is called"
        );
    }

    /// A root end keeps its mouth as it keeps its guard: held for the whole
    /// run when the root is declared, the caller's to cross when it is not.
    ///
    /// The two-trunk net with only `a` requested: `c`'s two interfaces are
    /// left for whoever packs this node next, and their mouths face into the
    /// frame -- the shape of a boundary the parent will route to later --
    /// rather than out of it, where the shell would hold them anyway.
    #[test]
    fn a_root_end_holds_its_mouth_ring() {
        let net = two_trunk_net();
        let (_, packed, children) = packed_chain(&net);
        let request = PackedTrunkRequest {
            signal: "a".into(),
            source: packed_endpoint(&packed, PortRole::Output, "a").0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, "a").0],
        };
        let limits = SearchConfig::checked_defaults().router_limits;
        let baseline = route_packed_trunks(
            &children,
            &packed,
            &[request.clone()],
            &DurablePhysicalRouter,
            limits,
        )
        .expect("the internal trunk routes");
        let trunk_ids = [
            packed_endpoint(&packed, PortRole::Output, "a").0,
            packed_endpoint(&packed, PortRole::Input, "a").0,
        ];
        let top = packed.halo.iter().map(|at| at.y).max().unwrap();
        let (sx, sy, sz) = baseline.world.size();
        let canvas = PackedCanvas {
            max: Anchor {
                x: sx - 1,
                y: sy - 1,
                z: sz - 1,
            },
        };
        let trunk_owned = trunk_ids
            .iter()
            .flat_map(|id| {
                let terminal = terminal_of(&packed, id);
                let access = packed_terminal_access(&packed, id, &terminal, top).unwrap();
                let egress = packed_egress(&terminal, None, canvas);
                access
                    .into_iter()
                    .chain(egress.owned())
                    .chain(egress.closure.iter().copied())
                    .collect::<Vec<_>>()
            })
            .collect::<BTreeSet<_>>();
        // The root end whose mouth has a cell inside the frame that nothing
        // else holds is the one that can show both directions.
        let (root_in, root, expected_open) = [
            packed_endpoint(&packed, PortRole::Input, "c").0,
            packed_endpoint(&packed, PortRole::Output, "c").0,
            packed_endpoint(&packed, PortRole::Input, "x").0,
            packed_endpoint(&packed, PortRole::Output, "b").0,
        ]
        .into_iter()
        .find_map(|id| {
            let root = terminal_of(&packed, &id);
            let mut all = trunk_ids
                .iter()
                .map(|id| terminal_of(&packed, id))
                .collect::<Vec<_>>();
            all.push(terminal_of(&packed, &id));
            let open = ring_cells_only_the_ring_holds(&packed, &all, &root, baseline.world.size())
                .into_iter()
                .filter(|at| !trunk_owned.contains(at))
                .collect::<BTreeSet<_>>();
            (!open.is_empty()).then_some((id, root, open))
        })
        .expect("a root end whose mouth has a cell only the root's claim holds");
        let ring = mouth_ring(&root).into_iter().collect::<BTreeSet<_>>();
        let watch = |roots: &[FreeLeafInterfaceId]| {
            let watcher = WatchingGuards {
                cells: ring.clone(),
                unheld: std::cell::RefCell::new(BTreeSet::new()),
            };
            route_packed_trunks_with_root_guards(
                &children,
                &packed,
                &[request.clone()],
                roots,
                &watcher,
                limits,
            )
            .expect("the internal trunk routes beside a root end");
            watcher.unheld.into_inner()
        };
        assert_eq!(
            watch(std::slice::from_ref(&root_in)),
            BTreeSet::new(),
            "a declared root end must hold its whole mouth ring"
        );
        assert_eq!(
            watch(&[]),
            expected_open,
            "without a declared root the mouth is the caller's to cross"
        );
    }

    /// With mouth rings held, request order still is not an input, and no
    /// trunk's route stands in another terminal's mouth in either order.
    #[test]
    fn mouth_rings_keep_foreign_routes_out_whatever_the_request_order() {
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;

        let net = two_trunk_net();
        let (_, packed, children) = packed_chain(&net);
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        let limits = SearchConfig::checked_defaults().router_limits;
        let route = |requests: &[PackedTrunkRequest]| {
            route_packed_trunks(&children, &packed, requests, &DurablePhysicalRouter, limits)
                .unwrap()
        };
        let forward = route(&[request("a"), request("c")]);
        let reverse = route(&[request("c"), request("a")]);
        assert_eq!(
            canonical_world_fingerprint(&forward.world),
            canonical_world_fingerprint(&reverse.world)
        );
        assert_eq!(forward.routes, reverse.routes);

        // Routes come back in sorted signal order: `a` then `c`. Each
        // terminal's ring is inside the world this composition built, and
        // the *other* trunk never stands in it.
        let (sx, sy, sz) = forward.world.size();
        for (signal, own, foreign) in [("a", 0usize, 1usize), ("c", 1, 0)] {
            for role in [PortRole::Output, PortRole::Input] {
                let (id, _) = packed_endpoint(&packed, role, signal);
                let ring = mouth_ring(&terminal_of(&packed, &id));
                for at in ring {
                    assert!(
                        at.x >= 0 && at.y >= 0 && at.z >= 0 && at.x < sx && at.y < sy && at.z < sz,
                        "{signal} {role:?} mouth {at:?} is outside the built world"
                    );
                    assert!(
                        !forward.routes[foreign]
                            .owned_blocks()
                            .any(|block| block.at == at),
                        "{signal} {role:?} mouth {at:?} holds the other trunk's block"
                    );
                }
                let _ = own;
            }
        }
    }

    /// A synthetic banded node: two halo boxes `gap` columns apart along
    /// `x`, and one east-bound trunk per `(source z, sink z)` pair, all at
    /// height 3.
    fn synthetic_band(
        gap: i32,
        pairs: &[(i32, i32)],
        third_between: bool,
    ) -> (PackedFreeLeaves, Vec<ResolvedPackedTrunk>) {
        let west = root_chunk_id(&netlist(&["p"], &["q"], vec![Gate::nor("q", &["p"])]));
        let east = root_chunk_id(&netlist(&["r"], &["s"], vec![Gate::nor("s", &["r"])]));
        let mid = root_chunk_id(&netlist(&["t"], &["u"], vec![Gate::nor("u", &["t"])]));
        let (west, east, mid) = (west.unwrap(), east.unwrap(), mid.unwrap());
        // Halos eight high: a lid five over the terminals at height three,
        // so a one- or two-layer band (6 or 9 columns) is cheaper than the
        // ten-cell climb the lanes would cost, and a four-layer one is not.
        let box_halo = |x0: i32, x1: i32| {
            let mut halo = BTreeSet::new();
            for x in x0..=x1 {
                for y in 0..=8 {
                    for z in 0..=30 {
                        halo.insert(Anchor { x, y, z });
                    }
                }
            }
            halo
        };
        let leaf = |chunk: &ChunkId, x0: i32, x1: i32| PackedFreeLeaf {
            chunk: chunk.clone(),
            translation: Anchor { x: x0, y: 0, z: 0 },
            interfaces: BTreeMap::new(),
            occupied: BTreeSet::new(),
            halo: box_halo(x0, x1),
            access: BTreeSet::new(),
        };
        let east_x0 = 10 + gap + 1;
        let mut placements = BTreeMap::from([
            (west.clone(), leaf(&west, 0, 10)),
            (east.clone(), leaf(&east, east_x0, east_x0 + 10)),
        ]);
        if third_between {
            placements.insert(mid.clone(), leaf(&mid, 12, 13));
        }
        let halo = placements
            .values()
            .flat_map(|leaf| leaf.halo.iter().copied())
            .collect();
        let packed = PackedFreeLeaves { placements, halo };
        let trunks = pairs
            .iter()
            .enumerate()
            .map(|(index, (source_z, sink_z))| {
                let slot = u32::try_from(index).unwrap();
                let source = Terminal {
                    endpoint: PhysicalEndpointId::DeclaredOutput(PortId(slot)),
                    anchor: Anchor {
                        x: 8,
                        y: 3,
                        z: *source_z,
                    },
                    facing: Facing::East,
                    support: Anchor {
                        x: 7,
                        y: 3,
                        z: *source_z,
                    },
                    target: None,
                };
                let sink = Terminal {
                    endpoint: PhysicalEndpointId::PrimaryInput(PortId(slot)),
                    anchor: Anchor {
                        x: east_x0 + 2,
                        y: 3,
                        z: *sink_z,
                    },
                    facing: Facing::West,
                    support: Anchor {
                        x: east_x0 + 3,
                        y: 3,
                        z: *sink_z,
                    },
                    target: None,
                };
                ResolvedPackedTrunk {
                    signal: format!("s{index}"),
                    strength: MAX_SIGNAL_STRENGTH,
                    source_id: Some(FreeLeafInterfaceId {
                        chunk: west.clone(),
                        endpoint: source.endpoint,
                    }),
                    source,
                    sinks: vec![(
                        Some(FreeLeafInterfaceId {
                            chunk: east.clone(),
                            endpoint: sink.endpoint,
                        }),
                        sink,
                    )],
                }
            })
            .collect();
        (packed, trunks)
    }

    /// Brute-force longest strictly decreasing subsequence of sink rows in
    /// source-row order: the number of layers the permutation needs.
    fn lds(pairs: &[(i32, i32)]) -> usize {
        let mut sorted = pairs.to_vec();
        sorted.sort();
        let sinks = sorted.iter().map(|(_, sink)| *sink).collect::<Vec<_>>();
        let mut best = vec![1usize; sinks.len()];
        for i in 0..sinks.len() {
            for j in 0..i {
                if sinks[j] > sinks[i] {
                    best[i] = best[i].max(best[j] + 1);
                }
            }
        }
        best.into_iter().max().unwrap_or(0)
    }

    /// The band plan is exactly the geometry rule: crossings whose sinks keep
    /// their source order share a layer, each inversion costs a layer, the
    /// count is the longest decreasing run, and no plan at all for a seam
    /// narrower than that needs, a third child standing in it, a fanout, or
    /// a band that does not pay for the climb it replaces.
    #[test]
    fn a_band_plan_follows_the_seam_width_and_the_crossing_order() {
        let one_layer = band_min_width(1).unwrap();
        let two_layers = band_min_width(2).unwrap();
        assert_eq!(
            one_layer,
            2 * (i32::try_from(TERMINAL_RUNWAY_CELLS).unwrap() + 1)
        );
        assert_eq!(two_layers, one_layer + PACKED_LANE_PITCH);

        // Ordered crossings on a one-layer band: every trunk on layer one.
        let (packed, trunks) = synthetic_band(one_layer, &[(2, 4), (8, 10), (14, 20)], false);
        let plan = band_plan(&packed, &trunks).expect("an ordered seam is banded");
        assert_eq!(plan.layers, vec![3, 3, 3]);
        assert!(plan
            .corridor_edge
            .iter()
            .all(|edge| *edge == 10 + one_layer + 1));

        // Nested, non-inverting spans share a layer: the sinks keep the
        // sources' order even though the spans overlap.
        let (packed, trunks) = synthetic_band(one_layer, &[(2, 14), (8, 20)], false);
        let plan = band_plan(&packed, &trunks).expect("nested ordered spans are one layer");
        assert_eq!(plan.layers, vec![3, 3]);

        // One inversion on a two-layer band: the inverted crossing climbs
        // one pitch, the others keep the first layer.
        let (packed, trunks) = synthetic_band(two_layers, &[(2, 10), (8, 4), (14, 20)], false);
        let plan = band_plan(&packed, &trunks).expect("one inversion with room is banded");
        assert_eq!(plan.layers, vec![3, 3 + PACKED_LANE_PITCH, 3]);

        // The same inversion on a one-layer band: not banded.
        let (packed, trunks) = synthetic_band(one_layer, &[(2, 10), (8, 4), (14, 20)], false);
        assert!(band_plan(&packed, &trunks).is_none());

        // A fully decreasing chain of k needs k layers, and a band that wide
        // no longer pays for the climb it replaces on this small lid.
        let chain = [(2, 20), (8, 14), (14, 8), (20, 2)];
        assert_eq!(band_layers(&chain), vec![0, 1, 2, 3]);
        assert_eq!(lds(&chain), 4);
        let (packed, trunks) = synthetic_band(band_min_width(4).unwrap(), &chain, false);
        assert!(
            band_plan(&packed, &trunks).is_none(),
            "a four-layer band does not pay"
        );

        // A third child in the seam: not banded, whatever the width.
        let (packed, trunks) = synthetic_band(two_layers, &[(2, 4)], true);
        assert!(band_plan(&packed, &trunks).is_none());

        // A fanout: not banded.
        let (packed, mut trunks) = synthetic_band(two_layers, &[(2, 4), (8, 10)], false);
        let extra = trunks.pop().unwrap().sinks.into_iter().next().unwrap();
        trunks[0].sinks.push(extra);
        assert!(band_plan(&packed, &trunks).is_none());

        // Halos in contact: not banded.
        let (packed, trunks) = synthetic_band(0, &[(2, 4)], false);
        assert!(band_plan(&packed, &trunks).is_none());
    }

    /// Every permutation of up to six sinks: the colouring uses exactly as
    /// many layers as the longest decreasing run, and on every layer the
    /// sinks rise with the sources.
    #[test]
    fn band_layer_count_is_the_longest_decreasing_run_on_every_small_permutation() {
        fn permutations(items: &[i32]) -> Vec<Vec<i32>> {
            if items.len() <= 1 {
                return vec![items.to_vec()];
            }
            let mut out = Vec::new();
            for i in 0..items.len() {
                let mut rest = items.to_vec();
                let head = rest.remove(i);
                for mut tail in permutations(&rest) {
                    tail.insert(0, head);
                    out.push(tail);
                }
            }
            out
        }
        for n in 1..=6usize {
            let rows = (0..n)
                .map(|i| i as i32 * PACKED_LANE_PITCH)
                .collect::<Vec<_>>();
            for sinks in permutations(&rows) {
                let pairs = rows
                    .iter()
                    .zip(&sinks)
                    .map(|(source, sink)| (*source, *sink))
                    .collect::<Vec<_>>();
                let layers = band_layers(&pairs);
                let count = layers.iter().copied().max().unwrap() + 1;
                assert_eq!(count, lds(&pairs), "{pairs:?}: {layers:?}");
                for layer in 0..count {
                    let mut last = None;
                    for (pair, assigned) in pairs.iter().zip(&layers) {
                        if *assigned != layer {
                            continue;
                        }
                        if let Some(last) = last {
                            assert!(pair.1 > last, "{pairs:?}: layer {layer} not increasing");
                        }
                        last = Some(pair.1);
                    }
                }
            }
        }
    }

    /// The cost gate compares the band's own width and second-layer climbs
    /// against the lane climbs it replaces, on the lid and terminal heights
    /// given, and takes the band only when strictly cheaper.
    #[test]
    fn the_band_cost_gate_refuses_a_band_that_does_not_pay() {
        let one_layer = band_min_width(1).unwrap();
        let two_layers = band_min_width(2).unwrap();
        // The climb the lanes cost the cheapest trunk is twice the lid's
        // height over the terminals; the band pays only below that.
        assert_eq!(band_pays(&[0, 0, 0], 3 + one_layer / 2, 3), Some(false));
        assert_eq!(band_pays(&[0, 0, 0], 3 + one_layer / 2 + 1, 3), Some(true));
        assert_eq!(band_pays(&[0, 1], 3 + two_layers / 2, 3), Some(false));
        assert_eq!(band_pays(&[0, 1], 3 + two_layers / 2 + 1, 3), Some(true));
        // The measured segment_a seam: six layers need 21 columns against a
        // climb of 2 * (11 - 3) = 16. Does not pay.
        assert_eq!(band_pays(&[0, 1, 2, 3, 4, 5], 11, 3), Some(false));
    }

    /// The two-chain net at a grain of two, packed with the band its seam
    /// derives: two children, two trunks across one seam.
    fn banded_two_chain() -> (PackedFreeLeaves, PackedChildWorld, Vec<String>) {
        let net = netlist(
            &["x", "y"],
            &["c", "d"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["y"]),
                Gate::nor("c", &["a"]),
                Gate::nor("d", &["b"]),
            ],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 2).unwrap();
        let contract = crate::compile::fragment_synth::allocation::SignalContract {
            polarity: SignalPolarity::Positive,
            strength: MAX_SIGNAL_STRENGTH,
            delay_budget_ticks: 4,
        };
        // The standard grid: the band geometry below is laid out for leaves
        // of that footprint, and it is the band being tested, not the grid.
        let leaves = chunks
            .iter()
            .map(|chunk| {
                synthesise_free_leaf(
                    chunk,
                    contract,
                    &SearchConfig::checked_defaults(),
                    &[crate::compile::fragment_synth::placement::STANDARD_PITCH],
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(leaves.len(), 2, "the grain must give two children");
        let driver = leaves
            .iter()
            .position(|leaf| leaf.netlist.outputs.contains(&"a".to_string()))
            .unwrap();
        let reader = 1 - driver;
        let mut bands = SeamBands::none();
        bands.set(
            &leaves[driver].chunk,
            &leaves[reader].chunk,
            band_min_width(2).unwrap(),
        );
        let order = [leaves[driver].chunk.clone(), leaves[reader].chunk.clone()];
        // The first ranked layout that puts the reader east of the driver
        // across the band -- a stacked layout points the runways away from
        // each other and is the kind of candidate production retries past.
        let driver_chunk = leaves[driver].chunk.clone();
        let reader_chunk = leaves[reader].chunk.clone();
        let mut first = None;
        let _ = search_ranked_layouts_in_order::<(), (), _>(
            &leaves,
            &order,
            PackingBudget::from_search(&SearchConfig::checked_defaults()),
            &bands,
            |_, packed| {
                let x_max = |chunk: &ChunkId| {
                    packed.placements[chunk]
                        .halo
                        .iter()
                        .map(|at| at.x)
                        .max()
                        .unwrap()
                };
                let x_min = |chunk: &ChunkId| {
                    packed.placements[chunk]
                        .halo
                        .iter()
                        .map(|at| at.x)
                        .min()
                        .unwrap()
                };
                if x_min(&reader_chunk) > x_max(&driver_chunk) {
                    first = Some(packed.clone());
                    LayoutVerdict::Accepted(())
                } else {
                    LayoutVerdict::Retry(())
                }
            },
        );
        let mut packed = first.expect("a ranked layout puts the reader east of the driver");
        let children = compose_packed_free_leaf_worlds(&leaves, &packed).unwrap();
        // A taller lid, as a real node's tallest sibling gives it: a column
        // of halo cells over one child, so the climb a lane would cost is
        // what the band is measured against. A halo is a keep-out mask, so
        // the composed world is untouched.
        let raise = &leaves[driver].chunk;
        let top = packed.halo.iter().map(|at| at.y).max().unwrap();
        let (x, z) = packed.placements[raise]
            .halo
            .iter()
            .map(|at| (at.x, at.z))
            .next()
            .unwrap();
        for y in top..=top + 2 * PACKED_LANE_PITCH {
            packed
                .placements
                .get_mut(raise)
                .unwrap()
                .halo
                .insert(Anchor { x, y, z });
            packed.halo.insert(Anchor { x, y, z });
        }
        (packed, children, vec!["a".into(), "b".into()])
    }

    /// Across a real band, both trunks are guided to terminal height, both
    /// chains compute, and the routes are the same whichever order the
    /// requests come in.
    #[test]
    fn banded_trunks_route_at_terminal_height_in_either_request_order() {
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;

        let (packed, children, signals) = banded_two_chain();
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        let limits = SearchConfig::checked_defaults().router_limits;
        let route = |requests: &[PackedTrunkRequest]| {
            route_packed_trunks(&children, &packed, requests, &DurablePhysicalRouter, limits)
                .expect("banded trunks route")
        };
        let forward = route(&[request(&signals[0]), request(&signals[1])]);
        let reverse = route(&[request(&signals[1]), request(&signals[0])]);
        assert_eq!(
            canonical_world_fingerprint(&forward.world),
            canonical_world_fingerprint(&reverse.world)
        );
        assert_eq!(forward.routes, reverse.routes);
        assert_eq!(forward.lanes, reverse.lanes);
        // Every lane is a band layer: a sink's own terminal height, or that
        // plus one pitch for a crossing on the second layer.
        let sink_heights = signals
            .iter()
            .map(|signal| packed_endpoint(&packed, PortRole::Input, signal).1.pin.at.y)
            .collect::<BTreeSet<_>>();
        for lane in &forward.lanes {
            let lane = lane.expect("a banded trunk has a layer");
            assert!(
                sink_heights.contains(&lane) || sink_heights.contains(&(lane - PACKED_LANE_PITCH)),
                "lane {lane} is not a band layer over sinks at {sink_heights:?}"
            );
        }
        for (input, output) in [("x", "c"), ("y", "d")] {
            let input = packed_endpoint(&packed, PortRole::Input, input).1;
            let output = packed_endpoint(&packed, PortRole::Output, output).1;
            for bit in [false, true] {
                assert_eq!(
                    observe_packed(&forward.world, input.pin.at, output.pin.at, bit),
                    bit
                );
            }
        }
    }

    /// An earlier band route cannot seal a later trunk's corridor across its
    /// own child: the corridor is that sink's to occupy, so a conductor laid
    /// there by the first trunk is refused at that cell.
    #[test]
    fn an_earlier_band_route_cannot_seal_a_later_sinks_corridor() {
        let (packed, children, signals) = banded_two_chain();
        let request = |signal: &str| PackedTrunkRequest {
            signal: signal.into(),
            source: packed_endpoint(&packed, PortRole::Output, signal).0,
            sinks: vec![packed_endpoint(&packed, PortRole::Input, signal).0],
        };
        // Sorted order routes the first signal first; the second's sink
        // corridor runs from its halo's first column to its mouth.
        let (later_id, later_interface) = packed_endpoint(&packed, PortRole::Input, &signals[1]);
        let later = terminal_of(&packed, &later_id);
        let edge = packed.placements[&later_id.chunk]
            .halo
            .iter()
            .map(|at| at.x)
            .min()
            .unwrap();
        let mouth = mouth_ring(&later)[1];
        // The corridor runs from the halo's first column to the mouth when
        // the mouth is inside the halo; when the mouth already stands in the
        // band, the mouth itself is the later sink's first owned cell.
        let victim = if mouth.x > edge + 1 {
            Anchor {
                x: (edge + 1 + mouth.x) / 2,
                y: later_interface.pin.at.y,
                z: later.anchor.z,
            }
        } else {
            mouth
        };
        assert!(
            !runway_core(later.anchor, later.facing).contains(&victim),
            "the victim must be past the runway, not on it"
        );
        match route_packed_trunks(
            &children,
            &packed,
            &[request(&signals[0]), request(&signals[1])],
            &SealingRouter { victim },
            SearchConfig::checked_defaults().router_limits,
        ) {
            Err(PackedConnectionError::Overlap { at }) => assert_eq!(at, victim),
            other => panic!(
                "a conductor in a later sink's corridor must be refused, got {:?}",
                other.map(|routed| routed.routes.len())
            ),
        }
    }

    /// A packed fanout: the source's access opens before the search, and every
    /// sink's stays shut until the router starts that sink's own branch.
    #[test]
    fn packed_fanout_opens_the_source_and_keeps_every_sink_guarded() {
        let net = netlist(
            &["x"],
            &["b", "c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        );
        let (_, packed, children) = packed_chain(&net);
        assert_eq!(
            packed.placements.len(),
            3,
            "the fixture must be three leaves"
        );
        let sources = packed_endpoints(&packed, PortRole::Output, "a");
        let sinks = packed_endpoints(&packed, PortRole::Input, "a");
        assert_eq!(sources.len(), 1);
        assert_eq!(sinks.len(), 2, "the fixture must be fanout");

        let request = PackedTrunkRequest {
            signal: "a".into(),
            source: sources[0].0.clone(),
            sinks: sinks.iter().map(|(id, _)| id.clone()).collect(),
        };
        // One probe per endpoint: its own anchor, which is one of the three
        // core access cells the parent holds for it.
        let probe = |id: &FreeLeafInterfaceId, interface: &ParentConnectableInterface| {
            let (leaf, iface, index) = packed_interface(&packed, id).unwrap();
            let terminal = packed_terminal(leaf, iface, index, id.endpoint).unwrap();
            (terminal.endpoint, interface.pin.at)
        };
        let watcher = WatchingAccess {
            held: std::cell::RefCell::new(Vec::new()),
            probes: std::iter::once(probe(&sources[0].0, &sources[0].1))
                .chain(sinks.iter().map(|(id, interface)| probe(id, interface)))
                .collect(),
        };
        let source_endpoint = watcher.probes[0].0;
        let sink_endpoints = watcher.probes[1..]
            .iter()
            .map(|(endpoint, _)| *endpoint)
            .collect::<BTreeSet<_>>();

        let routed = route_packed_trunks(
            &children,
            &packed,
            &[request],
            &watcher,
            SearchConfig::checked_defaults().router_limits,
        )
        .expect("a packed fanout routes under the forced runway contract");

        let held = watcher.held.into_inner();
        assert_eq!(held.len(), 1, "one request is one call");
        assert!(
            !held[0].contains(&source_endpoint),
            "the source's own access must be open for its search"
        );
        assert_eq!(
            held[0], sink_endpoints,
            "every sink's access must still be guarded when the router is called"
        );

        // Both branches still arrive on their own forced suffix, and the trunk
        // is one tree rather than two.
        let tree = &routed.routes[0];
        assert_eq!(tree.branches.len(), 2);
        for ((_, interface), branch) in sinks.iter().zip(&tree.branches) {
            let mut suffix = runway_core(interface.pin.at, interface_direction(interface));
            suffix.reverse();
            assert_eq!(branch.path[branch.path.len() - suffix.len()..], suffix[..]);
        }
        let shared = tree.branches[0]
            .path
            .iter()
            .filter(|at| tree.branches[1].path.contains(at))
            .count();
        assert!(shared >= 1, "a fanout must keep a shared trunk");
        let mut once = BTreeSet::new();
        assert!(
            tree.cells.iter().all(|block| once.insert(block.at)),
            "a shared runway is shared, not laid twice"
        );

        let input = packed_endpoint(&packed, PortRole::Input, "x").1;
        for signal in ["b", "c"] {
            let output = packed_endpoint(&packed, PortRole::Output, signal).1;
            for bit in [false, true] {
                assert_eq!(
                    observe_packed(&routed.world, input.pin.at, output.pin.at, bit),
                    bit,
                    "{signal} must follow x through the fanout"
                );
            }
        }
    }

    #[test]
    fn pending_egress_paths_and_shared_closures_survive_prior_consumption() {
        let endpoint_a = PhysicalEndpointId::PrimaryInput(PortId(40));
        let endpoint_b = PhysicalEndpointId::PrimaryInput(PortId(41));
        let terminal = |endpoint, x| Terminal {
            endpoint,
            anchor: Anchor { x, y: 1, z: 5 },
            facing: Facing::East,
            support: Anchor { x, y: 0, z: 5 },
            target: None,
        };
        let canvas = PackedCanvas {
            max: Anchor { x: 40, y: 8, z: 12 },
        };
        let mut first = packed_egress(&terminal(endpoint_a, 2), Some(6), canvas);
        let mut later = packed_egress(&terminal(endpoint_b, 20), Some(6), canvas);
        let shared_closure = Anchor { x: 15, y: 4, z: 5 };
        first.closure.insert(shared_closure);
        later.closure.insert(shared_closure);
        let all = vec![(0, first.clone()), (1, later.clone())];

        let mut reservations = PhysicalReservations::new();
        reserve_packed_egress(&mut reservations, &all);
        reserve_packed_egress_closures(&mut reservations, &all);

        // Every cell of the later endpoint's declared route to lane 6 is
        // endpoint-owned before the first router call; its closure is also
        // protected while trunk 0 runs.
        for at in later.owned() {
            assert_eq!(
                reservations.get(&at).map(|claim| claim.owner),
                Some(PhysicalReservationOwner::Endpoint(endpoint_b)),
                "pending egress cell {at:?}"
            );
        }
        assert!(matches!(
            reservations.get(&shared_closure),
            Some(claim)
                if claim.owner == PhysicalReservationOwner::Endpoint(endpoint_a)
                    && claim.kind == PhysicalReservationKind::KeepOut
        ));

        // Simulate trunk 0 consuming its endpoint path and releasing its
        // closure. The shared cell must be returned to the still-pending
        // endpoint before trunk 1 searches.
        for at in first.owned().chain(first.closure.iter().copied()) {
            reservations.release_endpoint_keep_out(at, endpoint_a);
        }
        reserve_packed_egress_closures(&mut reservations, &[(1, later.clone())]);
        assert!(matches!(
            reservations.get(&shared_closure),
            Some(claim)
                if claim.owner == PhysicalReservationOwner::Endpoint(endpoint_b)
                    && claim.kind == PhysicalReservationKind::KeepOut
        ));
        for at in later.owned() {
            assert_eq!(
                reservations.get(&at).map(|claim| claim.owner),
                Some(PhysicalReservationOwner::Endpoint(endpoint_b)),
                "later path was lost after earlier consumption at {at:?}"
            );
        }
    }

    #[test]
    fn lane_tracks_fill_the_band_and_exhaust_with_a_typed_error() {
        let corridor = Corridor {
            region: Prism {
                min: Anchor { x: 0, y: 0, z: 1 },
                max: Anchor { x: 10, y: 8, z: 10 },
            },
            capacity: 3,
        };
        assert_eq!(corridor.access_bands(), [1, 2, 9, 10]);
        assert_eq!(corridor.lane_band(), Some((3, 8)));
        assert_eq!(corridor.lane_capacity(), 3);
        assert_eq!(
            (0..3)
                .map(|i| lane_track(&corridor, i, "s").unwrap())
                .collect::<Vec<_>>(),
            [8, 6, 4]
        );
        assert!(matches!(
            lane_track(&corridor, 3, "s"),
            Err(ComposeError::CorridorLanesExhausted {
                lane: 3,
                lanes: 3,
                ..
            })
        ));
    }

    #[test]
    fn fanout_trunk_keeps_the_shared_lane_and_stays_supported_in_parent_space() {
        let net = netlist(
            &["x"],
            &["y", "z"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("y", &["a"]),
                Gate::nor("z", &["a"]),
            ],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 2).unwrap();
        let circuit = compose(
            &plan,
            &artifacts,
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .unwrap();
        let (index, trunk) = plan
            .trunks
            .iter()
            .enumerate()
            .find(|(_, trunk)| trunk.signal == "a")
            .expect("the producer-to-two-consumer trunk exists");
        let source = resolve(&plan, &trunk.signal, &trunk.source).unwrap();
        let sinks = trunk
            .sinks
            .iter()
            .map(|end| resolve(&plan, &trunk.signal, end).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(sinks.len(), 2, "the fixture must be fanout");
        let tree = &circuit.trunks[index];
        let track = lane_track(&plan.corridor, trunk.lane, &trunk.signal).unwrap();
        assert!(tree.cells.iter().any(|block| block.at.z == track));
        for block in tree.owned_blocks() {
            let terminal_cell = std::iter::once(&source).chain(&sinks).any(|terminal| {
                block.at == terminal.anchor
                    || block.at
                        == Anchor {
                            y: terminal.anchor.y - 1,
                            ..terminal.anchor
                        }
            });
            assert!(
                plan.corridor.region.contains(block.at)
                    || plan.children.iter().any(|child| child.in_halo(block.at))
                    || terminal_cell,
                "fanout block {:?} escaped parent space",
                block.at
            );
        }
        for block in &tree.cells {
            assert!(block.at.y > 0);
            assert_ne!(
                circuit
                    .world
                    .get(block.at.x, block.at.y - 1, block.at.z)
                    .kind,
                BlockKind::Air,
                "fanout block {:?} has no support",
                block.at
            );
        }
    }

    /// A lane at `y = 3` stands on a floor the router lays; an approach at
    /// `y = 1` stands on the world floor or a floor of its own.  Either way
    /// no conductor a trunk emits floats.
    #[test]
    fn every_emitted_trunk_conductor_stands_on_something() {
        let net = netlist(
            &["x"],
            &["y"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["b"]),
                Gate::nor("y", &["c"]),
            ],
        );
        let (_, circuit) = composed(&net);
        let mut checked = 0;
        for tree in &circuit.trunks {
            for block in &tree.cells {
                assert!(block.at.y > 0, "{:?} sits on the world boundary", block.at);
                let below = circuit.world.get(block.at.x, block.at.y - 1, block.at.z);
                assert_ne!(
                    below.kind,
                    BlockKind::Air,
                    "{:?} {:?} has nothing beneath it",
                    block.at,
                    block.state.kind
                );
                checked += 1;
            }
        }
        assert!(checked > 0);
    }

    /// One corridor with a portal row at `z = 41`, a source at `x = 20` and a
    /// sink at `x = 28`: the geometry both access-band tests read.
    fn access_band_fixture() -> (Corridor, Terminal, Terminal, Anchor) {
        let corridor = Corridor {
            region: Prism {
                min: Anchor { x: 0, y: 0, z: 1 },
                max: Anchor { x: 60, y: 8, z: 40 },
            },
            capacity: 20,
        };
        let source_anchor = Anchor { x: 20, y: 1, z: 41 };
        let sink_anchor = Anchor { x: 28, y: 1, z: 41 };
        let source = Terminal {
            endpoint: PhysicalEndpointId::PrimaryInput(PortId(0)),
            anchor: source_anchor,
            facing: Facing::North,
            support: Anchor {
                z: 42,
                ..source_anchor
            },
            target: None,
        };
        let connection = ConnectionId::External {
            instance: InstanceId(1),
            input_index: 0,
        };
        let sink = Terminal {
            endpoint: PhysicalEndpointId::Landing(connection),
            anchor: sink_anchor,
            facing: Facing::North,
            support: Anchor {
                z: 42,
                ..sink_anchor
            },
            target: Some(RouteTarget::Connection(connection)),
        };
        (corridor, source, sink, Anchor { x: 60, y: 8, z: 41 })
    }

    #[test]
    fn guided_trunk_releases_only_its_access_columns() {
        let (corridor, source, sink, far) = access_band_fixture();
        let source_anchor = source.anchor;
        let sink_anchor = sink.anchor;
        let guidance = RouteGuidance {
            origin: Anchor { x: 0, y: 0, z: 0 },
            lateral: Facing::South,
            track: 10,
            half_width: 0,
            access_half_width: ACCESS_HALF_WIDTH,
            preferred_y: Some(3),
            access_y: None,
            hard: true,
            penalty_per_block: 8,
        };
        let mut reservations = PhysicalReservations::new();
        for z in corridor.access_bands() {
            reserve_box(
                &mut reservations,
                &Prism {
                    min: Anchor { x: 0, y: 0, z },
                    max: Anchor { x: 60, y: 8, z },
                },
                ACCESS_BAND_KEEP_OUT,
            );
        }
        let endpoint =
            PhysicalReservationOwner::Endpoint(PhysicalEndpointId::PrimaryInput(PortId(9)));
        reservations.reserve(
            Anchor { x: 10, y: 1, z: 40 },
            endpoint,
            PhysicalReservationKind::KeepOut,
        );
        reservations.reserve(
            Anchor { x: 12, y: 1, z: 40 },
            PhysicalReservationOwner::Endpoint(PhysicalEndpointId::PrimaryInput(PortId(10))),
            PhysicalReservationKind::KeepOut,
        );
        reservations.reserve(
            Anchor { x: 14, y: 1, z: 40 },
            PhysicalReservationOwner::Route(RouteId(9)),
            PhysicalReservationKind::Conductor(crate::compile::dust()),
        );
        reservations.reserve(
            Anchor { x: 16, y: 1, z: 40 },
            PhysicalReservationOwner::RouteStair(RouteId(9)),
            PhysicalReservationKind::Floor(crate::compile::stone()),
        );
        release_access_band(
            &mut reservations,
            &corridor,
            far,
            guidance,
            &source,
            std::slice::from_ref(&sink),
        );
        for x in 0..=60 {
            let at = Anchor { x, y: 1, z: 40 };
            let in_column = x.abs_diff(source_anchor.x) <= ACCESS_HALF_WIDTH
                || x.abs_diff(sink_anchor.x) <= ACCESS_HALF_WIDTH;
            let preserved = matches!(x, 10 | 12 | 14 | 16);
            assert_eq!(reservations.get(&at).is_none(), in_column && !preserved);
        }
        assert!(reservations.get(&Anchor { x: 10, y: 1, z: 40 }).is_some());
        assert!(reservations.get(&Anchor { x: 12, y: 1, z: 40 }).is_some());
        assert!(reservations.get(&Anchor { x: 14, y: 1, z: 40 }).is_some());
        assert!(reservations.get(&Anchor { x: 16, y: 1, z: 40 }).is_some());
    }

    #[test]
    fn a_root_ended_trunk_releases_only_its_endpoint_windows() {
        let (corridor, source, sink, far) = access_band_fixture();
        let sinks = std::slice::from_ref(&sink);
        // Exactly the guidance `compose` now issues for every trunk: a lane of
        // its own across the corridor, with approaches at the lower portal
        // height.
        let guidance = RouteGuidance {
            origin: Anchor { x: 0, y: 0, z: 0 },
            lateral: Facing::South,
            track: 10,
            half_width: 0,
            access_half_width: ACCESS_HALF_WIDTH,
            preferred_y: Some(3),
            access_y: Some(1),
            hard: true,
            penalty_per_block: 8,
        };
        let mut reservations = PhysicalReservations::new();
        for z in corridor.access_bands() {
            reserve_box(
                &mut reservations,
                &Prism {
                    min: Anchor { x: 0, y: 0, z },
                    max: Anchor { x: 60, y: 8, z },
                },
                ACCESS_BAND_KEEP_OUT,
            );
        }
        release_access_band(&mut reservations, &corridor, far, guidance, &source, sinks);
        for z in corridor.access_bands() {
            for x in 0..=60 {
                let at = Anchor { x, y: 1, z };
                let in_window = x.abs_diff(source.anchor.x) <= ACCESS_HALF_WIDTH
                    || x.abs_diff(sink.anchor.x) <= ACCESS_HALF_WIDTH;
                assert_eq!(
                    reservations.get(&at).is_none(),
                    in_window,
                    "{at:?} release does not match its endpoint window"
                );
            }
        }

        // Regression: the East-lateral guidance root-ended trunks used to get
        // measured the band row against the portal `z`, which every cell on
        // that row sits within -- so the whole row was released and later
        // trunks lost the band that keeps their exits clear.
        let (start, goals) = guided_endpoints(&source, sinks);
        let band = corridor.access_bands()[3];
        let east = RouteGuidance {
            lateral: Facing::East,
            track: source.anchor.x,
            ..guidance
        };
        assert!(
            (0..=60).all(|x| {
                let at = Anchor { x, y: 1, z: band };
                goals.iter().any(|goal| east.allows(at, start, *goal))
            }),
            "the East guidance is expected to admit the entire band row"
        );
        assert!(
            (0..=60).any(|x| {
                let at = Anchor { x, y: 1, z: band };
                !goals.iter().any(|goal| guidance.allows(at, start, *goal))
            }),
            "a lane-guided trunk must still be refused most of the band row"
        );
    }

    /// A router that records what the map looked like when it was called, and
    /// can refuse instead of routing.
    struct Recording {
        /// Whether each call saw its own source guard released.
        saw_released: std::cell::RefCell<Vec<bool>>,
        /// Which of `probes` each call found *not* held by the access band.
        released_probes: std::cell::RefCell<Vec<BTreeSet<Anchor>>>,
        /// Access-band cells to look at on every call.
        probes: Vec<Anchor>,
        /// Refuse every call, so the error path is exercised.
        refuse: bool,
    }

    impl Recording {
        fn new(probes: Vec<Anchor>, refuse: bool) -> Self {
            Self {
                saw_released: std::cell::RefCell::new(Vec::new()),
                released_probes: std::cell::RefCell::new(Vec::new()),
                probes,
                refuse,
            }
        }
    }

    impl PhysicalRouter for Recording {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.route_guided(request, None)
        }

        fn route_guided(
            &self,
            request: RouteRequest<'_>,
            guidance: Option<RouteGuidance>,
        ) -> Result<RealisedRouteTree, RouterFailure> {
            // The source's own anchor is one of the guard cells this trunk
            // releases, so a released view has no endpoint keep-out there.
            let held = request
                .reservations
                .get(&request.source.anchor)
                .is_some_and(|claim| {
                    claim.owner == PhysicalReservationOwner::Endpoint(request.source.id)
                        && claim.kind == PhysicalReservationKind::KeepOut
                });
            self.saw_released.borrow_mut().push(!held);
            self.released_probes.borrow_mut().push(
                self.probes
                    .iter()
                    .copied()
                    .filter(|at| {
                        request
                            .reservations
                            .get(at)
                            .is_none_or(|claim| claim.owner != ACCESS_BAND_KEEP_OUT)
                    })
                    .collect(),
            );
            if self.refuse {
                return Err(RouterFailure::NoLocalRoute {
                    route: request.id,
                    source: request.source.id,
                    sink: request.sinks.as_slice()[0].id,
                });
            }
            DurablePhysicalRouter.route_guided(request, guidance)
        }
    }

    #[test]
    fn a_trunk_searches_a_released_view_and_leaves_the_master_restored() {
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;

        // Two independent inverters: four trunks whose windows sit far enough
        // apart that one trunk's is not inside another's.
        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 1).unwrap();
        let limits = SearchConfig::checked_defaults().router_limits;

        // Every cell of the corridor's access bands, at the portal height: the
        // band a trunk opens its own window through and every later trunk must
        // find closed again.
        let probes: Vec<Anchor> = plan
            .corridor
            .access_bands()
            .into_iter()
            .flat_map(|z| {
                (plan.corridor.region.min.x..=plan.corridor.region.max.x).map(move |x| Anchor {
                    x,
                    y: 1,
                    z,
                })
            })
            .collect();

        // Success: every trunk saw its own guard released, and the composition
        // is byte-identical to the one the real router produces.
        let recording = Recording::new(probes.clone(), false);
        let observed = compose(&plan, &artifacts, &recording, limits).unwrap();
        let seen = recording.saw_released.into_inner();
        assert_eq!(seen.len(), plan.trunks.len(), "every trunk must be routed");
        assert!(
            seen.iter().all(|released| *released),
            "a trunk must search a view with its own source guard released: {seen:?}"
        );
        // The heart of it: one trunk's released window must not still be open
        // when the next trunk is routed. Without the rollback the master keeps
        // every release, so each call would see a superset of the last one's.
        let released = recording.released_probes.into_inner();
        assert!(
            released.len() >= 2,
            "the fixture must route more than one trunk"
        );
        assert!(
            !released[0].is_empty(),
            "the first trunk must open a window through the band"
        );
        for (index, later) in released.iter().enumerate().skip(1) {
            assert!(
                !released[0].is_subset(later),
                "trunk {index} still saw trunk 0's band window open: the master \
                 map was not restored"
            );
        }

        let expected = compose(&plan, &artifacts, &DurablePhysicalRouter, limits).unwrap();
        assert_eq!(
            canonical_world_fingerprint(&observed.world),
            canonical_world_fingerprint(&expected.world),
            "routing through the released view must compose the identical world"
        );
        assert_eq!(observed.trunks, expected.trunks);

        // Refusal: the first trunk still saw the released view, and the error
        // is the ordinary typed refusal rather than anything the rollback
        // changed.
        let refusing = Recording::new(probes, true);
        let failure = compose(&plan, &artifacts, &refusing, limits).unwrap_err();
        assert!(
            refusing.saw_released.into_inner().first().copied() == Some(true),
            "the refused trunk must also have searched the released view"
        );
        assert!(
            matches!(failure, ComposeError::Route { ref signal, .. } if *signal == plan.trunks[0].signal),
            "expected the router's own refusal, got {failure:?}"
        );
    }

    #[test]
    fn cloning_a_map_mid_transaction_does_not_clone_the_transaction() {
        let endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
        let cell = Anchor { x: 3, y: 1, z: 4 };
        let mut master = PhysicalReservations::new();
        master.reserve(
            cell,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );
        let before = master.clone();

        master.begin_attempt();
        assert!(master.release_endpoint_keep_out(cell, endpoint));

        // What the router gets: the released view, and none of the caller's
        // open transaction -- so its own writes can never be rolled back by
        // the caller, nor the caller's releases undone by the router.
        let mut handed_to_router = master.clone();
        handed_to_router.reserve(
            cell,
            PhysicalReservationOwner::Route(RouteId(1)),
            PhysicalReservationKind::Conductor(crate::compile::dust()),
        );
        handed_to_router.rollback_attempt();
        assert_eq!(
            handed_to_router.get(&cell).map(|claim| claim.owner),
            Some(PhysicalReservationOwner::Route(RouteId(1))),
            "the clone had no journal, so rolling it back must do nothing"
        );

        master.rollback_attempt();
        assert_eq!(master, before, "the caller's own rollback must still work");
    }

    #[test]
    fn a_trunk_escape_is_rejected_with_its_signal_and_cell() {
        struct Escaping(Anchor);

        impl PhysicalRouter for Escaping {
            fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
                DurablePhysicalRouter.route(request)
            }

            fn route_guided(
                &self,
                request: RouteRequest<'_>,
                guidance: Option<RouteGuidance>,
            ) -> Result<RealisedRouteTree, RouterFailure> {
                let mut tree = DurablePhysicalRouter.route_guided(request, guidance)?;
                tree.cells.push(PlacedBlock {
                    at: self.0,
                    state: crate::compile::dust(),
                });
                Ok(tree)
            }
        }

        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 1).unwrap();
        let outside = Anchor {
            x: plan.corridor.region.max.x + 1,
            y: 1,
            z: plan.corridor.region.min.z + 2,
        };
        assert!(!plan.corridor.region.contains(outside));
        assert!(!plan.children.iter().any(|child| child.in_halo(outside)));
        assert_eq!(
            compose(
                &plan,
                &artifacts,
                &Escaping(outside),
                SearchConfig::checked_defaults().router_limits,
            )
            .unwrap_err(),
            ComposeError::RouteEscaped {
                signal: plan.trunks[0].signal.clone(),
                at: outside,
            }
        );
    }

    #[test]
    fn adjacent_portal_windows_keep_sibling_runways_guarded() {
        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        for child in &plan.children {
            for (left, right) in child.portals.iter().zip(child.portals.iter().skip(1)) {
                assert_eq!(left.pin.at.x.abs_diff(right.pin.at.x), 3);
            }
        }
        let artifacts = synthesise_children(&chunks, &plan, 1).unwrap();
        let circuit = compose(
            &plan,
            &artifacts,
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .unwrap();
        let ends = plan
            .trunks
            .iter()
            .map(|trunk| {
                Ok::<_, ComposeError>((
                    resolve(&plan, &trunk.signal, &trunk.source).unwrap(),
                    trunk
                        .sinks
                        .iter()
                        .map(|end| resolve(&plan, &trunk.signal, end).unwrap())
                        .collect::<Vec<_>>(),
                ))
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for ((trunk, tree), (source, sinks)) in plan.trunks.iter().zip(&circuit.trunks).zip(&ends) {
            assert!(
                tree.owned_blocks().all(|block| {
                    plan.corridor.region.contains(block.at)
                        || plan.children.iter().any(|child| child.in_halo(block.at))
                        || std::iter::once(source).chain(sinks).any(|terminal| {
                            block.at == terminal.anchor
                                || block.at
                                    == Anchor {
                                        y: terminal.anchor.y - 1,
                                        ..terminal.anchor
                                    }
                        })
                }),
                "trunk {} escaped its owned parent space",
                trunk.signal
            );
        }
    }

    /// Guarding is per-endpoint ownership, and portals sit one pitch apart, so
    /// a runway that spanned the access width would let the first endpoint on a
    /// row claim its neighbour's anchor and drop that guard when it routes.
    #[test]
    fn an_endpoint_never_guards_a_sibling_anchor_at_the_portal_pitch() {
        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        for child in &plan.children {
            for (left, right) in child.portals.iter().zip(child.portals.iter().skip(1)) {
                assert_eq!(
                    left.pin.at.x.abs_diff(right.pin.at.x),
                    3,
                    "the fixture must place portals exactly one pitch apart"
                );
            }
        }
        let far = plan.local_extent();
        let ends: Vec<(Terminal, Vec<Terminal>)> = plan
            .trunks
            .iter()
            .map(|trunk| {
                (
                    resolve(&plan, &trunk.signal, &trunk.source).unwrap(),
                    trunk
                        .sinks
                        .iter()
                        .map(|end| resolve(&plan, &trunk.signal, end).unwrap())
                        .collect(),
                )
            })
            .collect();
        let terminals: Vec<&Terminal> = ends
            .iter()
            .flat_map(|(source, sinks)| std::iter::once(source).chain(sinks))
            .collect();
        assert!(
            terminals.len() > 1,
            "the fixture must compose more than one trunk end"
        );

        for guard in &terminals {
            let runway: BTreeSet<Anchor> = guard_cells(guard, far.y).into_iter().collect();
            for other in &terminals {
                if other.endpoint == guard.endpoint {
                    continue;
                }
                assert!(
                    !runway.contains(&other.anchor),
                    "{:?} guards {:?}'s anchor at {:?}",
                    guard.endpoint,
                    other.endpoint,
                    other.anchor
                );
            }
        }

        // Reserve exactly as composition does, then take only the first trunk's
        // source release: every other end must still be held by itself.
        let mut reservations = PhysicalReservations::new();
        for (source, sinks) in &ends {
            for terminal in std::iter::once(source).chain(sinks) {
                for at in guard_cells(terminal, far.y) {
                    reservations.reserve(
                        at,
                        PhysicalReservationOwner::Endpoint(terminal.endpoint),
                        PhysicalReservationKind::KeepOut,
                    );
                }
            }
        }
        for terminal in &terminals {
            assert_eq!(
                reservations.get(&terminal.anchor).map(|held| held.owner),
                Some(PhysicalReservationOwner::Endpoint(terminal.endpoint)),
                "{:?}'s anchor is not held by itself",
                terminal.endpoint
            );
        }
        let (first, _) = &ends[0];
        for at in guard_cells(first, far.y) {
            reservations.release_endpoint_keep_out(at, first.endpoint);
        }
        for terminal in terminals
            .iter()
            .filter(|end| end.endpoint != first.endpoint)
        {
            assert_eq!(
                reservations.get(&terminal.anchor).map(|held| held.owner),
                Some(PhysicalReservationOwner::Endpoint(terminal.endpoint)),
                "{:?} lost its guard when the first source released",
                terminal.endpoint
            );
        }
    }

    fn netlist(inputs: &[&str], outputs: &[&str], gates: Vec<Gate>) -> Netlist {
        Netlist {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            gates,
        }
    }

    fn composed(net: &Netlist) -> (AllocationPlan, ComposedCircuit) {
        composed_with_workers(net, 2)
    }

    fn composed_with_workers(net: &Netlist, workers: usize) -> (AllocationPlan, ComposedCircuit) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), 1).unwrap();
        let plan = allocate(net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, workers).unwrap();
        let circuit = compose(
            &plan,
            &artifacts,
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .unwrap();
        (plan, circuit)
    }

    /// The tiny landed fixture: one inverter whose input is pinned on the north
    /// row facing south, and whose output is pinned four rows behind it facing
    /// east.  Two rows and two facings, so no caller row can hold them.
    fn landed_inverter() -> (Netlist, PortPlacements) {
        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let mut pins = PortPlacements::default();
        pins.pin("x", Anchor { x: 1, y: 1, z: 0 }, Facing::South);
        pins.pin("y", Anchor { x: 9, y: 1, z: 4 }, Facing::East);
        (net, pins)
    }

    /// A landed inverter whose input is pinned facing `toward`, well clear of
    /// its output: the three horizontal facings a source can leave along.
    fn landed_input(at: Anchor, toward: Facing) -> (Netlist, PortPlacements) {
        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let mut pins = PortPlacements::default();
        pins.pin("x", at, toward);
        pins.pin("y", Anchor { x: 20, y: 1, z: 4 }, Facing::East);
        (net, pins)
    }

    fn composed_with_pins(
        net: &Netlist,
        pins: &PortPlacements,
        workers: usize,
    ) -> (AllocationPlan, ComposedCircuit) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), 1).unwrap();
        let plan = allocate_with_root_ports(net, &chunks, LIMITS, Some(pins)).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, workers).unwrap();
        let circuit = compose(
            &plan,
            &artifacts,
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .unwrap();
        (plan, circuit)
    }

    fn observe(plan: &AllocationPlan, circuit: &ComposedCircuit, inputs: &[bool]) -> Vec<bool> {
        let mut world = circuit.world.clone();
        let mut input = inputs.iter().copied();
        for port in &plan.root_ports {
            let at = (port.pin.at.x, port.pin.at.y, port.pin.at.z);
            match port.role {
                PortRole::Input => drive_caller_cell(&mut world, at, input.next().unwrap()),
                PortRole::Output => probe_caller_cell(&mut world, at),
            }
        }
        assert!(input.next().is_none());
        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(400).unwrap();
        plan.root_ports
            .iter()
            .filter(|port| port.role == PortRole::Output)
            .map(|port| {
                simulator
                    .world()
                    .get(port.pin.at.x, port.pin.at.y, port.pin.at.z)
                    .lit
            })
            .collect()
    }

    #[test]
    fn odd_inversion_composes_without_shorting_input_to_output() {
        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let (plan, circuit) = composed(&net);
        assert_eq!(circuit.trunks.len(), 2);
        for (trunk, tree) in plan.trunks.iter().zip(&circuit.trunks) {
            assert_eq!(tree.branches.len(), trunk.sinks.len());
        }
        assert_eq!(observe(&plan, &circuit, &[false]), [true]);
        assert_eq!(observe(&plan, &circuit, &[true]), [false]);
    }

    #[test]
    fn fanout_shares_one_trunk_and_drives_both_outputs() {
        let net = netlist(
            &["x"],
            &["b", "c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        );
        let (plan, circuit) = composed(&net);
        let a = plan.trunks.iter().position(|t| t.signal == "a").unwrap();
        assert_eq!(circuit.trunks[a].branches.len(), 2);
        assert_eq!(observe(&plan, &circuit, &[false]), [false, false]);
        assert_eq!(observe(&plan, &circuit, &[true]), [true, true]);
    }

    #[test]
    fn independent_inputs_do_not_couple_in_the_shared_corridor() {
        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let (plan, circuit) = composed(&net);
        // The colouring puts more than one trunk on a lane here, which is the
        // case worth observing: two signals cross the corridor on one row and
        // still neither shorts nor drives the other.
        let lanes: BTreeSet<u32> = plan.trunks.iter().map(|trunk| trunk.lane).collect();
        assert!(
            lanes.len() < plan.trunks.len(),
            "{} trunks on {} lanes: the fixture must share a lane",
            plan.trunks.len(),
            lanes.len()
        );
        assert_eq!(observe(&plan, &circuit, &[false, true]), [true, false]);
        assert_eq!(observe(&plan, &circuit, &[true, false]), [false, true]);
    }

    #[test]
    fn no_two_trunks_share_a_conductor_cell() {
        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let (_, circuit) = composed(&net);
        assert!(
            circuit.trunks.len() > 1,
            "the fixture must route more trunks"
        );

        // Every trunk commits against the master reservation map, so a cell one
        // trunk conducts through is never handed to another, and the block
        // standing there is the one that trunk laid.
        let mut owner: BTreeMap<Anchor, RouteId> = BTreeMap::new();
        for tree in &circuit.trunks {
            for block in &tree.cells {
                if let Some(earlier) = owner.insert(block.at, tree.id) {
                    panic!(
                        "{:?} carries both {:?} and {:?}",
                        block.at, earlier, tree.id
                    );
                }
                assert_eq!(
                    circuit.world.get(block.at.x, block.at.y, block.at.z),
                    &block.state,
                    "{:?} does not hold the block {:?} laid",
                    block.at,
                    tree.id
                );
            }
        }
    }

    #[test]
    fn root_composition_is_worker_invariant_and_inverts_both_inputs() {
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;

        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let (serial_plan, serial) = composed_with_workers(&net, 1);
        let (parallel_plan, parallel) = composed_with_workers(&net, 4);
        assert_eq!(serial_plan, parallel_plan);
        assert_eq!(
            canonical_world_fingerprint(&serial.world),
            canonical_world_fingerprint(&parallel.world)
        );
        for (inputs, expected) in [
            ([false, false], [true, true]),
            ([false, true], [true, false]),
            ([true, false], [false, true]),
            ([true, true], [false, false]),
        ] {
            assert_eq!(observe(&serial_plan, &serial, &inputs), expected);
            assert_eq!(observe(&serial_plan, &parallel, &inputs), expected);
        }
    }

    #[test]
    fn malformed_artifact_sets_are_typed() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 1).unwrap();
        let limits = SearchConfig::checked_defaults().router_limits;
        assert_eq!(
            compose(&plan, &artifacts[..1], &DurablePhysicalRouter, limits).err(),
            Some(ComposeError::MissingArtifact {
                chunk: plan.children[1].chunk.clone()
            })
        );
        let mut doubled = artifacts.clone();
        doubled.push(artifacts[0].clone());
        assert_eq!(
            compose(&plan, &doubled, &DurablePhysicalRouter, limits).err(),
            Some(ComposeError::DuplicateArtifact {
                chunk: artifacts[0].chunk.clone()
            })
        );
    }

    #[test]
    fn an_unpinned_root_is_still_a_caller_row_and_lands_nothing() {
        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let (plan, _) = composed(&net);
        assert_eq!(plan.root_placement.access, RootAccess::CallerRow);
        assert_eq!(plan.root_placement.landed_region(), None);
        assert_eq!(plan.caller_row_z(), 0);
        assert_eq!(plan.corridor.region.min.z, 1);
        for port in &plan.root_ports {
            assert_eq!(port.pin.at.z, 0, "a caller-row port stays on the row");
        }
    }

    #[test]
    fn a_landed_root_inverts_through_two_rows_and_two_facings() {
        let (net, pins) = landed_inverter();
        let (plan, circuit) = composed_with_pins(&net, &pins, 2);
        assert!(matches!(
            plan.root_placement.access,
            RootAccess::Landed { .. }
        ));
        // The pins are exactly where the caller put them.
        for port in &plan.root_ports {
            assert_eq!(Some(port.pin), pins.get(&port.signal));
        }
        assert_eq!(observe(&plan, &circuit, &[false]), [true]);
        assert_eq!(observe(&plan, &circuit, &[true]), [false]);
    }

    #[test]
    fn a_landed_pin_keeps_its_own_cell_and_neighbours_empty() {
        let (net, pins) = landed_inverter();
        let (plan, circuit) = composed_with_pins(&net, &pins, 2);
        for port in &plan.root_ports {
            let handover = port.pin.handover(port.role);
            for at in pin_isolation(&port.pin, port.role) {
                assert_ne!(at, handover, "the handover is never isolated");
                assert_eq!(
                    circuit.world.get(at.x, at.y, at.z).kind,
                    BlockKind::Air,
                    "{:?} beside pinned {} is not empty",
                    at,
                    port.signal
                );
            }
            // The one cell this contract does build against the pin.
            assert_ne!(
                circuit.world.get(handover.x, handover.y, handover.z).kind,
                BlockKind::Air,
                "the handover for {} was never built",
                port.signal
            );
        }
    }

    #[test]
    fn a_landed_root_keeps_every_child_on_its_own_caller_row() {
        let (net, pins) = landed_inverter();
        let (plan, _) = composed_with_pins(&net, &pins, 2);
        for child in &plan.children {
            let rows: BTreeSet<i32> = child.portals.iter().map(|p| p.pin.at.z).collect();
            assert_eq!(
                rows.len(),
                1,
                "child {:?} portals are not on one row",
                child.chunk
            );
            // A child is pinned by this parent, so its own contract is the
            // ordinary caller-row one: nothing nested ever lands.
            let placement =
                root_placement(&netlist(&["p"], &["q"], vec![Gate::nor("q", &["p"])]), None)
                    .unwrap();
            assert_eq!(placement.access, RootAccess::CallerRow);
        }
    }

    #[test]
    fn a_landed_composition_is_worker_invariant() {
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;

        let (net, pins) = landed_inverter();
        let (serial_plan, serial) = composed_with_pins(&net, &pins, 1);
        let (parallel_plan, parallel) = composed_with_pins(&net, &pins, 4);
        assert_eq!(serial_plan, parallel_plan);
        assert_eq!(
            canonical_world_fingerprint(&serial.world),
            canonical_world_fingerprint(&parallel.world)
        );
    }

    #[test]
    fn a_landed_input_leaves_along_every_horizontal_facing_without_escaping() {
        // A source may only leave its anchor along its own facing, so the cell
        // past the net cell is the router's, not the caller's. Each of these
        // would route outside the reserved space if that cell were left out of
        // the placement -- and `compose` would refuse with RouteEscaped.
        for (toward, at) in [
            (Facing::North, Anchor { x: 5, y: 1, z: 10 }),
            (Facing::East, Anchor { x: 5, y: 1, z: 10 }),
            (Facing::West, Anchor { x: 8, y: 1, z: 10 }),
        ] {
            let (net, pins) = landed_input(at, toward);
            let placement = root_placement(&net, Some(&pins)).unwrap();
            let RootAccess::Landed { region } = placement.access else {
                panic!("{toward:?} at {at:?} must land");
            };
            let exit = match toward {
                Facing::North => Anchor { z: at.z - 3, ..at },
                Facing::East => Anchor { x: at.x + 3, ..at },
                Facing::West => Anchor { x: at.x - 3, ..at },
                other => panic!("{other:?} is not horizontal"),
            };
            assert!(
                region.contains(exit),
                "{toward:?} source exit {exit:?} is outside {region:?}"
            );

            let (plan, circuit) = composed_with_pins(&net, &pins, 2);
            assert_eq!(
                observe(&plan, &circuit, &[false]),
                [true],
                "{toward:?} input does not invert"
            );
            assert_eq!(observe(&plan, &circuit, &[true]), [false]);
        }
    }

    #[test]
    fn landed_pins_too_close_to_share_hardware_are_refused_by_type() {
        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let mut pins = PortPlacements::default();
        // One row apart: the input's own runway would stand beside the
        // output's handover.
        pins.pin("x", Anchor { x: 5, y: 1, z: 10 }, Facing::East);
        pins.pin("y", Anchor { x: 6, y: 1, z: 11 }, Facing::East);
        match root_placement(&net, Some(&pins)) {
            Err(AllocationError::RootPinsTooClose {
                first,
                second,
                apart,
                minimum,
                ..
            }) => {
                assert_eq!((first.as_str(), second.as_str()), ("x", "y"));
                assert!(apart < minimum, "{apart} is not closer than {minimum}");
            }
            other => panic!("expected a spacing refusal, got {other:?}"),
        }
    }
}
