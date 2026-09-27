//! One packed parent node, derived from certified free leaves.
//!
//! The recursive production path still allocates a fixed region per child and
//! joins them through a corridor.  This module builds the other shape: the
//! children are certified on their own terms, packed by translation only, and
//! joined by parent trunks that the existing forced-runway router lays between
//! their automatic terminals. Shared inputs get a parent-owned handover
//! repeater; the same router fans its output out to the child terminals.
//!
//! Single-reader primary inputs and outputs reuse the child caller cells.
//! A shared primary input has one node-owned caller cell and a routed fanout.
//! Outputs also read inside and pass-through signals remain typed refusals.
//!
//! Because the boundary is that shape, a certified node is already a
//! parent-connectable artifact: [`into_parent_connectable`] re-keys its ports
//! onto node-owned endpoints and derives the same masks a leaf exposes, so the
//! next level up packs and routes it with no knowledge that it is a node.  The
//! recursion is therefore a fixed point in one type, [`FreeLeafArtifact`],
//! rather than a hierarchy of its own.
//!
//! The boundary it does expose is checked, before certification, against the
//! contract a packed parent would hold it to: child-built handover hardware
//! behind every root port, a clear straight runway in front of it, and one
//! portal pitch of space between any two.  It is deliberately *not* checked
//! against `allocation::root_pin_row`, the caller-row contract, and it does
//! not meet it: measured on the fanout fixture, the ports land at `y = 3`
//! rather than `PORTAL_Y`, the input row is `z = 8` while the output row is
//! `z = 2`, and every pin faces east.  Translation-only packing cannot fix
//! that -- a leaf's pin facing and height are fixed by the interior its own
//! planner certified, and a caller row would need rotation and a shared
//! plane.  Nesting a packed node inside an *allocating* parent therefore
//! remains open; nesting one inside another packed node is what this checks.
//!
//! The composed world is certified by the shipping authority,
//! [`certify_root_world`], against the node's own netlist.  That is only
//! honest if the children's gates really are the node's gates, which is what
//! the exact-cover check below proves before a single vector is simulated.

// Crate-private until the public synthesis API unfreezes at Gate 3, exactly as
// `leaf`, `packing` and `parent` are.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::allocation::{
    boundary_signal_uses, plane_apart, AllocationError, ChildBoundary, RootPort, SignalContract,
    SignalUse, TrunkOwner, PORTAL_PITCH,
};
use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use crate::compile::fragment_synth::certification::{
    certify_root_world, CandidateCertificationError, CertificationWorkers, RootCertificate,
};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::identity::{PhysicalEndpointId, PortId};
use crate::compile::fragment_synth::leaf::{
    interface_route_direction, parent_connectable_masks, FreeLeafArtifact, FreeLeafInterfaceId,
    GateCoverageError, GateMetadata, GateMetadataError, ParentConnectableInterface,
};
use crate::compile::fragment_synth::packing::{
    compose_packed_free_leaf_worlds, envelope_score, search_ranked_layouts_in_order,
    translate_artifact, LayoutSearchError, LayoutVerdict, PackedFreeLeaves,
    PackedWorldCompositionError, PackingBudget, PackingError, PlacementOrderError, SeamBands,
};
use crate::compile::fragment_synth::fabric::{face_reach, pad_reach, Layers, PITCH};
use crate::compile::fragment_synth::parent::{
    band_layers, band_min_width, band_pays, route_packed_trunks_fabric,
    route_packed_trunks_with_inputs, PackedConnectionError,
    PackedInputTrunkRequest, PackedOutputExport, PackedTrunkRequest, PACKED_LANE_PITCH,
};
use crate::compile::fragment_synth::partition::{
    canonical_order, node_chunk_id, ChunkId, PartitionError,
};
use crate::compile::fragment_synth::terminal_geometry::{
    runway_core, terminal_guard_cells, terminal_guard_cells_from, TERMINAL_RUNWAY_CELLS,
};
use crate::compile::geometry::Anchor;
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::planner::{PortPlacements, PortRole};
use crate::compile::routing::{PhysicalRouter, RealisedRouteTree};
use crate::compile::Netlist;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

/// A node primary port, resolved to the packed cell a caller drives or reads.
///
/// `interface` records the child owner when the port borrows a child terminal.
/// A shared input instead has a node-owned handover and no child identity.
/// Both carry their actual signal contract into the next recursive level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedRootPort {
    pub port: RootPort,
    /// None means this node built the handover, not one of its children.
    pub interface: Option<FreeLeafInterfaceId>,
    pub contract: SignalContract,
}

/// A packed node: one world, its parent trunks, its boundary, its certificate.
#[derive(Debug, Clone)]
pub(crate) struct PackedNode {
    /// The netlist this node was certified against.  Kept so the node can be
    /// handed on as one child without the caller having to carry it alongside.
    pub netlist: Netlist,
    pub world: World,
    pub packed: PackedFreeLeaves,
    /// One realised tree per internal boundary signal, in the router's stable
    /// signal order.
    pub trunks: Vec<RealisedRouteTree>,
    /// The boundary signal each trunk carries, in the same order as `trunks`.
    pub trunk_signals: Vec<String>,
    /// Lane height per trunk, in stable route order. `None` means this node has
    /// a single route and therefore needs no lane separation.
    pub trunk_lanes: Vec<Option<i32>>,
    /// Physical route measurements retained only for in-memory test reports.
    #[cfg(test)]
    pub trunk_physical_metrics: Vec<PackedTrunkPhysicalMetrics>,
    #[cfg(test)]
    pub trunk_physical_totals: PackedTrunkPhysicalTotals,
    /// Candidate ranking and route evidence retained only for unit tests.
    #[cfg(test)]
    pub candidate_scores: Vec<PackedCandidateScoreRow>,
    /// The node's ports: every declared input in order, then every declared
    /// output in order, which is the contract `certify_root_world` validates.
    pub root_ports: Vec<PackedRootPort>,
    pub certificate: RootCertificate,
    /// Every gate this node contains, in this node's own frame: each child's
    /// metadata translated by the packing translation, exactly once.
    pub gates: GateMetadata,
    /// Which ranked layout was accepted.  Zero is the layout
    /// `pack_free_leaves` alone would have produced; anything higher is
    /// evidence that the greedy placement was refused and a later one was
    /// taken.  It is evidence, not identity: the fingerprint below is the
    /// world and the trunks, so two runs that reach the same world by
    /// different ranks would be a bug this field makes visible rather than a
    /// difference the fingerprint hides.
    pub layout_rank: usize,
    /// Stable identity of this node: its world and the trunks laid into it.
    pub fingerprint: Fingerprint,
}

/// One layout's routed, boundary-checked result, before certification.
struct LayoutOutcome {
    packed: PackedFreeLeaves,
    world: World,
    trunks: Vec<RealisedRouteTree>,
    trunk_signals: Vec<String>,
    trunk_lanes: Vec<Option<i32>>,
    root_ports: Vec<PackedRootPort>,
    gates: GateMetadata,
}

/// One stable source-to-sink portal pair whose physical separation affects
/// the parent trunk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PortalDemand {
    signal: String,
    source: FreeLeafInterfaceId,
    sink: FreeLeafInterfaceId,
}

/// Lexicographic, weight-free proxy for how much parent-owned wire a packing
/// candidate is likely to need. Routing remains the acceptance authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PortalLayoutScore {
    pub sum_trunk_max_manhattan: u64,
    pub total_sink_manhattan: u64,
    pub max_abs_dz: u64,
    pub sum_abs_dz: u64,
    pub envelope: Vec<(u128, i32, i32, i32, i32)>,
    pub original_rank: usize,
    pub translations: Vec<(ChunkId, i32, i32, i32)>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedCandidateScoreRow {
    pub original_rank: usize,
    pub score: PortalLayoutScore,
    /// `None` means score-ranked but not reached after an earlier candidate
    /// routed; `Some(false)` is a router or boundary refusal.
    pub routed: Option<bool>,
    pub trunk_metrics: Option<Vec<PackedTrunkPhysicalMetrics>>,
    pub trunk_totals: Option<PackedTrunkPhysicalTotals>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PackedTrunkPhysicalMetrics {
    pub cells: usize,
    pub floors: usize,
    pub repeaters: usize,
    pub vertical_risers: usize,
    pub lane: Option<i32>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PackedTrunkPhysicalTotals {
    pub cells: usize,
    pub floors: usize,
    pub repeaters: usize,
    pub vertical_risers: usize,
}

#[cfg(test)]
fn physical_metrics(
    trunks: &[RealisedRouteTree],
    lanes: &[Option<i32>],
) -> (Vec<PackedTrunkPhysicalMetrics>, PackedTrunkPhysicalTotals) {
    let metrics = trunks
        .iter()
        .enumerate()
        .map(|(index, trunk)| {
            let mut vertical_edges = BTreeSet::new();
            for branch in &trunk.branches {
                for pair in branch.path.windows(2) {
                    if pair[0].y != pair[1].y {
                        vertical_edges.insert((pair[0], pair[1]));
                    }
                }
            }
            PackedTrunkPhysicalMetrics {
                cells: trunk.cells.len(),
                floors: trunk.floors.len(),
                repeaters: trunk
                    .cells
                    .iter()
                    .filter(|block| block.state.kind == BlockKind::Repeater)
                    .count(),
                vertical_risers: vertical_edges.len(),
                lane: lanes.get(index).copied().flatten(),
            }
        })
        .collect::<Vec<_>>();
    let totals = metrics
        .iter()
        .fold(PackedTrunkPhysicalTotals::default(), |mut all, metric| {
            all.cells += metric.cells;
            all.floors += metric.floors;
            all.repeaters += metric.repeaters;
            all.vertical_risers += metric.vertical_risers;
            all
        });
    (metrics, totals)
}

#[derive(Debug, Error)]
pub(crate) enum PackedNodeError {
    #[error("a packed node needs at least one certified child")]
    NoChildren,
    #[error("child {chunk:?} was given more than once")]
    DuplicateChild { chunk: ChunkId },
    #[error("child {chunk:?} declares {role:?} ports {actual:?}, but exposes {expected:?}")]
    ChildInterfaceContract {
        chunk: ChunkId,
        role: PortRole,
        expected: Vec<String>,
        actual: Vec<String>,
    },
    #[error("child {chunk:?} certified gate {gate}, which this node does not declare")]
    ForeignGate { chunk: ChunkId, gate: String },
    #[error("child {chunk:?} certified a different gate for {gate}")]
    GateMismatch { chunk: ChunkId, gate: String },
    #[error("gate {gate} is certified by both {first:?} and {second:?}")]
    GateCoveredTwice {
        gate: String,
        first: ChunkId,
        second: ChunkId,
    },
    #[error("no child certified gate {gate}")]
    GateUncovered { gate: String },
    #[error("child {chunk:?} has no {role:?} interface for {signal}")]
    MissingInterface {
        chunk: ChunkId,
        signal: String,
        role: PortRole,
    },
    #[error("child {chunk:?} has more than one {role:?} interface for {signal}")]
    AmbiguousInterface {
        chunk: ChunkId,
        signal: String,
        role: PortRole,
    },
    #[error("a packed node has no child for {chunk:?}")]
    MissingChild { chunk: ChunkId },
    #[error("{signal} is both an input and an output of this node")]
    PassThroughPort { signal: String },
    #[error("this node declares {role:?} port {signal} more than once")]
    DuplicatePort { signal: String, role: PortRole },
    #[error("this node has more than u32::MAX ports")]
    NodePortIndexOverflow,
    #[error("the band a seam of this node needs does not fit an i32")]
    BandOverflow,
    #[error("declared {role:?} port {signal} resolved to no packed cell")]
    UnresolvedPort { signal: String, role: PortRole },
    #[error("all {attempted} packed layouts were refused; the first refused: {rank_zero}")]
    LayoutsExhausted {
        attempted: usize,
        rank_zero: Box<PackedNodeError>,
    },
    #[error("the layout budget stopped after {attempted} layouts, before one was complete")]
    LayoutBudgetExhausted { attempted: usize },
    #[error("the children of this node depend on each other in a cycle: {chunks:?}")]
    ChildDependencyCycle { chunks: Vec<ChunkId> },
    #[error(transparent)]
    PlacementOrder(#[from] PlacementOrderError),
    #[error("child {chunk:?} gate metadata does not translate: {error}")]
    GateMetadata {
        chunk: ChunkId,
        #[source]
        error: GateMetadataError,
    },
    #[error(transparent)]
    GateCoverage(#[from] GateCoverageError),
    #[error("the node world does not match its certificate: {certified:?} vs {actual:?}")]
    UncertifiedWorld {
        certified: Fingerprint,
        actual: Fingerprint,
    },
    #[error("this node's world has nothing in it to pack")]
    EmptyNodeWorld,
    #[error("node port {signal} lands at {at:?}, which the derived access does not cover")]
    NodePortOutsideAccess { signal: String, at: Anchor },
    #[error("root port {signal} is routed {toward:?}, out of the plane a parent routes in")]
    RootPortOutOfPlane { signal: String, toward: Facing },
    #[error("a layout {needs:?} wide does not fit the {room:?} its pins leave")]
    PinnedRegionTooSmall { needs: (i32, i32), room: (i32, i32) },
    #[error("the pins leave {room:?} (x by z) to build in, but {space}")]
    PinnedSpaceShort { room: (i32, i32), space: PinnedSpaceShortfall },
    #[error("root port {signal} has no handover hardware at {at:?}")]
    RootPortHandoverUnbuilt { signal: String, at: Anchor },
    #[error("root port {signal} needs runway cell {at:?}, which is outside the node")]
    RootPortRunwayEscapes { signal: String, at: Anchor },
    #[error("root port {signal} runway cell {at:?} already holds {kind:?}")]
    RootPortRunwayBlocked {
        signal: String,
        at: Anchor,
        kind: BlockKind,
    },
    #[error("root ports {first} and {second} stand {apart} apart, closer than one portal pitch")]
    RootPortsTooClose {
        first: String,
        second: String,
        apart: i64,
    },
    #[error("root port {first} guards {at:?}, which is root port {second}'s own runway")]
    RootPortGuardsAnother {
        first: String,
        second: String,
        at: Anchor,
    },
    #[error(transparent)]
    Node(#[from] PartitionError),
    #[error(transparent)]
    Connectivity(#[from] AllocationError),
    #[error(transparent)]
    Packing(#[from] PackingError),
    #[error(transparent)]
    Composition(#[from] PackedWorldCompositionError),
    #[error(transparent)]
    Connection(#[from] PackedConnectionError),
    #[error(transparent)]
    Certification(#[from] CandidateCertificationError),
}

/// Pack `children` into one node, join them, and certify the result.
///
/// The children are the certified artifacts of `node`'s own partition; this
/// proves they are, then packs them in stable [`ChunkId`] order inside the
/// frame [`pack_free_leaves`] derives from their halo spans -- no tuned
/// envelope, and no limit a caller can widen until a placement fits.  Internal
/// boundary signals become one trunk each, fanout included, laid by the
/// existing forced-runway router.  Root ends are resolved, not routed.
/// Whether a packed node leaves explicit empty bands between its children.
///
/// [`None`](Self::None) is production: the packing this crate has always
/// done, cell for cell. [`Derived`](Self::Derived) reads each seam's band
/// off the demands that cross it, see [`seam_bands`], and is reachable
/// through [`synthesise_packed_node_with_bands`] so a test can measure one
/// against the other on the same children. It is not the default because
/// the measurement went the other way: on a node whose lanes are never
/// climbed the band lengthens every trunk by its own width and buys nothing,
/// and a seam whose crossings need more than two layers -- every congested
/// seam measured so far -- is not band-eligible at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BandPolicy {
    Derived,
    None,
    /// No bands and no packing search: the children stand in a strip with
    /// their faces' reach between them, and every trunk is planned onto the
    /// lid fabric ([`super::fabric`]).
    Fabric,
}

pub(crate) fn synthesise_packed_node(
    node: &Netlist,
    children: &[FreeLeafArtifact],
    router: &impl PhysicalRouter,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: CertificationWorkers,
) -> Result<PackedNode, PackedNodeError> {
    synthesise_packed_node_with_bands(
        node,
        children,
        router,
        search,
        certification,
        workers,
        BandPolicy::None,
    )
}

pub(crate) fn synthesise_packed_node_with_bands(
    node: &Netlist,
    children: &[FreeLeafArtifact],
    router: &impl PhysicalRouter,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: CertificationWorkers,
    policy: BandPolicy,
) -> Result<PackedNode, PackedNodeError> {
    synthesise_packed_node_framed(
        node,
        children,
        router,
        search,
        certification,
        workers,
        policy,
        None,
    )
}

/// [`synthesise_packed_node`] for a root whose caller pinned its ports.
///
/// The layout is moved inside the plan-view bounding rectangle of the pins,
/// clear of them by [`PINNED_MARGIN`], and every port is built by this node
/// at the caller's own cell: an input as a node-owned source feeding every
/// child that reads it, an output as a node-owned handover on its trunk. A
/// layout larger than the rectangle is refused per layout, so a smaller rank
/// may still fit.
pub(crate) fn synthesise_packed_node_pinned(
    node: &Netlist,
    children: &[FreeLeafArtifact],
    pins: &PortPlacements,
    router: &impl PhysicalRouter,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: CertificationWorkers,
) -> Result<PackedNode, PackedNodeError> {
    synthesise_packed_node_framed(
        node,
        children,
        router,
        search,
        certification,
        workers,
        BandPolicy::None,
        Some(pins),
    )
}

/// [`synthesise_packed_node`] on the lid fabric ([`BandPolicy::Fabric`]),
/// pinned or not.
pub(crate) fn synthesise_packed_node_fabric(
    node: &Netlist,
    children: &[FreeLeafArtifact],
    pins: Option<&PortPlacements>,
    router: &impl PhysicalRouter,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: CertificationWorkers,
) -> Result<PackedNode, PackedNodeError> {
    synthesise_packed_node_framed(
        node,
        children,
        router,
        search,
        certification,
        workers,
        BandPolicy::Fabric,
        pins,
    )
}

/// How far a pinned layout keeps from the rectangle its pins draw: room for
/// each pin's handover, runway and the turn into the layout.
pub(crate) const PINNED_MARGIN: i32 = 8;

#[allow(clippy::too_many_arguments)]
fn synthesise_packed_node_framed(
    node: &Netlist,
    children: &[FreeLeafArtifact],
    router: &impl PhysicalRouter,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: CertificationWorkers,
    policy: BandPolicy,
    pins: Option<&PortPlacements>,
) -> Result<PackedNode, PackedNodeError> {
    if children.is_empty() {
        return Err(PackedNodeError::NoChildren);
    }
    // Validates drivers, declared ports and acyclicity before anything is
    // derived from them; the partitioner's own front door.  It does not refuse
    // a netlist that declares the same *output* twice -- a boundary map is
    // keyed by signal, so the second copy would find the first already taken
    // and the port list would be built by unwrapping a `None`.
    canonical_order(node)?;
    for (role, signals) in [
        (PortRole::Input, &node.inputs),
        (PortRole::Output, &node.outputs),
    ] {
        let mut seen = BTreeSet::new();
        for signal in signals {
            if !seen.insert(signal) {
                return Err(PackedNodeError::DuplicatePort {
                    signal: signal.clone(),
                    role,
                });
            }
        }
    }
    let by_chunk = index_children(children)?;
    prove_exact_gate_cover(node, &by_chunk)?;

    let boundaries = by_chunk
        .values()
        .map(|child| ChildBoundary {
            chunk: &child.chunk,
            inputs: &child.netlist.inputs,
            outputs: &child.netlist.outputs,
        })
        .collect::<Vec<_>>();
    let uses = boundary_signal_uses(node, &boundaries)?;

    // Everything from here to the routed, boundary-checked world depends on
    // where the children were put, so it is what a different layout can fix.
    // Everything above -- the netlist, the interfaces, the connectivity -- does
    // not, and has already refused if it was going to.
    let room = pins.map(|pins| PinnedRoom::of(pins, &node.inputs));
    let fabric = policy == BandPolicy::Fabric;
    let order = dataflow_order(&uses, &by_chunk)?;
    // Node-built ports stand past every child's east face and their own pads;
    // a pinned root's ports enter at feet west of the first child instead.
    let (port_gap, feet_gap) = if fabric {
        let ports = face_reach(node.inputs.len() + node.outputs.len(), fabric_reach(children).1);
        let reach = |chunk: &ChunkId| {
            fabric_reach(std::slice::from_ref(by_chunk[chunk])).0[0]
        };
        let east = order.iter().map(|chunk| reach(chunk).1).max().unwrap_or(0);
        let west = order.first().map_or(0, |chunk| reach(chunk).0);
        (east + ports + 1, west + ports + 1)
    } else {
        (10, 0)
    };
    let route_count = uses
        .values()
        .filter(|use_| {
            !use_.child_sinks.is_empty()
                && (!matches!(use_.sources[0], TrunkOwner::Root) || use_.child_sinks.len() > 1)
        })
        .count();
    // The highest trunk lane over a layout: its tallest child plus one pitch
    // per trunk after the first.
    let lane_top = |packed: &PackedFreeLeaves| {
        packed.halo.iter().map(|at| at.y).max().unwrap_or(4).max(4)
            + if route_count > 1 {
                1 + (route_count as i32 - 1) * super::parent::PACKED_LANE_PITCH
            } else {
                0
            }
    };
    // Whether the fabric plan is checked before routing: set while a strip
    // tighter than every face's full reach is being tried.
    let fabric_checked = std::cell::Cell::new(false);
    let build = |packed: &PackedFreeLeaves| -> Result<LayoutOutcome, PackedNodeError> {
        // A previously external input may face out of the nonnegative frame.
        // Once consumed by a fanout trunk, its whole egress must be in-frame.
        // Translate the layout without changing any relative child placement.
        let mut shifted;
        let fanout_sinks = uses
            .iter()
            .filter(|(_, use_)| {
                matches!(use_.sources[0], TrunkOwner::Root) && use_.child_sinks.len() > 1
            })
            .flat_map(|(signal, use_)| use_.child_sinks.iter().map(move |chunk| (chunk, signal)));
        let top = lane_top(packed);
        let floor_lane = packed.halo.iter().map(|at| at.y).max().unwrap_or(4).max(4) + 1;
        let mut offset = crate::compile::geometry::Anchor { x: 0, y: 0, z: 0 };
        for (chunk, signal) in fanout_sinks {
            let (_, interface) = interface_of(packed, chunk, signal, PortRole::Input)?;
            let direction = interface_route_direction(interface);
            let egress_top = if route_count > 1 {
                top
            } else {
                interface.pin.at.y
            };
            for at in super::terminal_geometry::terminal_egress_path(
                interface.pin.at,
                direction,
                egress_top,
            ) {
                offset.x = offset.x.max(-at.x);
                offset.z = offset.z.max(-at.z);
            }
        }
        if let Some(room) = &room {
            offset = pinned_offset(packed, room, floor_lane, feet_gap)?;
        }
        let packed = if offset.x != 0 || offset.z != 0 {
            shifted = packed.clone();
            shifted.halo.clear();
            for (chunk, placement) in &mut shifted.placements {
                let translation = crate::compile::geometry::Anchor {
                    x: placement
                        .translation
                        .x
                        .checked_add(offset.x)
                        .ok_or_else(|| PackingError::CoordinateOverflow {
                            chunk: chunk.clone(),
                        })?,
                    y: placement.translation.y,
                    z: placement
                        .translation
                        .z
                        .checked_add(offset.z)
                        .ok_or_else(|| PackingError::CoordinateOverflow {
                            chunk: chunk.clone(),
                        })?,
                };
                *placement = translate_artifact(by_chunk[chunk], translation)?;
                shifted.halo.extend(placement.halo.iter().copied());
            }
            &shifted
        } else {
            packed
        };
        let composed = compose_packed_free_leaf_worlds(children, packed)?;

        // One translation per child, in stable chunk order, and then the same
        // exact-cover statement the gates themselves answered -- now about where
        // they stand rather than what they are.
        let mut gates = GateMetadata::default();
        for (chunk, placement) in &packed.placements {
            let child = by_chunk
                .get(chunk)
                .ok_or_else(|| PackedNodeError::MissingChild {
                    chunk: chunk.clone(),
                })?;
            gates
                .absorb(&child.gates, placement.translation)
                .map_err(|error| PackedNodeError::GateMetadata {
                    chunk: chunk.clone(),
                    error,
                })?;
        }
        gates.covers(node)?;

        let mut requests = Vec::new();
        let mut input_requests = Vec::new();
        let mut exports = Vec::new();
        let mut inputs: BTreeMap<String, PackedRootPort> = BTreeMap::new();
        let mut outputs: BTreeMap<String, PackedRootPort> = BTreeMap::new();
        for (signal, use_) in &uses {
            match &use_.sources[0] {
                TrunkOwner::Root => {
                    if use_.root_sink {
                        return Err(PackedNodeError::PassThroughPort {
                            signal: signal.clone(),
                        });
                    }
                    let mut sinks = use_.child_sinks.iter();
                    let (chunk, extra) = (sinks.next(), sinks.count());
                    let chunk = chunk.ok_or_else(|| PackedNodeError::UnresolvedPort {
                        signal: signal.clone(),
                        role: PortRole::Input,
                    })?;
                    let pinned = pins.and_then(|pins| pins.get(signal));
                    if extra > 0 || pinned.is_some() {
                        let (_, interface) = interface_of(packed, chunk, signal, PortRole::Input)?;
                        // Beyond every child's halo: westward internal source,
                        // eastward caller runway. Rows are separated by a portal pitch.
                        let x = packed.halo.iter().map(|at| at.x).max().unwrap_or(0) + port_gap;
                        let z = 4 + input_requests.len() as i32 * PORTAL_PITCH;
                        let port = RootPort {
                            signal: signal.clone(),
                            role: PortRole::Input,
                            pin: pinned.unwrap_or(crate::compile::planner::PortPin {
                                at: crate::compile::geometry::Anchor { x, y: 3, z },
                                toward: Facing::West,
                            }),
                        };
                        let sinks = use_
                            .child_sinks
                            .iter()
                            .map(|sink| {
                                interface_of(packed, sink, signal, PortRole::Input)
                                    .map(|(id, _)| id)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let index = node
                            .inputs
                            .iter()
                            .position(|name| name == signal)
                            .expect("root signal is a declared input");
                        input_requests.push(PackedInputTrunkRequest {
                            port: PortId(
                                u32::try_from(index)
                                    .map_err(|_| PackedNodeError::NodePortIndexOverflow)?,
                            ),
                            root: port.clone(),
                            contract: interface.contract,
                            sinks,
                        });
                        inputs.insert(
                            signal.clone(),
                            PackedRootPort {
                                port,
                                interface: None,
                                contract: interface.contract,
                            },
                        );
                        continue;
                    }
                    let (id, interface) = interface_of(packed, chunk, signal, PortRole::Input)?;
                    inputs.insert(
                        signal.clone(),
                        root_port(signal, PortRole::Input, id, interface),
                    );
                }
                TrunkOwner::Child(chunk) => {
                    let (source, interface) =
                        interface_of(packed, chunk, signal, PortRole::Output)?;
                    let pinned = pins.and_then(|pins| pins.get(signal));
                    if use_.root_sink && (pinned.is_some() || !use_.child_sinks.is_empty()) {
                        // The caller would read the cell the trunk starts in.
                        // One cell cannot be both, so the node builds its own
                        // handover beyond every halo, fed as one more sink of
                        // the signal's trunk, and the caller reads that. It
                        // stands south of every halo too, so no child output
                        // facing east on the same row runs into it.
                        let x = packed.halo.iter().map(|at| at.x).max().unwrap_or(0) + port_gap;
                        let z = packed.halo.iter().map(|at| at.z).max().unwrap_or(0)
                            + (1 + exports.len() as i32) * PORTAL_PITCH;
                        let index = node
                            .outputs
                            .iter()
                            .position(|name| name == signal)
                            .expect("root sink signal is a declared output");
                        let port = RootPort {
                            signal: signal.clone(),
                            role: PortRole::Output,
                            pin: pinned.unwrap_or(crate::compile::planner::PortPin {
                                at: crate::compile::geometry::Anchor { x, y: 3, z },
                                toward: Facing::East,
                            }),
                        };
                        exports.push(PackedOutputExport {
                            port: PortId(
                                u32::try_from(index)
                                    .map_err(|_| PackedNodeError::NodePortIndexOverflow)?,
                            ),
                            root: port.clone(),
                        });
                        outputs.insert(
                            signal.clone(),
                            PackedRootPort {
                                port,
                                interface: None,
                                contract: interface.contract,
                            },
                        );
                    } else if use_.root_sink {
                        outputs.insert(
                            signal.clone(),
                            root_port(signal, PortRole::Output, source, interface),
                        );
                        continue;
                    }
                    let sinks = use_
                        .child_sinks
                        .iter()
                        .map(|sink| {
                            interface_of(packed, sink, signal, PortRole::Input).map(|(id, _)| id)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    requests.push(PackedTrunkRequest {
                        signal: signal.clone(),
                        source,
                        sinks,
                    });
                }
            }
        }

        // Declared order, inputs then outputs: exactly what `validate_root_ports`
        // compares against and what a certification vector's bits are zipped with.
        let mut root_ports = Vec::with_capacity(node.inputs.len() + node.outputs.len());
        for (role, signals, resolved) in [
            (PortRole::Input, &node.inputs, &mut inputs),
            (PortRole::Output, &node.outputs, &mut outputs),
        ] {
            for signal in signals {
                root_ports.push(resolved.remove(signal).ok_or_else(|| {
                    PackedNodeError::UnresolvedPort {
                        signal: signal.clone(),
                        role,
                    }
                })?);
            }
        }

        // The root ends go to the router as guards, not as requests: their whole
        // guard column is held for the caller through the canvas top, so no
        // internal trunk may cross a cell the next level will need.
        let roots = root_ports
            .iter()
            .filter_map(|port| port.interface.clone())
            .collect::<Vec<_>>();
        let routed = if fabric {
            let feet = room.as_ref().map(|_| {
                packed.halo.iter().map(|at| at.x).min().unwrap_or(0) - feet_gap
            });
            route_packed_trunks_fabric(
                &composed,
                packed,
                &requests,
                &roots,
                &input_requests,
                &exports,
                router,
                search.router_limits,
                feet,
                fabric_checked.get(),
            )?
        } else {
            route_packed_trunks_with_inputs(
                &composed,
                packed,
                &requests,
                &roots,
                &input_requests,
                &exports,
                router,
                search.router_limits,
            )?
        };
        validate_packed_root_boundary(packed, &routed.world, &root_ports)?;

        Ok(LayoutOutcome {
            packed: packed.clone(),
            world: routed.world,
            trunks: routed.routes,
            trunk_signals: routed.signals,
            trunk_lanes: routed.lanes,
            root_ports,
            gates,
        })
    };

    if let Some(room) = &room {
        pinned_space(children, room)?;
    }
    let demands = portal_demands(&uses, &by_chunk)?;
    let bands = match policy {
        BandPolicy::Derived => seam_bands(&uses, &by_chunk)?,
        BandPolicy::None | BandPolicy::Fabric => SeamBands::none(),
    };
    let mut candidates = Vec::new();
    let mut fabric_built = None;
    let enumeration = if fabric {
        let shape = StripShape::of(children, &order);
        fabric_checked.set(true);
        let tight = shape.tightest(children, &order, &build)?;
        fabric_checked.set(false);
        match tight {
            Some((packed, outcome)) => {
                candidates.push((0, packed));
                fabric_built = Some(outcome);
            }
            None => candidates.push((0, fabric_strip(children, &order, &shape.full())?)),
        }
        Ok((0, ()))
    } else {
        search_ranked_layouts_in_order(
        children,
        &order,
        PackingBudget::from_search(search)
            .within(room.as_ref().map_or((None, None), PinnedRoom::extent))
            .separating_terminals(),
        &bands,
        |rank, packed| {
            // A pinned root keeps only layouts its room can hold, and at most
            // PINNED_CANDIDATES of them: every candidate is a whole copy of its
            // children, and eight children enumerate a hundred thousand layouts.
            let Some(room) = &room else {
                candidates.push((rank, packed.clone()));
                return LayoutVerdict::<(), ()>::Retry(());
            };
            let floor_lane = packed.halo.iter().map(|at| at.y).max().unwrap_or(4).max(4) + 1;
            if pinned_offset(packed, room, floor_lane, 0).is_err() || port_runway_blocked(packed, node) {
                return LayoutVerdict::Retry(());
            }
            candidates.push((rank, packed.clone()));
            if candidates.len() >= PINNED_CANDIDATES {
                LayoutVerdict::Accepted(())
            } else {
                LayoutVerdict::Retry(())
            }
        },
        )
    };
    let attempted = match enumeration {
        Ok((rank, ())) => rank + 1,
        Err(LayoutSearchError::Exhausted { attempted, .. }) => attempted,
        Err(LayoutSearchError::BudgetExhausted { attempted }) => {
            if candidates.is_empty() {
                return Err(PackedNodeError::LayoutBudgetExhausted { attempted });
            }
            attempted
        }
        Err(LayoutSearchError::Packing(error)) => return Err(error.into()),
        Err(LayoutSearchError::Order(error)) => return Err(error.into()),
        Err(LayoutSearchError::Fatal(())) => unreachable!("candidate collection cannot fail"),
    };
    if let (Some(room), true) = (&room, candidates.is_empty()) {
        let extent = room.extent();
        return Err(PackedNodeError::LayoutsExhausted {
            attempted,
            rank_zero: Box::new(PackedNodeError::PinnedRegionTooSmall {
                needs: (0, 0),
                room: (extent.0.unwrap_or(i32::MAX), extent.1.unwrap_or(i32::MAX)),
            }),
        });
    }
    let mut scored = candidates
        .into_iter()
        .map(|(rank, packed)| {
            let score = portal_layout_score(&packed, &demands, &order, rank);
            (score, rank, packed)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.0.cmp(&right.0));
    #[cfg(test)]
    let mut candidate_scores = scored
        .iter()
        .map(|(score, original_rank, _)| PackedCandidateScoreRow {
            original_rank: *original_rank,
            score: score.clone(),
            routed: None,
            trunk_metrics: None,
            trunk_totals: None,
        })
        .collect::<Vec<_>>();

    let mut accepted = None;
    let mut rank_zero_error = None;
    let trace = std::env::var_os("REDA_TRACE_PINNED").is_some();
    if trace {
        eprintln!("reda: pinned layouts: {} candidates", scored.len());
    }
    for (_score, rank, packed) in scored {
        let started = std::time::Instant::now();
        let built = match fabric_built.take() {
            Some(outcome) => Ok(outcome),
            None => build(&packed),
        };
        if trace {
            eprintln!(
                "reda: pinned layout rank {rank}: {:?} in {:?}",
                built.as_ref().err().map(|e| e.to_string().chars().take(600).collect::<String>()),
                started.elapsed()
            );
        }
        match built {
            Ok(outcome) => {
                #[cfg(test)]
                {
                    let (metrics, totals) = physical_metrics(&outcome.trunks, &outcome.trunk_lanes);
                    let row = candidate_scores
                        .iter_mut()
                        .find(|row| row.original_rank == rank)
                        .expect("every routed candidate was scored");
                    row.routed = Some(true);
                    row.trunk_metrics = Some(metrics);
                    row.trunk_totals = Some(totals);
                }
                accepted = Some((rank, outcome));
                break;
            }
            Err(error) if error.is_layout_dependent() => {
                #[cfg(test)]
                if let Some(row) = candidate_scores
                    .iter_mut()
                    .find(|row| row.original_rank == rank)
                {
                    row.routed = Some(false);
                }
                // A pinned root may have dropped rank zero as unplaceable;
                // its first routed refusal stands in for it.
                if rank == 0 || rank_zero_error.is_none() {
                    rank_zero_error = Some(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    let (layout_rank, outcome) = accepted.ok_or_else(|| PackedNodeError::LayoutsExhausted {
        attempted,
        rank_zero: Box::new(rank_zero_error.expect("rank zero is in every non-empty search")),
    })?;
    let LayoutOutcome {
        packed,
        world,
        trunks,
        trunk_signals,
        trunk_lanes,
        root_ports,
        gates,
    } = outcome;

    let certificate = certify_root_world(
        &world,
        node,
        &root_ports
            .iter()
            .map(|port| port.port.clone())
            .collect::<Vec<_>>(),
        certification,
        workers,
    )?;

    // The world component is the certificate's own, not a second reading of
    // the same world: a node's identity is the world that was certified.
    let trunk_fingerprint = canonical_fingerprint(
        &serde_json::to_vec(&trunks).expect("a realised route tree list serializes"),
    );
    let fingerprint = canonical_fingerprint(
        format!(
            "packed-node-v1:{}:{}",
            certificate.world_fingerprint.as_str(),
            trunk_fingerprint.as_str()
        )
        .as_bytes(),
    );
    #[cfg(test)]
    let (trunk_physical_metrics, trunk_physical_totals) = physical_metrics(&trunks, &trunk_lanes);
    Ok(PackedNode {
        netlist: node.clone(),
        gates,
        world,
        packed,
        trunks,
        trunk_signals,
        trunk_lanes,
        #[cfg(test)]
        trunk_physical_metrics,
        #[cfg(test)]
        trunk_physical_totals,
        #[cfg(test)]
        candidate_scores,
        layout_rank,
        root_ports,
        certificate,
        fingerprint,
    })
}

/// The order children must be placed in: every driver before every reader.
///
/// **Why placement order is not a free choice.** Every terminal this crate
/// builds faces east, so a parent route leaves an output eastward and enters
/// an input from the west. Translation-only packing lays children out left to
/// right, so a child placed to the left of its reader has its runway pointing
/// at that reader, and a child placed to the right has its runway pointing
/// away. Nothing downstream can recover from the second case: the router's
/// forced runway is straight, so a reversed pair is not a harder search but an
/// impossible one, and re-ranking the *later* child's translation cannot move
/// the earlier one back.
///
/// Before this, the order was the canonical [`ChunkId`] order -- a
/// fingerprint, which is to say a coin flip -- so about half of every
/// two-child node came out reversed and was refused by the router.
///
/// The edges come from the connectivity that was already derived: for each
/// boundary signal, the child that drives it precedes every child that reads
/// it. It is Kahn's algorithm with the ready set kept in [`ChunkId`] order, so
/// independent children fall back to exactly the canonical order and the
/// result depends on nothing but the netlist.
fn dataflow_order(
    uses: &BTreeMap<String, SignalUse>,
    by_chunk: &BTreeMap<ChunkId, &FreeLeafArtifact>,
) -> Result<Vec<ChunkId>, PackedNodeError> {
    let mut blocked_by: BTreeMap<&ChunkId, BTreeSet<&ChunkId>> = by_chunk
        .keys()
        .map(|chunk| (chunk, BTreeSet::new()))
        .collect();
    let mut readers: BTreeMap<&ChunkId, BTreeSet<&ChunkId>> = BTreeMap::new();
    for use_ in uses.values() {
        let TrunkOwner::Child(driver) = &use_.sources[0] else {
            continue;
        };
        for reader in &use_.child_sinks {
            if reader == driver {
                continue;
            }
            let (Some(driver), Some(reader)) = (
                by_chunk.get_key_value(driver).map(|(key, _)| key),
                by_chunk.get_key_value(reader).map(|(key, _)| key),
            ) else {
                continue;
            };
            if readers.entry(driver).or_default().insert(reader) {
                blocked_by
                    .get_mut(reader)
                    .expect("every child has an entry")
                    .insert(driver);
            }
        }
    }

    let mut ready: BTreeSet<&ChunkId> = blocked_by
        .iter()
        .filter(|(_, blockers)| blockers.is_empty())
        .map(|(chunk, _)| *chunk)
        .collect();
    let mut order = Vec::with_capacity(by_chunk.len());
    while let Some(chunk) = ready.pop_first() {
        order.push(chunk.clone());
        blocked_by.remove(chunk);
        for reader in readers.get(chunk).into_iter().flatten() {
            let blockers = blocked_by.get_mut(*reader).expect("a reader is a child");
            blockers.remove(chunk);
            if blockers.is_empty() {
                ready.insert(reader);
            }
        }
    }
    if !blocked_by.is_empty() {
        // `canonical_order` has already refused a cyclic netlist, so this is
        // the aggregated child graph disagreeing with it rather than a user
        // shape. It is still a refusal, not an assertion.
        return Err(PackedNodeError::ChildDependencyCycle {
            chunks: blocked_by.keys().map(|chunk| (*chunk).clone()).collect(),
        });
    }
    Ok(order)
}

/// **The empty band each seam of this node needs, from the demands that
/// cross it.**
///
/// A trunk crossing a seam at terminal height needs the two runway mouths
/// to face each other across empty parent-owned columns: one runway and one
/// mouth per side, `2 * (TERMINAL_RUNWAY_CELLS + 1)` columns. Two trunks
/// whose sources and sinks are in opposite `z` order have to cross, and a
/// crossing needs a second layer one [`PACKED_LANE_PITCH`] up, which needs
/// that many more columns to climb in. So each seam is one layer if its
/// crossing demands keep their `z` order and two if any pair inverts, read
/// off the children's own pin coordinates, which translation preserves.
///
/// **Atomic.** Every internal trunk of the node must be one source to one
/// sink, between two different children, leaving its source east or west
/// and entering its sink from the opposite side; a fanout, a north-south
/// pair, or a trunk inside one child puts the whole node back on the packing
/// and the guided lanes it has always had. Root ends are not trunks and do
/// not count. Checked arithmetic throughout: a width that overflows is a
/// typed refusal, never a wrapped one.
fn seam_bands(
    uses: &BTreeMap<String, SignalUse>,
    by_chunk: &BTreeMap<ChunkId, &FreeLeafArtifact>,
) -> Result<SeamBands, PackedNodeError> {
    let interface = |chunk: &ChunkId, signal: &str, role: PortRole| {
        let id = artifact_interface_id(by_chunk, chunk, signal, role)?;
        let artifact = &by_chunk[chunk];
        Ok::<_, PackedNodeError>(artifact.interfaces[&id].clone())
    };
    let mut crossings: BTreeMap<(ChunkId, ChunkId), Vec<(i32, i32)>> = BTreeMap::new();
    let mut sink_heights: BTreeMap<(ChunkId, ChunkId), i32> = BTreeMap::new();
    let lid_y = by_chunk
        .values()
        .filter_map(|child| {
            let ys = child.halo.iter().map(|at| at.y);
            Some(ys.clone().max()? - ys.min()?)
        })
        .max()
        .unwrap_or(0);
    for (signal, use_) in uses {
        let TrunkOwner::Child(source) = &use_.sources[0] else {
            continue;
        };
        if use_.root_sink {
            continue;
        }
        if use_.child_sinks.len() != 1 {
            return Ok(SeamBands::none());
        }
        let sink = use_.child_sinks.iter().next().expect("one sink");
        if sink == source {
            return Ok(SeamBands::none());
        }
        let out = interface(source, signal, PortRole::Output)?;
        let into = interface(sink, signal, PortRole::Input)?;
        let leaves = interface_route_direction(&out);
        if !matches!(leaves, Facing::East | Facing::West)
            || interface_route_direction(&into) != leaves.opposite()
        {
            return Ok(SeamBands::none());
        }
        let key = if source <= sink {
            (source.clone(), sink.clone())
        } else {
            (sink.clone(), source.clone())
        };
        crossings
            .entry(key.clone())
            .or_default()
            .push((out.pin.at.z, into.pin.at.z));
        let sink_floor = by_chunk[sink].halo.iter().map(|at| at.y).min().unwrap_or(0);
        let height = sink_heights
            .entry(key)
            .or_insert(into.pin.at.y - sink_floor);
        *height = (*height).max(into.pin.at.y - sink_floor);
    }
    let mut bands = SeamBands::none();
    for ((a, b), mut pairs) in crossings {
        pairs.sort();
        let pitch_apart = |mut rows: Vec<i32>| {
            rows.sort_unstable();
            rows.windows(2).all(|pair| {
                pair[1]
                    .checked_sub(pair[0])
                    .is_some_and(|gap| gap >= PACKED_LANE_PITCH)
            })
        };
        if !pitch_apart(pairs.iter().map(|(source, _)| *source).collect())
            || !pitch_apart(pairs.iter().map(|(_, sink)| *sink).collect())
        {
            return Ok(SeamBands::none());
        }
        let layers = band_layers(&pairs);
        let layer_count = i32::try_from(layers.iter().copied().max().unwrap_or(0))
            .map_err(|_| PackedNodeError::BandOverflow)?
            + 1;
        let width = band_min_width(layer_count).ok_or(PackedNodeError::BandOverflow)?;
        let terminal_y = sink_heights[&(a.clone(), b.clone())];
        if !band_pays(&layers, lid_y, terminal_y).ok_or(PackedNodeError::BandOverflow)? {
            return Ok(SeamBands::none());
        }
        bands.set(&a, &b, width);
    }
    Ok(bands)
}

fn portal_demands(
    uses: &BTreeMap<String, SignalUse>,
    by_chunk: &BTreeMap<ChunkId, &FreeLeafArtifact>,
) -> Result<Vec<PortalDemand>, PackedNodeError> {
    let mut demands = Vec::new();
    for (signal, use_) in uses {
        let TrunkOwner::Child(source) = &use_.sources[0] else {
            continue;
        };
        if use_.root_sink {
            continue;
        }
        let source = artifact_interface_id(by_chunk, source, signal, PortRole::Output)?;
        for sink in &use_.child_sinks {
            let sink = artifact_interface_id(by_chunk, sink, signal, PortRole::Input)?;
            demands.push(PortalDemand {
                signal: signal.clone(),
                source: source.clone(),
                sink,
            });
        }
    }
    demands.sort();
    Ok(demands)
}

fn artifact_interface_id(
    by_chunk: &BTreeMap<ChunkId, &FreeLeafArtifact>,
    chunk: &ChunkId,
    signal: &str,
    role: PortRole,
) -> Result<FreeLeafInterfaceId, PackedNodeError> {
    let artifact = by_chunk
        .get(chunk)
        .ok_or_else(|| PackedNodeError::MissingChild {
            chunk: chunk.clone(),
        })?;
    let mut matching = artifact
        .interfaces
        .iter()
        .filter(|(_, interface)| interface.role == role && interface.signal == signal);
    let (id, _) = matching
        .next()
        .ok_or_else(|| PackedNodeError::MissingInterface {
            chunk: chunk.clone(),
            signal: signal.to_owned(),
            role,
        })?;
    if matching.next().is_some() {
        return Err(PackedNodeError::AmbiguousInterface {
            chunk: chunk.clone(),
            signal: signal.to_owned(),
            role,
        });
    }
    Ok(id.clone())
}

fn portal_layout_score(
    packed: &PackedFreeLeaves,
    demands: &[PortalDemand],
    order: &[ChunkId],
    original_rank: usize,
) -> PortalLayoutScore {
    let mut total_sink_manhattan = 0;
    let mut max_abs_dz = 0;
    let mut sum_abs_dz = 0;
    let mut trunk_maxima: BTreeMap<&str, u64> = BTreeMap::new();
    for demand in demands {
        let source = portal_mouth(packed, &demand.source);
        let sink = portal_mouth(packed, &demand.sink);
        let dx = (source.0 - sink.0).unsigned_abs();
        let dy = (source.1 - sink.1).unsigned_abs();
        let dz = (source.2 - sink.2).unsigned_abs();
        let distance = dx + dy + dz;
        total_sink_manhattan += distance;
        max_abs_dz = max_abs_dz.max(dz);
        sum_abs_dz += dz;
        trunk_maxima
            .entry(&demand.signal)
            .and_modify(|maximum| *maximum = (*maximum).max(distance))
            .or_insert(distance);
    }
    let sum_trunk_max_manhattan = trunk_maxima.values().copied().sum();

    let mut reserved = BTreeSet::new();
    let mut envelope = Vec::with_capacity(order.len());
    for chunk in order {
        let placement = &packed.placements[chunk];
        envelope.push(envelope_score(
            &reserved,
            &placement.halo,
            placement.translation,
        ));
        reserved.extend(placement.halo.iter().copied());
    }
    let translations = packed
        .placements
        .iter()
        .map(|(chunk, placement)| {
            (
                chunk.clone(),
                placement.translation.x,
                placement.translation.y,
                placement.translation.z,
            )
        })
        .collect();
    PortalLayoutScore {
        sum_trunk_max_manhattan,
        total_sink_manhattan,
        max_abs_dz,
        sum_abs_dz,
        envelope,
        original_rank,
        translations,
    }
}

fn portal_mouth(packed: &PackedFreeLeaves, id: &FreeLeafInterfaceId) -> (i64, i64, i64) {
    let interface = &packed.placements[&id.chunk].interfaces[id];
    let mut mouth = interface.pin.at;
    let steps = i32::from(TERMINAL_RUNWAY_CELLS as u16);
    match interface_route_direction(interface) {
        Facing::East => mouth.x += steps,
        Facing::West => mouth.x -= steps,
        Facing::South => mouth.z += steps,
        Facing::North => mouth.z -= steps,
        Facing::Up => mouth.y += steps,
        Facing::Down => mouth.y -= steps,
    }
    (i64::from(mouth.x), i64::from(mouth.y), i64::from(mouth.z))
}

impl PackedNodeError {
    /// Could a different placement of the same children fix this?
    ///
    /// Only the refusals that name a cell, a collision or a route can: they
    /// are statements about where things ended up. A refusal about the
    /// netlist, the interfaces, the connectivity or the certificate says the
    /// same thing for every layout, and retrying it is pure waste.
    fn is_layout_dependent(&self) -> bool {
        matches!(
            self,
            Self::Packing(_)
                | Self::Composition(_)
                | Self::Connection(_)
                | Self::GateMetadata { .. }
                | Self::RootPortRunwayBlocked { .. }
                | Self::RootPortRunwayEscapes { .. }
                | Self::RootPortsTooClose { .. }
                | Self::RootPortGuardsAnother { .. }
                | Self::PinnedRegionTooSmall { .. }
        )
    }
}

/// Why a pinned root's room cannot hold its children on one floor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PinnedSpaceShortfall {
    /// One child is wider or deeper than the room along a bounded axis.
    ChildTooLarge { child: (i32, i32) },
    /// The children's footprints together exceed the room's area.
    AreaExceeded { needs: i64, floors: i64 },
}

impl std::fmt::Display for PinnedSpaceShortfall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChildTooLarge { child } => write!(
                f,
                "one child alone is {child:?}; it has to be split finer"
            ),
            Self::AreaExceeded { needs, floors } => write!(
                f,
                "the children's footprints add up to {needs} cells, about {floors} floors of it"
            ),
        }
    }
}

/// The plan-view room a pinned root builds in.
///
/// It is the pins' bounding rectangle, [`PINNED_MARGIN`] inside every side
/// that carries a pin, and open past every side that does not: the caller's
/// wiring reaches a pin from outside, so the room may only grow where no pin
/// faces out. A pin on a corner blocks only the side its caller comes from.
#[derive(Debug, Clone)]
pub(crate) struct PinnedRoom {
    lo: (i32, i32),
    hi: (i32, i32),
    /// Sides the room may grow past: west, east, north, south.
    open: [bool; 4],
    /// Sides a caller stands on: an input's driver or an output's reader.
    /// Everything past one is the caller's, so the room never crosses it --
    /// not even from the half-plane past an open side.
    fed: [bool; 4],
    /// Each pin, and the way its signal runs on the circuit's side of it:
    /// an input's `toward`, an output's opposite.
    pins: Vec<(crate::compile::geometry::Anchor, Facing)>,
}

impl PinnedRoom {
    /// `inputs` names the root's inputs, which tells each pin's role and so
    /// the side its caller reaches it from.
    pub(crate) fn of(pins: &PortPlacements, inputs: &[String]) -> Self {
        let (mut lo, mut hi) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for (_, pin) in pins.iter() {
            lo = (lo.0.min(pin.at.x), lo.1.min(pin.at.z));
            hi = (hi.0.max(pin.at.x), hi.1.max(pin.at.z));
        }
        let mut open = [true; 4];
        let mut fed = [false; 4];
        for (signal, pin) in pins.iter() {
            // An input's caller drives it from behind; an output's caller
            // reads it from in front.
            let caller = if inputs.contains(signal) {
                pin.toward.opposite()
            } else {
                pin.toward
            };
            let side = match caller {
                Facing::West if pin.at.x == lo.0 => Some(0),
                Facing::East if pin.at.x == hi.0 => Some(1),
                Facing::North if pin.at.z == lo.1 => Some(2),
                Facing::South if pin.at.z == hi.1 => Some(3),
                _ => None,
            };
            // Either way the caller stands on that side: an input's caller
            // drives it from there and an output's reads it there -- a
            // display in front of its outputs. The circuit crosses neither.
            if let Some(side) = side {
                open[side] = false;
                fed[side] = true;
            }
        }
        Self {
            lo,
            hi,
            open,
            fed,
            pins: pins
                .iter()
                .map(|(signal, pin)| {
                    let inward = if inputs.contains(signal) {
                        pin.toward
                    } else {
                        pin.toward.opposite()
                    };
                    (pin.at, inward)
                })
                .collect(),
        }
    }

    /// The room's (x, z) extents inside its margins; `None` along an axis
    /// it may grow along without end.
    ///
    /// A closed box is its margins. Once a side is open, the half-plane past
    /// it may run across any side only outputs close, so an axis is bounded
    /// only by a fed side at its high end -- down to the world's edge at 1
    /// when its low end is not fed as well.
    pub(crate) fn extent(&self) -> (Option<i32>, Option<i32>) {
        let m = PINNED_MARGIN;
        if !self.open.iter().any(|open| *open) {
            return (
                Some(self.hi.0 - self.lo.0 + 1 - 2 * m),
                Some(self.hi.1 - self.lo.1 + 1 - 2 * m),
            );
        }
        let axis = |fed_lo: bool, fed_hi: bool, lo: i32, hi: i32| {
            fed_hi.then(|| hi - m - if fed_lo { lo + m } else { 1 } + 1)
        };
        (
            axis(self.fed[0], self.fed[1], self.lo.0, self.hi.0),
            axis(self.fed[2], self.fed[3], self.lo.1, self.hi.1),
        )
    }

    /// Where a footprint `size` (x, z) may stand: the position of its low
    /// corner, closest to the pins overall, keeping [`PINNED_MARGIN`] from
    /// every pin and from every side it may not cross. `None` when nowhere.
    ///
    /// A pin's trunk climbs from the pin to a lane at least as high as `top`
    /// before it may cross the footprint, so on the side the pin faces the
    /// circuit it keeps a stair run of `top - y` on top of the margin's
    /// runway.
    ///
    /// ponytail: sized for the lowest lane; a pin whose trunk is given a
    /// higher one climbs the rest sideways, in front of the footprint.
    fn place(&self, size: (i32, i32), top: i32) -> Option<(i32, i32)> {
        let (lo, hi, open, fed, m) = (self.lo, self.hi, self.open, self.fed, PINNED_MARGIN);
        // Every corner that could be closest: the footprint overlapping the
        // box, give or take its own size and a margin. Farther only reaches
        // further.
        let xs = (lo.0 - m - size.0).max(1)..=hi.0 + m + 1;
        let zs = (lo.1 - m - size.1).max(1)..=hi.1 + m + 1;
        let mut best: Option<(i64, (i32, i32))> = None;
        for x in xs {
            for z in zs.clone() {
                let (x1, z1) = (x + size.0 - 1, z + size.1 - 1);
                // Inside the margins of every closed side -- except that a
                // footprint wholly past an open side stands in a half-plane no
                // caller reaches, and may run across a side only outputs
                // close. A fed side is never crossed.
                let within = [x >= lo.0 + m, x1 <= hi.0 - m, z >= lo.1 + m, z1 <= hi.1 - m];
                let past = (open[0] && x1 < lo.0 - m)
                    || (open[1] && x > hi.0 + m)
                    || (open[2] && z1 < lo.1 - m)
                    || (open[3] && z > hi.1 + m);
                if !(0..4).all(|side| within[side] || open[side] || (past && !fed[side])) {
                    continue;
                }
                let clear = self.pins.iter().all(|(pin, inward)| {
                    let gap = |side: Facing| {
                        if side == *inward {
                            // Pin to handover to anchor (2), the runway (2),
                            // its mouth (1), one cell along per cell of climb,
                            // and the halo's own ring (2).
                            PINNED_MARGIN.max((top - pin.y).abs() + 7)
                        } else {
                            PINNED_MARGIN
                        }
                    };
                    pin.x < x - gap(Facing::East)
                        || pin.x > x1 + gap(Facing::West)
                        || pin.z < z - gap(Facing::South)
                        || pin.z > z1 + gap(Facing::North)
                });
                if !clear {
                    continue;
                }
                let reach = self
                    .pins
                    .iter()
                    .map(|(pin, _)| {
                        i64::from((x - pin.x).max(0).max(pin.x - x1))
                            + i64::from((z - pin.z).max(0).max(pin.z - z1))
                    })
                    .sum::<i64>();
                if best.is_none_or(|(known, _)| reach < known) {
                    best = Some((reach, (x, z)));
                }
            }
        }
        best.map(|(_, corner)| corner)
    }
}

/// The least plan-view area a leaf takes per gate, measured on the leaves
/// production builds (2026-09-25, halo footprint over gate count):
/// `verilog:seven_segment` 564 and 412, `segment_a` 542 and 344. The smallest
/// is the bound, so an estimate built on it never overstates a root's need.
pub(crate) const MIN_LEAF_AREA_PER_GATE: i64 = 344;

/// Whether a terminal on one of the node's own ports runs its way out --
/// runway, mouth and the first cells past it -- into another signal's runway
/// or mouth.
///
/// Packing leaves a way between two children only for the trunks it knows
/// of, which join child to child. A pinned root also routes every port to its
/// caller's pin, so a layout can face another child's output straight into a
/// port's terminal, and whichever routes second has no way in. Child-to-child
/// terminals that face each other are the packer's business, and not checked.
fn port_runway_blocked(packed: &PackedFreeLeaves, node: &Netlist) -> bool {
    let way_out = |interface: &ParentConnectableInterface, reach: bool| {
        let facing = interface_route_direction(interface);
        let near = runway_core(interface.pin.at, facing);
        let far = runway_core(*near.last().expect("a core has its anchor"), facing);
        let far = if reach { &far[1..] } else { &far[1..2] };
        near.iter().chain(far).copied().collect::<Vec<_>>()
    };
    let interfaces = packed
        .placements
        .values()
        .flat_map(|leaf| leaf.interfaces.values())
        .collect::<Vec<_>>();
    let mut claimed: BTreeMap<Anchor, Vec<&str>> = BTreeMap::new();
    for interface in &interfaces {
        for at in way_out(interface, false) {
            claimed.entry(at).or_default().push(&interface.signal);
        }
    }
    interfaces.iter().any(|interface| {
        let port = match interface.role {
            PortRole::Input => node.inputs.contains(&interface.signal),
            PortRole::Output => node.outputs.contains(&interface.signal),
        };
        port && way_out(interface, true).iter().any(|at| {
            claimed
                .get(at)
                .is_some_and(|signals| signals.iter().any(|signal| *signal != interface.signal))
        })
    })
}

/// Each child's (west, east) face reach on the lid fabric, and the climb every
/// pad makes: from the lowest terminal to the fabric's first layer over the
/// tallest child, all children on one floor.
fn fabric_reach<C: std::borrow::Borrow<FreeLeafArtifact>>(children: &[C]) -> (Vec<(i32, i32)>, i32) {
    let span = |child: &FreeLeafArtifact| {
        let ys = child.halo.iter().map(|at| at.y);
        (ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0))
    };
    let lid = children
        .iter()
        .map(|child| {
            let child = child.borrow();
            let (low, high) = span(child);
            high - low
        })
        .max()
        .unwrap_or(0);
    let climb = children
        .iter()
        .flat_map(|child| {
            let child = child.borrow();
            let low = span(child).0;
            child
                .interfaces
                .values()
                .map(move |interface| Layers::over(lid).e - (interface.pin.at.y - low))
        })
        .max()
        .unwrap_or(0);
    let reach = children
        .iter()
        .map(|child| {
            let child = child.borrow();
            let count = |facing| {
                child
                    .interfaces
                    .values()
                    .filter(|interface| interface_route_direction(interface) == facing)
                    .count()
            };
            (
                face_reach(count(Facing::West), climb),
                face_reach(count(Facing::East), climb),
            )
        })
        .collect();
    (reach, climb)
}

/// A fabric strip's seams: the gap from each child's halo to the next one's
/// along +x, and each child's shift along z.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StripShape {
    gaps: Vec<i32>,
    shifts: Vec<i32>,
    /// Every face's full reach: the gaps a strip always had, which keep each
    /// face's pads and columns clear of its neighbour's by construction.
    full: Vec<i32>,
    /// One pad's reach: the least a seam can be and still hold the pads that
    /// climb out of it, whichever side they come from.
    least: i32,
}

impl StripShape {
    fn of(children: &[FreeLeafArtifact], order: &[ChunkId]) -> Self {
        let ordered = ordered_children(children, order);
        let (reach, climb) = fabric_reach(&ordered);
        let full = reach
            .windows(2)
            .map(|pair| pair[0].1 + pair[1].0 + 1)
            .collect::<Vec<_>>();
        Self {
            gaps: full.clone(),
            shifts: vec![0; ordered.len()],
            full,
            least: pad_reach(climb) + 1,
        }
    }

    /// The strip every fabric node was always built on.
    fn full(&self) -> Self {
        Self {
            gaps: self.full.clone(),
            shifts: vec![0; self.shifts.len()],
            ..self.clone()
        }
    }

    /// The tightest strip `build` accepts, and what it built.
    ///
    /// Every seam starts at one pad's reach, each child shifted along z so its
    /// west terminals' rows stand a pitch clear of its neighbour's east ones:
    /// the two faces then share one seam, pads interleaved and each comb of
    /// columns over the other child. The plan is checked before anything is
    /// routed, and the seam a clash falls in widens by a pitch, up to its full
    /// reach. `None` when the tight strip fails for any other reason, so the
    /// caller builds the full one as it always did.
    fn tightest(
        &self,
        children: &[FreeLeafArtifact],
        order: &[ChunkId],
        build: &dyn Fn(&PackedFreeLeaves) -> Result<LayoutOutcome, PackedNodeError>,
    ) -> Result<Option<(PackedFreeLeaves, LayoutOutcome)>, PackedNodeError> {
        let ordered = ordered_children(children, order);
        let mut shape = Self {
            gaps: self.full.iter().map(|&full| self.least.min(full)).collect(),
            shifts: interleaved_shifts(&ordered),
            ..self.clone()
        };
        if shape.gaps == self.full {
            return Ok(None);
        }
        loop {
            let packed = fabric_strip(children, order, &shape)?;
            let built = build(&packed);
            if std::env::var_os("REDA_TRACE_FABRIC").is_some() {
                eprintln!(
                    "reda: fabric strip gaps {:?} of {:?}, shifts {:?}: {}",
                    shape.gaps,
                    self.full,
                    shape.shifts,
                    built.as_ref().map_or_else(ToString::to_string, |_| "built".into())
                );
            }
            match built {
                Ok(outcome) => return Ok(Some((packed, outcome))),
                Err(PackedNodeError::Connection(PackedConnectionError::FabricClash { at })) => {
                    let seam = seam_at(&ordered, &packed, at.x);
                    if shape.gaps[seam] >= self.full[seam] {
                        return Ok(None);
                    }
                    shape.gaps[seam] = (shape.gaps[seam] + PITCH).min(self.full[seam]);
                }
                Err(_) => return Ok(None),
            }
        }
    }
}

fn ordered_children<'a>(
    children: &'a [FreeLeafArtifact],
    order: &[ChunkId],
) -> Vec<&'a FreeLeafArtifact> {
    let by_chunk = children
        .iter()
        .map(|child| (&child.chunk, child))
        .collect::<BTreeMap<_, _>>();
    order.iter().map(|chunk| by_chunk[chunk]).collect()
}

fn halo_bounds(child: &FreeLeafArtifact) -> (Anchor, Anchor) {
    let mut low = Anchor { x: i32::MAX, y: i32::MAX, z: i32::MAX };
    let mut high = Anchor { x: i32::MIN, y: i32::MIN, z: i32::MIN };
    for at in &child.halo {
        low = Anchor { x: low.x.min(at.x), y: low.y.min(at.y), z: low.z.min(at.z) };
        high = Anchor { x: high.x.max(at.x), y: high.y.max(at.y), z: high.z.max(at.z) };
    }
    (low, high)
}

/// Each child's z shift, in `0..2 * PITCH`, so that every row its west face
/// climbs out of stands at least a pitch from every row its western
/// neighbour's east face climbs out of: the two faces' pads then share a seam
/// without meeting. The least shift that does it, or none when none does.
fn interleaved_shifts(ordered: &[&FreeLeafArtifact]) -> Vec<i32> {
    let rows = |child: &FreeLeafArtifact, facing: Facing| {
        let low = halo_bounds(child).0;
        child
            .interfaces
            .values()
            .filter(|interface| interface_route_direction(interface) == facing)
            .map(|interface| interface.pin.at.z - low.z)
            .collect::<Vec<_>>()
    };
    let mut shifts = vec![0; ordered.len()];
    for index in 1..ordered.len() {
        let east = rows(ordered[index - 1], Facing::East)
            .into_iter()
            .map(|z| z + shifts[index - 1])
            .collect::<Vec<_>>();
        let west = rows(ordered[index], Facing::West);
        shifts[index] = (0..2 * PITCH)
            .find(|shift| {
                west.iter()
                    .all(|z| east.iter().all(|other| (z + shift - other).abs() >= PITCH))
            })
            .unwrap_or(0);
    }
    shifts
}

/// The seam a clash at `x` is charged to: the last one starting at or west
/// of it, since a face's columns stand east of its own seam.
fn seam_at(ordered: &[&FreeLeafArtifact], packed: &PackedFreeLeaves, x: i32) -> usize {
    let east_edges = ordered
        .iter()
        .map(|child| {
            packed.placements[&child.chunk]
                .halo
                .iter()
                .map(|at| at.x)
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    east_edges[..east_edges.len().saturating_sub(1)]
        .iter()
        .rposition(|edge| *edge <= x)
        .unwrap_or(0)
}

/// The fabric's layout: every child on one floor, along +x in `order`, each
/// seam as wide as `shape` says and each child shifted along z by it.
fn fabric_strip(
    children: &[FreeLeafArtifact],
    order: &[ChunkId],
    shape: &StripShape,
) -> Result<PackedFreeLeaves, PackedNodeError> {
    let ordered = ordered_children(children, order);
    let (reach, _) = fabric_reach(&ordered);
    let mut placements = BTreeMap::new();
    let mut halo = BTreeSet::new();
    let mut x = reach.first().map_or(0, |(west, _)| *west);
    for (index, child) in ordered.iter().enumerate() {
        let (low, high) = halo_bounds(child);
        let placement = translate_artifact(
            child,
            Anchor {
                x: x - low.x,
                y: -low.y,
                z: shape.shifts[index] - low.z,
            },
        )?;
        halo.extend(placement.halo.iter().copied());
        placements.insert(child.chunk.clone(), placement);
        if let Some(gap) = shape.gaps.get(index) {
            x += high.x - low.x + gap;
        }
    }
    Ok(PackedFreeLeaves { placements, halo })
}

/// The most layouts a pinned root collects before scoring them.
///
/// ponytail: the first ones in enumeration order, not the best-scored of all;
/// keep a best-K heap if a later layout is ever the one that routes.
const PINNED_CANDIDATES: usize = 16;

/// Before any child is built: the floors a pinned root of `gates` gates needs
/// at least, if its room is bounded both ways and too small to hold them on
/// one floor.
///
/// A lower bound from [`MIN_LEAF_AREA_PER_GATE`], so a `Some` is certain to
/// be short and building the children to find that out would be wasted.
pub(crate) fn pinned_floors_short(gates: usize, room: &PinnedRoom) -> Option<(i64, i64)> {
    let (Some(x), Some(z)) = room.extent() else {
        return None;
    };
    let area = i64::from(x.max(0)) * i64::from(z.max(0));
    let needs = i64::try_from(gates).unwrap_or(i64::MAX).saturating_mul(MIN_LEAF_AREA_PER_GATE);
    (needs > area).then(|| (needs, (needs + area - 1) / area.max(1)))
}

/// Refuse a pinned root whose children cannot stand in its room on one
/// floor, before any layout is searched: a child larger than the room along a
/// bounded axis, or footprints that add up to more than a bounded room's area.
/// Both are lower bounds -- trunks and gaps only add to them -- so nothing
/// that could fit is refused.
fn pinned_space(children: &[FreeLeafArtifact], room: &PinnedRoom) -> Result<(), PackedNodeError> {
    let extent = room.extent();
    let shown = (extent.0.unwrap_or(i32::MAX), extent.1.unwrap_or(i32::MAX));
    let mut needs = 0i64;
    for child in children {
        let (mut min, mut max) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for at in &child.halo {
            min = (min.0.min(at.x), min.1.min(at.z));
            max = (max.0.max(at.x), max.1.max(at.z));
        }
        let footprint = (max.0 - min.0 + 1, max.1 - min.1 + 1);
        if footprint.0 > shown.0 || footprint.1 > shown.1 {
            return Err(PackedNodeError::PinnedSpaceShort {
                room: shown,
                space: PinnedSpaceShortfall::ChildTooLarge { child: footprint },
            });
        }
        needs += i64::from(footprint.0) * i64::from(footprint.1);
    }
    if let (Some(x), Some(z)) = extent {
        let area = i64::from(x.max(0)) * i64::from(z.max(0));
        if needs > area {
            return Err(PackedNodeError::PinnedSpaceShort {
                room: shown,
                space: PinnedSpaceShortfall::AreaExceeded {
                    needs,
                    floors: (needs + area - 1) / area.max(1),
                },
            });
        }
    }
    Ok(())
}

/// The translation that stands `packed` in its pinned room, as close to the
/// pins as it can while clear of all of them ([`PinnedRoom::place`]).
///
/// ponytail: one floor only; stacking layers upward is what a room too small
/// for any placement needs.
fn pinned_offset(
    packed: &PackedFreeLeaves,
    room: &PinnedRoom,
    top: i32,
    west: i32,
) -> Result<crate::compile::geometry::Anchor, PackedNodeError> {
    let (mut min, mut max) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
    for at in &packed.halo {
        min = (min.0.min(at.x), min.1.min(at.z));
        max = (max.0.max(at.x), max.1.max(at.z));
    }
    // Room the layout needs west of its halos: the fabric's feet and pads.
    min.0 -= west;
    let needs = (max.0 - min.0 + 1, max.1 - min.1 + 1);
    let placed = room.place(needs, top);
    if std::env::var_os("REDA_TRACE_PINNED").is_some() {
        eprintln!("reda: pinned footprint {needs:?} under lanes to {top} placed at {placed:?}");
    }
    let (x, z) = placed.ok_or_else(|| {
        let extent = room.extent();
        PackedNodeError::PinnedRegionTooSmall {
            needs,
            room: (extent.0.unwrap_or(i32::MAX), extent.1.unwrap_or(i32::MAX)),
        }
    })?;
    Ok(crate::compile::geometry::Anchor {
        x: x - min.0,
        y: 0,
        z: z - min.1,
    })
}

/// Hand a certified node on as one parent-connectable child of `parent`.
///
/// The conversion is a remap, not a rebuild.  The world is the world
/// [`certify_root_world`] accepted -- checked here against the certificate's
/// own fingerprint, so an edited world cannot be relabelled as a certified
/// child -- and the masks come from the single derivation every packable
/// artifact uses, [`parent_connectable_masks`].  What changes is identity:
/// each root port is re-keyed onto a node-owned
/// [`PhysicalEndpointId`] in the node's declared order, exactly as a leaf's
/// ports are, so the next level addresses this node's `b` rather than some
/// grandchild's cell.  The contract each port carries is the contract the
/// child interface really has; nothing here invents one.
///
/// [`ChunkId`] comes from [`node_chunk_id`]: the node's netlist under the
/// intended parent.  It is deliberately not derived from the packed world --
/// two identical nodes packed into different frames are the same child, and a
/// node repacked after a router change is still the same child.
pub(crate) fn into_parent_connectable(
    node: &PackedNode,
    parent: &ChunkId,
) -> Result<FreeLeafArtifact, PackedNodeError> {
    let actual = canonical_world_fingerprint(&node.world);
    if actual != node.certificate.world_fingerprint {
        return Err(PackedNodeError::UncertifiedWorld {
            certified: node.certificate.world_fingerprint.clone(),
            actual,
        });
    }
    let chunk = node_chunk_id(&node.netlist, parent)?;

    let mut interfaces = BTreeMap::new();
    for (role, signals) in [
        (PortRole::Input, &node.netlist.inputs),
        (PortRole::Output, &node.netlist.outputs),
    ] {
        for (index, signal) in signals.iter().enumerate() {
            let port =
                PortId(u32::try_from(index).map_err(|_| PackedNodeError::NodePortIndexOverflow)?);
            let endpoint = match role {
                PortRole::Input => PhysicalEndpointId::PrimaryInput(port),
                PortRole::Output => PhysicalEndpointId::DeclaredOutput(port),
            };
            let source = node
                .root_ports
                .iter()
                .find(|candidate| candidate.port.signal == *signal && candidate.port.role == role)
                .ok_or_else(|| PackedNodeError::MissingInterface {
                    chunk: chunk.clone(),
                    signal: signal.clone(),
                    role,
                })?;
            // Preserve the terminal contract, whether its handover belongs to
            // a child or was built by this node for an input fanout.
            interfaces.insert(
                FreeLeafInterfaceId {
                    chunk: chunk.clone(),
                    endpoint,
                },
                ParentConnectableInterface {
                    signal: signal.clone(),
                    role,
                    pin: source.port.pin,
                    contract: source.contract,
                },
            );
        }
    }

    let masks = parent_connectable_masks(&node.world, &interfaces)
        .ok_or(PackedNodeError::EmptyNodeWorld)?;
    // `pack_free_leaves` refuses an interface outside the access mask, and so
    // does the trunk router. Saying it here names the port rather than the
    // endpoint id a later refusal would.
    for interface in interfaces.values() {
        if !masks.access.contains(&interface.pin.at) {
            return Err(PackedNodeError::NodePortOutsideAccess {
                signal: interface.signal.clone(),
                at: interface.pin.at,
            });
        }
    }

    Ok(FreeLeafArtifact {
        chunk,
        netlist: node.netlist.clone(),
        world: node.world.clone(),
        interfaces,
        occupied: masks.occupied,
        halo: masks.halo,
        access: masks.access,
        // The node's own frame is this artifact's frame, so the accumulated
        // metadata travels across untouched. It is translated again only when
        // a parent packs this artifact, which is the one translation per level.
        gates: node.gates.clone(),
        certificate: Some(node.certificate.clone()),
    })
}

/// Every geometric invariant a packed node must already satisfy to be packed
/// again, checked against the world that was actually composed.
///
/// This is the parent-connectable contract, not the corridor contract.  A
/// packed parent joins its children through [`runway_core`] terminals wherever
/// they happen to sit, so what nesting needs is that each root port really is
/// one of those: child-built handover hardware behind it, a clear straight
/// runway in front of it, and enough space around it that no two ports' guards
/// reach each other.  The caller row and [`PORTAL_Y`](super::allocation) plane
/// that `root_pin_row` enforces belong to the allocating shape, which places
/// its ports rather than inheriting them, and a translation-only packing
/// cannot produce them -- see this module's own header.
///
/// The runway check is deliberately made against the finished world rather
/// than the masks: it is the end-to-end statement that the root guard columns
/// really did hold while the internal trunks were routed.
fn validate_packed_root_boundary(
    packed: &PackedFreeLeaves,
    world: &World,
    root_ports: &[PackedRootPort],
) -> Result<(), PackedNodeError> {
    let (size_x, size_y, size_z) = world.size();
    let top = size_y - 1;
    let mut resolved = Vec::with_capacity(root_ports.len());
    for port in root_ports {
        let owner = port
            .interface
            .as_ref()
            .map(|id| {
                let leaf = packed.placements.get(&id.chunk).ok_or_else(|| {
                    PackedNodeError::MissingChild {
                        chunk: id.chunk.clone(),
                    }
                })?;
                leaf.interfaces
                    .get(id)
                    .ok_or_else(|| PackedNodeError::MissingInterface {
                        chunk: id.chunk.clone(),
                        signal: port.port.signal.clone(),
                        role: port.port.role,
                    })?;
                Ok::<_, PackedNodeError>(leaf)
            })
            .transpose()?;
        let direction = match port.port.role {
            PortRole::Input => port.port.pin.toward.opposite(),
            PortRole::Output => port.port.pin.toward,
        };
        if matches!(direction, Facing::Up | Facing::Down) {
            return Err(PackedNodeError::RootPortOutOfPlane {
                signal: port.port.signal.clone(),
                toward: direction,
            });
        }
        // The handover is the child's own hardware.  Without it this is an
        // empty cell that happens to carry a name.
        let handover = port.port.pin.handover(port.port.role);
        if owner.map_or_else(
            || world.get(handover.x, handover.y, handover.z).kind == BlockKind::Air,
            |leaf| !leaf.occupied.contains(&handover),
        ) {
            return Err(PackedNodeError::RootPortHandoverUnbuilt {
                signal: port.port.signal.clone(),
                at: handover,
            });
        }
        let core = runway_core(port.port.pin.at, direction);
        for at in &core {
            if at.x < 0
                || at.y < 0
                || at.z < 0
                || at.x >= size_x
                || at.y >= size_y
                || at.z >= size_z
            {
                return Err(PackedNodeError::RootPortRunwayEscapes {
                    signal: port.port.signal.clone(),
                    at: *at,
                });
            }
            let kind = world.get(at.x, at.y, at.z).kind;
            if kind != BlockKind::Air {
                return Err(PackedNodeError::RootPortRunwayBlocked {
                    signal: port.port.signal.clone(),
                    at: *at,
                    kind,
                });
            }
        }
        resolved.push((port, direction, core));
    }

    for (index, (port, direction, _)) in resolved.iter().enumerate() {
        // A child's port keeps its whole guard column. A port this node built
        // at a caller's pin is guarded on its own layer, one either side: a
        // pin stacked above another on a wall is a different cell, not a
        // column the lower one owns. On one shared layer the two agree.
        let at = port.port.pin.at;
        let guard = match port.interface {
            Some(_) => terminal_guard_cells(at, *direction, top),
            None => terminal_guard_cells_from(at, *direction, at.y - 1, at.y + 1),
        }
        .into_iter()
        .collect::<BTreeSet<_>>();
        for (other, _, other_core) in resolved.iter().skip(index + 1) {
            let apart = plane_apart(port.port.pin.at, other.port.pin.at);
            if apart < i64::from(PORTAL_PITCH) {
                return Err(PackedNodeError::RootPortsTooClose {
                    first: port.port.signal.clone(),
                    second: other.port.signal.clone(),
                    apart,
                });
            }
            if let Some(at) = other_core.iter().find(|at| guard.contains(at)) {
                return Err(PackedNodeError::RootPortGuardsAnother {
                    first: port.port.signal.clone(),
                    second: other.port.signal.clone(),
                    at: *at,
                });
            }
        }
    }
    Ok(())
}

/// The children by stable identity, with their interfaces checked against the
/// logical ports they claim to expose.
fn index_children(
    children: &[FreeLeafArtifact],
) -> Result<BTreeMap<ChunkId, &FreeLeafArtifact>, PackedNodeError> {
    let mut by_chunk = BTreeMap::new();
    for child in children {
        for (role, expected) in [
            (PortRole::Input, &child.netlist.inputs),
            (PortRole::Output, &child.netlist.outputs),
        ] {
            let actual = child
                .interfaces
                .values()
                .filter(|interface| interface.role == role)
                .map(|interface| interface.signal.clone())
                .collect::<BTreeSet<_>>();
            if actual != expected.iter().cloned().collect::<BTreeSet<_>>() {
                return Err(PackedNodeError::ChildInterfaceContract {
                    chunk: child.chunk.clone(),
                    role,
                    expected: expected.clone(),
                    actual: actual.into_iter().collect(),
                });
            }
        }
        if by_chunk.insert(child.chunk.clone(), child).is_some() {
            return Err(PackedNodeError::DuplicateChild {
                chunk: child.chunk.clone(),
            });
        }
    }
    Ok(by_chunk)
}

/// Every node gate is certified by exactly one child, and no child certified
/// anything else.
///
/// This is what makes certifying the composed world against `node` a statement
/// about `node`.  Without it the certifier would happily prove that some other
/// circuit computes this netlist's function on the vectors it happened to try.
fn prove_exact_gate_cover(
    node: &Netlist,
    by_chunk: &BTreeMap<ChunkId, &FreeLeafArtifact>,
) -> Result<(), PackedNodeError> {
    let declared = node
        .gates
        .iter()
        .map(|gate| (gate.output.as_str(), gate))
        .collect::<BTreeMap<_, _>>();
    let mut covered: BTreeMap<&str, ChunkId> = BTreeMap::new();
    for (chunk, child) in by_chunk {
        for gate in &child.netlist.gates {
            let Some(expected) = declared.get(gate.output.as_str()) else {
                return Err(PackedNodeError::ForeignGate {
                    chunk: chunk.clone(),
                    gate: gate.output.clone(),
                });
            };
            if *expected != gate {
                return Err(PackedNodeError::GateMismatch {
                    chunk: chunk.clone(),
                    gate: gate.output.clone(),
                });
            }
            if let Some(first) = covered.insert(expected.output.as_str(), chunk.clone()) {
                return Err(PackedNodeError::GateCoveredTwice {
                    gate: gate.output.clone(),
                    first,
                    second: chunk.clone(),
                });
            }
        }
    }
    for output in declared.keys() {
        if !covered.contains_key(output) {
            return Err(PackedNodeError::GateUncovered {
                gate: (*output).to_owned(),
            });
        }
    }
    Ok(())
}

/// The one interface a packed child exposes for `signal` in `role`.
fn interface_of<'a>(
    packed: &'a PackedFreeLeaves,
    chunk: &ChunkId,
    signal: &str,
    role: PortRole,
) -> Result<(FreeLeafInterfaceId, &'a ParentConnectableInterface), PackedNodeError> {
    let leaf = packed
        .placements
        .get(chunk)
        .ok_or_else(|| PackedNodeError::MissingChild {
            chunk: chunk.clone(),
        })?;
    let mut matching = leaf
        .interfaces
        .iter()
        .filter(|(_, interface)| interface.role == role && interface.signal == signal);
    let found = matching
        .next()
        .ok_or_else(|| PackedNodeError::MissingInterface {
            chunk: chunk.clone(),
            signal: signal.to_owned(),
            role,
        })?;
    if matching.next().is_some() {
        return Err(PackedNodeError::AmbiguousInterface {
            chunk: chunk.clone(),
            signal: signal.to_owned(),
            role,
        });
    }
    Ok((found.0.clone(), found.1))
}

fn root_port(
    signal: &str,
    role: PortRole,
    interface: FreeLeafInterfaceId,
    packed: &ParentConnectableInterface,
) -> PackedRootPort {
    PackedRootPort {
        port: RootPort {
            signal: signal.to_owned(),
            role,
            pin: packed.pin,
        },
        interface: Some(interface),
        contract: packed.contract,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::SignalContract;
    use crate::compile::fragment_synth::leaf::{synthesise_free_leaf, LEAF_PITCHES};
    use crate::compile::fragment_synth::packing::pack_free_leaves;
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::routing::DurablePhysicalRouter;
    use crate::compile::topology::SignalPolarity;
    use crate::compile::Gate;
    use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;

    #[test]
    fn shared_input_fanout_is_parent_owned_certified_nested_and_worker_invariant() {
        let inner = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["b".into(), "c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["a"])],
        };
        let outer = Netlist {
            inputs: vec!["x".into()],
            outputs: inner.outputs.clone(),
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        };
        let sibling = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["a".into()],
            gates: vec![Gate::nor("a", &["x"])],
        };
        let parent = root_chunk_id(&outer).unwrap();
        let inner_id = node_chunk_id(&inner, &parent).unwrap();
        let leaves = leaves_under(&inner, &inner_id);
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let build = |workers| {
            let node = synthesise_packed_node(
                &inner,
                &leaves,
                &DurablePhysicalRouter,
                &search,
                &certification,
                CertificationWorkers::bounded(workers),
            )
            .unwrap();
            assert!(node.root_ports[0].interface.is_none());
            assert_eq!(node.trunks.len(), 1);
            assert_eq!(node.trunks[0].branches.len(), 2);
            assert_eq!(
                node.root_ports
                    .iter()
                    .filter(|root| root.port.role == PortRole::Input)
                    .count(),
                1
            );
            let child = into_parent_connectable(&node, &parent).unwrap();
            let mut children = leaves_under(&sibling, &parent);
            children.push(child);
            let outer_node = synthesise_packed_node(
                &outer,
                &children,
                &DurablePhysicalRouter,
                &search,
                &certification,
                CertificationWorkers::bounded(workers),
            )
            .unwrap();
            (node, outer_node)
        };
        let (node, outer_node) = build(1);
        let (parallel, parallel_outer) = build(4);
        assert_eq!(node.fingerprint, parallel.fingerprint);
        assert_eq!(outer_node.fingerprint, parallel_outer.fingerprint);
        assert!(!node.certificate.measurements.is_empty());
        assert!(!outer_node.certificate.measurements.is_empty());
        let mut cut = node.world.clone();
        for block in node.trunks[0].owned_blocks() {
            cut.set(
                block.at.x,
                block.at.y,
                block.at.z,
                crate::redstone::world::block::BlockState::air(),
            );
        }
        assert!(matches!(
            certify_root_world(
                &cut,
                &inner,
                &node
                    .root_ports
                    .iter()
                    .map(|root| root.port.clone())
                    .collect::<Vec<_>>(),
                &certification,
                CertificationWorkers::serial()
            ),
            Err(CandidateCertificationError::FunctionalMismatch { .. })
        ));
    }

    /// A `width` by `depth` rectangle closed on all four sides: an input on
    /// the west edge and outputs facing out of the other three.
    fn boxed(width: i32, depth: i32) -> (PortPlacements, Vec<String>) {
        let at = |x, z| crate::compile::geometry::Anchor { x, y: 1, z };
        let mut pins = PortPlacements::default();
        pins.pin("x", at(0, depth / 2), Facing::East);
        pins.pin("b", at(width - 1, depth / 2), Facing::East);
        pins.pin("c", at(width / 2, 0), Facing::North);
        pins.pin("s", at(width / 2, depth - 1), Facing::South);
        (pins, vec!["x".into()])
    }

    /// The gate-count estimate is a lower bound, and only a room bounded both
    /// ways can be short: a 100-by-100 box holds ten gates' leaves on one
    /// floor, not thirty, and opening one side lifts the bound.
    #[test]
    fn the_pinned_floor_estimate_flags_only_a_certain_shortfall() {
        let (pins, inputs) = boxed(100, 100);
        let room = PinnedRoom::of(&pins, &inputs);
        assert_eq!(room.extent(), (Some(84), Some(84)));
        assert_eq!(pinned_floors_short(10, &room), None);
        assert_eq!(pinned_floors_short(30, &room), Some((30 * MIN_LEAF_AREA_PER_GATE, 2)));

        // The east output turned to face north leaves the east side open;
        // the north and south outputs still close their sides to the callers
        // reading them.
        let mut open = pins.clone();
        open.pin("b", crate::compile::geometry::Anchor { x: 99, y: 1, z: 50 }, Facing::North);
        let room = PinnedRoom::of(&open, &inputs);
        assert_eq!(room.extent(), (None, Some(84)));
        assert_eq!(pinned_floors_short(30, &room), None);
    }

    /// A footprint stands clear of every pin, inside the margins of closed
    /// sides, and grows past an open one when the box is too small.
    #[test]
    fn a_pinned_footprint_avoids_every_pin_and_uses_an_open_side() {
        let (mut pins, inputs) = boxed(60, 60);
        // A pin in the middle of the box, as a glyph's outputs are.
        pins.pin("g", crate::compile::geometry::Anchor { x: 30, y: 1, z: 30 }, Facing::West);
        let room = PinnedRoom::of(&pins, &inputs);
        let clear = |pins: &PortPlacements, (x, z): (i32, i32), size: (i32, i32)| {
            pins.iter().all(|(_, pin)| {
                pin.at.x < x - PINNED_MARGIN
                    || pin.at.x > x + size.0 - 1 + PINNED_MARGIN
                    || pin.at.z < z - PINNED_MARGIN
                    || pin.at.z > z + size.1 - 1 + PINNED_MARGIN
            })
        };
        let small = (10, 10);
        let corner = room.place(small, 0).expect("a small footprint fits beside the middle pin");
        assert!(clear(&pins, corner, small));
        assert!(corner.0 >= PINNED_MARGIN && corner.1 >= PINNED_MARGIN);
        // Too wide for the closed box: refused, until its east side opens.
        let wide = (70, 10);
        assert_eq!(room.place(wide, 0), None);
        pins.pin("b", crate::compile::geometry::Anchor { x: 59, y: 1, z: 30 }, Facing::North);
        let room = PinnedRoom::of(&pins, &inputs);
        let corner = room.place(wide, 0).expect("the open east side takes the overflow");
        assert!(clear(&pins, corner, wide));
        // Past the open side the room is a half-plane, but never past a side
        // a caller stands on: the north and south outputs are read from there,
        // so a footprint deeper than the box fits nowhere.
        let deep = (10, 200);
        assert_eq!(room.place(deep, 0), None);
        // An input's side is the caller's just the same.
        let mut fed = pins.clone();
        fed.pin("y", crate::compile::geometry::Anchor { x: 30, y: 1, z: 59 }, Facing::North);
        let room = PinnedRoom::of(&fed, &["x".into(), "y".into()]);
        assert_eq!(room.place(deep, 0), None);
        let corner = room.place(wide, 0).expect("a shallow footprint still fits east");
        assert!(corner.1 + wide.1 - 1 <= 59 - PINNED_MARGIN);
        // A trunk climbing to a high lane needs its stair run in front of
        // the pin it leaves: `g`'s output is fed from the east, so nothing
        // stands in the 46 cells east of it.
        for top in [0, 40] {
            let (x, z) = room.place(small, top).expect("the stair run fits beside the pin");
            let run = PINNED_MARGIN.max(top - 1 + 7);
            let in_run = x <= 30 + run && x + small.0 > 30 && (z - 30).abs() <= small.1 + PINNED_MARGIN;
            assert!(!in_run || top == 0, "({x}, {z}) stands in g's stair run");
        }
    }

    /// Built children are checked before any layout is searched: one larger
    /// than the room, or footprints adding up past its area, is refused with
    /// the shortfall named.
    #[test]
    fn a_pinned_room_too_small_for_its_children_is_refused_by_name() {
        let net = fanout_node();
        let children = certified_children(&net);
        let (tiny, inputs) = boxed(20, 20);
        assert!(matches!(
            pinned_space(&children, &PinnedRoom::of(&tiny, &inputs)),
            Err(PackedNodeError::PinnedSpaceShort {
                space: PinnedSpaceShortfall::ChildTooLarge { .. },
                ..
            })
        ));
        let footprints = children
            .iter()
            .map(|child| {
                let xs = child.halo.iter().map(|at| at.x);
                let zs = child.halo.iter().map(|at| at.z);
                (
                    xs.clone().max().unwrap() - xs.min().unwrap() + 1,
                    zs.clone().max().unwrap() - zs.min().unwrap() + 1,
                )
            })
            .collect::<Vec<_>>();
        let widest = footprints.iter().map(|f| f.0).max().unwrap();
        let deepest = footprints.iter().map(|f| f.1).max().unwrap();
        // Room for the largest child alone, but not for all of them.
        let (snug, inputs) = boxed(widest + 2 * PINNED_MARGIN, deepest + 2 * PINNED_MARGIN);
        assert!(matches!(
            pinned_space(&children, &PinnedRoom::of(&snug, &inputs)),
            Err(PackedNodeError::PinnedSpaceShort {
                space: PinnedSpaceShortfall::AreaExceeded { floors, .. },
                ..
            }) if floors >= 2
        ));
    }

    /// `b` and `c` both follow `x` through a shared inverter, so the node has
    /// one root input, one internal fanout trunk, and two root outputs -- the
    /// smallest shape that exercises all three at once.
    fn fanout_node() -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into(), "c".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        }
    }

    /// One certified free leaf per gate.
    fn certified_children(net: &Netlist) -> Vec<FreeLeafArtifact> {
        leaves_under(net, &root_chunk_id(net).unwrap())
    }

    fn portal_score_fixture() -> (PackedFreeLeaves, Vec<PortalDemand>, Vec<ChunkId>) {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        };
        let children = certified_children(&net);
        let source = children
            .iter()
            .flat_map(|child| child.interfaces.iter())
            .find(|(_, interface)| interface.signal == "a" && interface.role == PortRole::Output)
            .map(|(id, _)| id.clone())
            .unwrap();
        let sink = children
            .iter()
            .flat_map(|child| child.interfaces.iter())
            .find(|(_, interface)| interface.signal == "a" && interface.role == PortRole::Input)
            .map(|(id, _)| id.clone())
            .unwrap();
        let packed = pack_free_leaves(&children).unwrap();
        let order = packed.placements.keys().cloned().collect();
        (
            packed,
            vec![PortalDemand {
                signal: "a".into(),
                source,
                sink,
            }],
            order,
        )
    }

    #[test]
    fn portal_score_improves_when_a_sink_mouth_moves_closer_in_z() {
        let (packed, demands, order) = portal_score_fixture();
        let source_z = portal_mouth(&packed, &demands[0].source).2 as i32;
        let with_sink_z = |z| {
            let mut candidate = packed.clone();
            candidate
                .placements
                .get_mut(&demands[0].sink.chunk)
                .unwrap()
                .interfaces
                .get_mut(&demands[0].sink)
                .unwrap()
                .pin
                .at
                .z = z;
            candidate
        };
        let before = portal_layout_score(&with_sink_z(source_z + 5), &demands, &order, 3);
        let after = portal_layout_score(&with_sink_z(source_z + 2), &demands, &order, 3);
        assert!(after < before, "a shorter mouth distance scores first");
        assert!(after.sum_trunk_max_manhattan < before.sum_trunk_max_manhattan);
    }

    #[test]
    fn portal_score_ties_are_stable_and_demand_order_independent() {
        let (packed, mut demands, order) = portal_score_fixture();
        let first = portal_layout_score(&packed, &demands, &order, 1);
        demands.reverse();
        let same = portal_layout_score(&packed, &demands, &order, 1);
        let later = portal_layout_score(&packed, &demands, &order, 2);
        assert_eq!(first, same);
        assert!(
            first < later,
            "original rank is the final deterministic tie-break"
        );
    }

    /// One certified free leaf per gate, every chunk parented to `parent`.
    fn leaves_under(net: &Netlist, parent: &ChunkId) -> Vec<FreeLeafArtifact> {
        let contract = SignalContract {
            polarity: SignalPolarity::Positive,
            strength: MAX_SIGNAL_STRENGTH,
            delay_budget_ticks: 4,
        };
        partition(net, parent, 1)
            .unwrap()
            .iter()
            .map(|chunk| synthesise_free_leaf(chunk, contract, &SearchConfig::checked_defaults(), &LEAF_PITCHES))
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn two_chain_node() -> Netlist {
        Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["c".into(), "d".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["y"]),
                Gate::nor("c", &["a"]),
                Gate::nor("d", &["b"]),
            ],
        }
    }

    fn with_a_tall_lid(mut children: Vec<FreeLeafArtifact>) -> Vec<FreeLeafArtifact> {
        let child = children.first_mut().expect("a child");
        let (x, z) = child.halo.iter().map(|at| (at.x, at.z)).next().unwrap();
        let top = child.halo.iter().map(|at| at.y).max().unwrap();
        for y in top..=top + 2 * PACKED_LANE_PITCH {
            child.halo.insert(Anchor { x, y, z });
        }
        children
    }

    fn children_at_grain(net: &Netlist, grain: usize) -> Vec<FreeLeafArtifact> {
        let contract = SignalContract {
            polarity: SignalPolarity::Positive,
            strength: MAX_SIGNAL_STRENGTH,
            delay_budget_ticks: 4,
        };
        partition(net, &root_chunk_id(net).unwrap(), grain)
            .unwrap()
            .iter()
            .map(|chunk| synthesise_free_leaf(chunk, contract, &SearchConfig::checked_defaults(), &LEAF_PITCHES))
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn node_uses<'a>(
        net: &Netlist,
        children: &'a [FreeLeafArtifact],
    ) -> (
        BTreeMap<String, SignalUse>,
        BTreeMap<ChunkId, &'a FreeLeafArtifact>,
    ) {
        let by_chunk = index_children(children).unwrap();
        let boundaries = by_chunk
            .values()
            .map(|child| ChildBoundary {
                chunk: &child.chunk,
                inputs: &child.netlist.inputs,
                outputs: &child.netlist.outputs,
            })
            .collect::<Vec<_>>();
        let uses = boundary_signal_uses(net, &boundaries).unwrap();
        (uses, by_chunk)
    }

    #[test]
    fn a_seams_band_is_six_or_nine_from_the_constants_and_the_crossing_spans() {
        let one_layer = band_min_width(1).unwrap();
        let two_layers = band_min_width(2).unwrap();
        assert_eq!(
            one_layer,
            2 * (i32::try_from(TERMINAL_RUNWAY_CELLS).unwrap() + 1)
        );
        assert_eq!(two_layers, one_layer + PACKED_LANE_PITCH);
        assert_eq!(one_layer, 6);
        assert_eq!(two_layers, 9);

        let net = two_chain_node();
        let children = children_at_grain(&net, 2);
        assert_eq!(children.len(), 2, "the grain must give two children");
        let (uses, by_chunk) = node_uses(&net, &children);
        let bands = seam_bands(&uses, &by_chunk).unwrap();
        let mut ids = by_chunk.keys().cloned();
        let (first, second) = (ids.next().unwrap(), ids.next().unwrap());
        let pin = |signal: &str, role: PortRole| {
            children
                .iter()
                .flat_map(|child| child.interfaces.values())
                .find(|interface| interface.signal == signal && interface.role == role)
                .map(|interface| interface.pin.at.z)
                .unwrap()
        };
        let pairs = ["a", "b"]
            .into_iter()
            .map(|signal| (pin(signal, PortRole::Output), pin(signal, PortRole::Input)))
            .collect::<Vec<_>>();
        let layers = band_layers(&pairs);
        let count = i32::try_from(layers.iter().copied().max().unwrap()).unwrap() + 1;
        let lid_y = children
            .iter()
            .map(|child| {
                child.halo.iter().map(|at| at.y).max().unwrap()
                    - child.halo.iter().map(|at| at.y).min().unwrap()
            })
            .max()
            .unwrap();
        let sink_child = children
            .iter()
            .find(|child| child.netlist.inputs.contains(&"a".to_string()))
            .unwrap();
        let terminal_y = sink_child
            .interfaces
            .values()
            .filter(|interface| interface.role == PortRole::Input)
            .map(|interface| {
                interface.pin.at.y - sink_child.halo.iter().map(|at| at.y).min().unwrap()
            })
            .max()
            .unwrap();
        let expected = if band_pays(&layers, lid_y, terminal_y).unwrap() {
            band_min_width(count).unwrap()
        } else {
            0
        };
        assert_eq!(bands.width(&first, &second), expected);
        assert!(expected == 0 || expected == one_layer || expected == two_layers);
        assert_eq!(bands.width(&first, &first), 0);
    }

    #[test]
    fn a_fanout_node_derives_no_band() {
        let net = fanout_node();
        let children = certified_children(&net);
        let (uses, by_chunk) = node_uses(&net, &children);
        assert!(seam_bands(&uses, &by_chunk).unwrap().is_none());
    }

    #[test]
    fn a_banded_two_chain_node_certifies_at_terminal_height() {
        let net = two_chain_node();
        let children = with_a_tall_lid(children_at_grain(&net, 2));
        let search = SearchConfig::checked_defaults();
        let build = |policy| {
            synthesise_packed_node_with_bands(
                &net,
                &children,
                &DurablePhysicalRouter,
                &search,
                &CertificationConfig::from_search(&search),
                CertificationWorkers::serial(),
                policy,
            )
        };
        let laned = build(BandPolicy::None).expect("the laned node certifies");
        let banded = build(BandPolicy::Derived).expect("the banded node certifies");
        assert_eq!(banded.packed.placements.len(), 2);
        assert_eq!(banded.trunks.len(), 2);
        assert_eq!(laned.trunks.len(), 2);

        let halo_top = |node: &PackedNode| node.packed.halo.iter().map(|at| at.y).max().unwrap();
        for lane in &laned.trunk_lanes {
            assert!(lane.unwrap() > halo_top(&laned));
        }
        let sink_heights = banded
            .packed
            .placements
            .values()
            .flat_map(|leaf| leaf.interfaces.values())
            .filter(|interface| interface.role == PortRole::Input)
            .map(|interface| interface.pin.at.y)
            .collect::<BTreeSet<_>>();
        for lane in &banded.trunk_lanes {
            let lane = lane.unwrap();
            assert!(
                sink_heights.contains(&lane) || sink_heights.contains(&(lane - PACKED_LANE_PITCH)),
                "banded trunk {lane} is not a band layer over sinks at {sink_heights:?}"
            );
        }
        let mut spans = banded
            .packed
            .placements
            .values()
            .map(|leaf| {
                (
                    leaf.halo.iter().map(|at| at.x).min().unwrap(),
                    leaf.halo.iter().map(|at| at.x).max().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        spans.sort();
        let one_layer = 2 * (i32::try_from(TERMINAL_RUNWAY_CELLS).unwrap() + 1);
        let gap = spans[1].0 - spans[0].1 - 1;
        assert!(gap >= one_layer);
        assert!(banded
            .trunks
            .iter()
            .flat_map(|trunk| trunk.cells.iter())
            .any(|block| block.at.x > spans[0].1 && block.at.x < spans[1].0));
        let (old, new) = (laned.trunk_physical_totals, banded.trunk_physical_totals);
        assert_eq!(new.vertical_risers, old.vertical_risers);
        assert_eq!(new.cells, old.cells + gap as usize * banded.trunks.len());
        assert!(new.repeaters <= old.repeaters + banded.trunks.len());
    }

    /// The first certified packed root: three independently certified leaves,
    /// packed, joined by one fanout trunk, and proved to compute the node's own
    /// netlist at the node's own ports by the shipping certifier.
    #[test]
    fn a_packed_fanout_node_is_certified_at_its_root_ports_and_is_deterministic() {
        let net = fanout_node();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let build = || {
            synthesise_packed_node(
                &net,
                &certified_children(&net),
                &DurablePhysicalRouter,
                &search,
                &certification,
                CertificationWorkers::serial(),
            )
            .expect("a packed fanout node certifies")
        };

        let node = build();
        assert_eq!(node.packed.placements.len(), 3, "one leaf per gate");
        assert_eq!(
            node.layout_rank, 0,
            "the greedy layout still routes, so no repair was needed"
        );
        assert_eq!(node.trunks.len(), 1, "one internal signal is one trunk");
        assert_eq!(node.trunk_physical_metrics.len(), node.trunks.len());
        assert_eq!(
            node.trunk_physical_totals.cells,
            node.trunks
                .iter()
                .map(|trunk| trunk.cells.len())
                .sum::<usize>()
        );
        assert_eq!(
            node.trunk_physical_totals.floors,
            node.trunks
                .iter()
                .map(|trunk| trunk.floors.len())
                .sum::<usize>()
        );
        assert_eq!(
            node.trunks[0].branches.len(),
            2,
            "the internal signal fans out to both readers"
        );

        // The root boundary is the node's own declared interface, in declared
        // order, and every port is a distinct cell a caller can reach.
        assert_eq!(
            node.root_ports
                .iter()
                .map(|port| (port.port.signal.as_str(), port.port.role))
                .collect::<Vec<_>>(),
            vec![
                ("x", PortRole::Input),
                ("b", PortRole::Output),
                ("c", PortRole::Output),
            ]
        );
        let cells = node
            .root_ports
            .iter()
            .map(|port| port.port.pin.at)
            .collect::<BTreeSet<_>>();
        assert_eq!(cells.len(), node.root_ports.len());
        // Each port is carried by a child that really owns that signal.
        for port in &node.root_ports {
            let id = port.interface.as_ref().unwrap();
            let leaf = &node.packed.placements[&id.chunk];
            let interface = &leaf.interfaces[id];
            assert_eq!(interface.signal, port.port.signal);
            assert_eq!(interface.role, port.port.role);
            assert_eq!(interface.pin, port.port.pin);
        }

        // The certificate is this world's, not a world the certifier was told
        // about: it fingerprints what was actually composed, and it measured
        // something.
        assert_eq!(
            node.certificate.world_fingerprint,
            canonical_world_fingerprint(&node.world)
        );
        assert!(!node.certificate.measurements.is_empty());

        // And the certificate is not vacuous: cut the one trunk that joins the
        // children and the same authority refuses the same node.
        let mut cut = node.world.clone();
        for block in node.trunks[0].cells.iter().chain(&node.trunks[0].floors) {
            cut.set(
                block.at.x,
                block.at.y,
                block.at.z,
                crate::redstone::world::block::BlockState::air(),
            );
        }
        let refusal = certify_root_world(
            &cut,
            &net,
            &node
                .root_ports
                .iter()
                .map(|port| port.port.clone())
                .collect::<Vec<_>>(),
            &certification,
            CertificationWorkers::serial(),
        )
        .expect_err("a node with its trunk cut does not compute its netlist");
        assert!(
            matches!(
                refusal,
                CandidateCertificationError::FunctionalMismatch { .. }
            ),
            "unexpected refusal: {refusal}"
        );

        // Same inputs, same node -- down to the trunks and the certificate.
        let again = build();
        assert_eq!(again.fingerprint, node.fingerprint);
        assert_eq!(again.trunks, node.trunks);
        assert_eq!(again.root_ports, node.root_ports);
        assert_eq!(again.certificate, node.certificate);
    }

    /// The two-level regression: an inner node of two certified leaves becomes
    /// one child, stands beside an ordinary certified leaf, and the outer node
    /// routes the signal between them and is certified on its own netlist.
    #[test]
    fn a_certified_node_packs_as_one_child_of_an_outer_node() {
        let outer = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("m", &["a"]),
                Gate::nor("b", &["m"]),
            ],
        };
        let sibling = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["a".into()],
            gates: vec![Gate::nor("a", &["x"])],
        };
        let inner = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("m", &["a"]), Gate::nor("b", &["m"])],
        };
        let outer_id = root_chunk_id(&outer).unwrap();
        let inner_id = node_chunk_id(&inner, &outer_id).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);

        let level = |net: &Netlist, children: &[FreeLeafArtifact]| {
            synthesise_packed_node(
                net,
                children,
                &DurablePhysicalRouter,
                &search,
                &certification,
                CertificationWorkers::serial(),
            )
        };
        let build = || {
            let node = level(&inner, &leaves_under(&inner, &inner_id))
                .expect("the inner node certifies on its own netlist");
            let child = into_parent_connectable(&node, &outer_id)
                .expect("a certified node converts to one child");
            let mut children = leaves_under(&sibling, &outer_id);
            children.push(child);
            let outer_node = level(&outer, &children).expect("the outer node certifies");
            (node, outer_node)
        };

        let (inner_node, outer_node) = build();

        // Identity is the netlist under the intended parent, not anything the
        // packing decided.
        let child = into_parent_connectable(&inner_node, &outer_id).unwrap();
        assert_eq!(child.chunk, inner_id);
        assert!(
            !inner_node.packed.placements.contains_key(&child.chunk),
            "a node is not one of its own children"
        );
        assert_eq!(child.netlist, inner);

        // Ports are re-keyed onto node-owned endpoints in declared order, and
        // each keeps the contract the cell behind it actually carries.
        assert_eq!(
            child
                .interfaces
                .iter()
                .map(|(id, interface)| (id.endpoint, interface.signal.as_str(), interface.role))
                .collect::<Vec<_>>(),
            vec![
                (
                    PhysicalEndpointId::PrimaryInput(PortId(0)),
                    "a",
                    PortRole::Input
                ),
                (
                    PhysicalEndpointId::DeclaredOutput(PortId(0)),
                    "b",
                    PortRole::Output
                ),
            ]
        );
        for (id, interface) in &child.interfaces {
            assert_eq!(id.chunk, child.chunk);
            let root = inner_node
                .root_ports
                .iter()
                .find(|port| port.port.signal == interface.signal)
                .expect("every node port came from a root port");
            assert_eq!(interface.pin, root.port.pin);
            assert_eq!(
                interface.contract,
                inner_node.packed.placements[&root.interface.as_ref().unwrap().chunk].interfaces
                    [root.interface.as_ref().unwrap()]
                .contract,
                "the port's contract is the cell's own"
            );
        }

        // The proof travels with the world it is a proof of.
        assert_eq!(child.certificate.as_ref(), Some(&inner_node.certificate));
        assert_eq!(
            canonical_world_fingerprint(&child.world),
            inner_node.certificate.world_fingerprint
        );

        // An edited world is not the certified one, and cannot be handed on
        // wearing its proof.
        let mut relabelled = inner_node.clone();
        let at = *relabelled
            .packed
            .halo
            .iter()
            .find(|at| relabelled.world.get(at.x, at.y, at.z).kind == BlockKind::Air)
            .expect("a packed halo has a free cell");
        relabelled
            .world
            .set(at.x, at.y, at.z, crate::compile::stone());
        let refusal = into_parent_connectable(&relabelled, &outer_id)
            .expect_err("an edited world is not the world that was certified");
        assert!(
            matches!(refusal, PackedNodeError::UncertifiedWorld { .. }),
            "unexpected refusal: {refusal}"
        );

        // And the outer level really is two children joined by one trunk.
        assert_eq!(outer_node.packed.placements.len(), 2);
        assert!(outer_node.packed.placements.contains_key(&inner_id));
        assert_eq!(
            outer_node.trunks.len(),
            1,
            "`a` is the one inter-node trunk"
        );
        assert!(!outer_node.certificate.measurements.is_empty());

        // Non-vacuous: cut the inter-node trunk and the same authority refuses.
        let mut cut = outer_node.world.clone();
        for block in outer_node.trunks[0]
            .cells
            .iter()
            .chain(&outer_node.trunks[0].floors)
        {
            cut.set(
                block.at.x,
                block.at.y,
                block.at.z,
                crate::redstone::world::block::BlockState::air(),
            );
        }
        let refusal = certify_root_world(
            &cut,
            &outer,
            &outer_node
                .root_ports
                .iter()
                .map(|port| port.port.clone())
                .collect::<Vec<_>>(),
            &certification,
            CertificationWorkers::serial(),
        )
        .expect_err("a cut inter-node trunk does not compute the outer netlist");
        assert!(
            matches!(
                refusal,
                CandidateCertificationError::FunctionalMismatch { .. }
            ),
            "unexpected refusal: {refusal}"
        );

        // Same inputs, same hierarchy.
        let (again_inner, again_outer) = build();
        assert_eq!(again_inner.fingerprint, inner_node.fingerprint);
        assert_eq!(again_outer.fingerprint, outer_node.fingerprint);
        assert_eq!(again_outer.trunks, outer_node.trunks);
        assert_eq!(again_outer.root_ports, outer_node.root_ports);
        assert_eq!(again_outer.certificate, outer_node.certificate);
    }

    /// **The root cause, in one node.**
    ///
    /// `e` drives `f`, so the leaf holding `e` must stand west of the leaf
    /// holding `f` or their east-facing runways point away from each other.
    /// For these names the canonical [`ChunkId`] order is the reverse of that,
    /// and generic packing -- which knows nothing about the netlist -- duly
    /// places the reader first. Packed-node synthesis does not: it orders its
    /// children by the connectivity it already derived, and the same two
    /// leaves route and certify.
    #[test]
    fn a_node_places_drivers_first_where_fingerprint_order_would_reverse_them() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["f".into()],
            gates: vec![Gate::nor("e", &["x"]), Gate::nor("f", &["e"])],
        };
        let children = certified_children(&net);
        assert_eq!(children.len(), 2);
        let owner = |signal: &str, role: PortRole| {
            children
                .iter()
                .find(|child| {
                    child
                        .interfaces
                        .values()
                        .any(|interface| interface.role == role && interface.signal == signal)
                })
                .map(|child| child.chunk.clone())
                .expect("a chunk owns each end of the boundary")
        };
        let driver = owner("e", PortRole::Output);
        let reader = owner("e", PortRole::Input);
        assert_ne!(driver, reader);

        // The fixture only means something if these names really do sort the
        // wrong way round.
        assert!(
            reader < driver,
            "this fixture needs a fingerprint order that reverses the dataflow"
        );

        // Generic packing follows the fingerprint, and puts the reader west.
        let generic = pack_free_leaves(&children).expect("the leaves pack");
        assert!(
            generic.placements[&reader].translation.x < generic.placements[&driver].translation.x,
            "generic packing is expected to place these in the reversed order"
        );

        // The node path puts the driver west, and the node builds.
        let search = SearchConfig::checked_defaults();
        let node = synthesise_packed_node(
            &net,
            &children,
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            CertificationWorkers::serial(),
        )
        .expect("dataflow order lets the reversed pair route and certify");
        assert!(
            node.packed.placements[&driver].translation.x
                < node.packed.placements[&reader].translation.x,
            "the driver must be placed before its reader"
        );
        assert!(
            node.layout_rank > 0,
            "the selected layout keeps its original envelope rank after re-ranking"
        );
        let chosen = node
            .candidate_scores
            .iter()
            .find(|row| row.original_rank == node.layout_rank)
            .expect("the accepted original candidate appears in the score table");
        let greedy = node
            .candidate_scores
            .iter()
            .find(|row| row.original_rank == 0)
            .expect("rank zero appears in the score table");
        assert_eq!(chosen.routed, Some(true));
        assert!(
            chosen.score < greedy.score,
            "the accepted legal layout must have a lower portal score than greedy rank zero"
        );
        assert!(chosen.trunk_metrics.is_some());
        assert!(chosen.trunk_totals.is_some());
        assert_eq!(node.trunks.len(), 1);
        assert!(!node.certificate.measurements.is_empty());

        // And the caller's vector order still cannot reach any of it.
        let mut reversed = children.clone();
        reversed.reverse();
        let again = synthesise_packed_node(
            &net,
            &reversed,
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            CertificationWorkers::serial(),
        )
        .expect("input order is not an input");
        assert_eq!(again.fingerprint, node.fingerprint);
        assert_eq!(again.packed, node.packed);
        assert_eq!(again.layout_rank, node.layout_rank);
    }

    /// **Multi-trunk lanes.** Two trunks cross into the same leaf -- `z` reads
    /// one operand from each branch -- which is the shape that had no second
    /// route at all before lanes existed.
    ///
    /// The operands are independent, so neither trunk is redundant: cutting
    /// either one changes what the node computes, which is what makes the
    /// refusal at the end of this test mean something.
    #[test]
    fn two_trunks_into_one_leaf_hold_separate_lanes_and_never_touch() {
        let net = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["z".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["y"]),
                Gate::nor("z", &["a", "b"]),
            ],
        };
        let search = SearchConfig::checked_defaults();
        let node = synthesise_packed_node(
            &net,
            &certified_children(&net),
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            CertificationWorkers::serial(),
        )
        .expect("two trunks into one leaf route on their own lanes");
        assert_eq!(node.trunks.len(), 2, "one trunk per operand");

        // The heights this composition assigned. Where the packer left the
        // reader a band, both trunks are guided to a band layer -- a sink's
        // terminal height, or one pitch above it; otherwise one lane each, a
        // full isolation pitch apart above the halo lid. Either way the
        // isolation the rest of this test proves must hold.
        let lanes = node.trunk_lanes.clone();
        assert_eq!(lanes.len(), node.trunks.len());
        assert!(lanes.iter().all(Option::is_some));
        let halo_top = node.packed.halo.iter().map(|at| at.y).max().unwrap();
        let sink_heights = node
            .packed
            .placements
            .values()
            .flat_map(|leaf| leaf.interfaces.values())
            .filter(|interface| interface.role == PortRole::Input)
            .map(|interface| interface.pin.at.y)
            .collect::<BTreeSet<_>>();
        let band_layer = |lane: i32| {
            sink_heights.contains(&lane) || sink_heights.contains(&(lane - PACKED_LANE_PITCH))
        };
        let banded = lanes.iter().all(|lane| band_layer(lane.unwrap()));
        if !banded {
            for pair in lanes.windows(2) {
                assert_eq!(
                    pair[1].unwrap() - pair[0].unwrap(),
                    3,
                    "lanes {lanes:?} are not one isolation pitch apart"
                );
            }
            for lane in &lanes {
                let lane = lane.unwrap();
                assert!(lane > halo_top, "lane {lane} is inside the packed volume");
            }
        }

        // No cell is claimed twice, and no conductor of one trunk lies inside
        // the two-hop ball around a conductor of the other -- the same reach
        // the leaf halo claims, so neither can energise the other.
        let owned = |tree: &RealisedRouteTree| {
            tree.owned_blocks()
                .map(|block| block.at)
                .collect::<BTreeSet<_>>()
        };
        assert!(
            owned(&node.trunks[0]).is_disjoint(&owned(&node.trunks[1])),
            "the two trunks share a cell"
        );
        let mut closest = (u32::MAX, None);
        for first in &node.trunks[0].cells {
            for second in &node.trunks[1].cells {
                let apart = first.at.x.abs_diff(second.at.x)
                    + first.at.y.abs_diff(second.at.y)
                    + first.at.z.abs_diff(second.at.z);
                if apart < closest.0 {
                    closest = (apart, Some((first.at, second.at)));
                }
            }
        }
        assert!(
            closest.0 > 2,
            "the trunks come within {}, inside the two-hop coupling ball: {:?}",
            closest.0,
            closest.1
        );

        // No trunk enters a terminal's two-hop coupling region except at its
        // own two ends, and no trunk stands in any terminal's lateral or ring
        // guard. The lid those guards reach to is the packed halo top -- the
        // span the coupling authority actually claims -- not the raised
        // canvas, and this is what says that stopping there guards everything
        // that needed guarding.
        let guard_top = node.packed.halo.iter().map(|at| at.y).max().unwrap();
        let pins = node
            .packed
            .placements
            .values()
            .flat_map(|leaf| leaf.interfaces.values())
            .map(|interface| (interface.pin.at, interface_route_direction(interface)))
            .collect::<Vec<_>>();
        for (index, tree) in node.trunks.iter().enumerate() {
            let ends = tree
                .branches
                .iter()
                .flat_map(|branch| [branch.path[0], branch.path[branch.path.len() - 1]])
                .collect::<BTreeSet<_>>();
            for (pin, direction) in &pins {
                let mine = ends.iter().any(|end| *end == *pin);
                let lateral = terminal_guard_cells(*pin, *direction, guard_top)
                    .into_iter()
                    .collect::<BTreeSet<_>>();
                let core = runway_core(*pin, *direction)
                    .into_iter()
                    .flat_map(|at| (0..=guard_top).map(move |y| Anchor { y, ..at }))
                    .collect::<BTreeSet<_>>();
                for block in &tree.cells {
                    assert!(
                        !lateral.contains(&block.at) || core.contains(&block.at),
                        "trunk {index} stands in a lateral or ring guard at {:?}",
                        block.at
                    );
                    if mine {
                        continue;
                    }
                    let reach = block.at.x.abs_diff(pin.x)
                        + block.at.y.abs_diff(pin.y)
                        + block.at.z.abs_diff(pin.z);
                    assert!(
                        reach > 2,
                        "trunk {index} reaches {:?}, inside the two-hop region of foreign terminal {pin:?}",
                        block.at
                    );
                }
            }
        }

        // Non-vacuous either way round: cut either trunk and the same
        // authority refuses the same netlist.
        for cut_index in 0..node.trunks.len() {
            let mut cut = node.world.clone();
            for block in node.trunks[cut_index]
                .cells
                .iter()
                .chain(&node.trunks[cut_index].floors)
            {
                cut.set(
                    block.at.x,
                    block.at.y,
                    block.at.z,
                    crate::redstone::world::block::BlockState::air(),
                );
            }
            let refusal = certify_root_world(
                &cut,
                &net,
                &node
                    .root_ports
                    .iter()
                    .map(|port| port.port.clone())
                    .collect::<Vec<_>>(),
                &CertificationConfig::from_search(&search),
                CertificationWorkers::serial(),
            )
            .expect_err("a cut trunk does not compute the netlist");
            assert!(
                matches!(
                    refusal,
                    CandidateCertificationError::FunctionalMismatch { .. }
                ),
                "cutting trunk {cut_index} gave {refusal}"
            );
        }
    }

    /// The geometry contract is enforced, not assumed: a root port a parent
    /// could not route to, and two root ports standing on top of each other,
    /// are both refused by type against the world that was built.
    #[test]
    fn an_invalid_packed_root_boundary_is_refused() {
        let net = fanout_node();
        let search = SearchConfig::checked_defaults();
        let node = synthesise_packed_node(
            &net,
            &certified_children(&net),
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            CertificationWorkers::serial(),
        )
        .expect("the fixture certifies");

        let port = &node.root_ports[0];
        let id = port.interface.as_ref().unwrap();
        let leaf = &node.packed.placements[&id.chunk];
        let direction = interface_route_direction(&leaf.interfaces[id]);
        let core = runway_core(port.port.pin.at, direction);

        // A cell in the runway is a port the next level cannot reach.
        let blocked = *core.last().expect("a runway has cells");
        let mut world = node.world.clone();
        world.set(blocked.x, blocked.y, blocked.z, crate::compile::stone());
        let refusal = validate_packed_root_boundary(&node.packed, &world, &node.root_ports)
            .expect_err("a blocked runway is not a port");
        assert!(
            matches!(
                refusal,
                PackedNodeError::RootPortRunwayBlocked { ref signal, at, .. }
                    if signal == &port.port.signal && at == blocked
            ),
            "unexpected refusal: {refusal}"
        );

        // Two ports on one cell have no clear space between their hardware.
        let mut crowded = node.root_ports.clone();
        let mut twin = port.clone();
        twin.port.signal = "twin".into();
        crowded.push(twin);
        let refusal = validate_packed_root_boundary(&node.packed, &node.world, &crowded)
            .expect_err("two ports one portal pitch apart are not a boundary");
        assert!(
            matches!(
                refusal,
                PackedNodeError::RootPortsTooClose { apart: 0, ref second, .. } if second == "twin"
            ),
            "unexpected refusal: {refusal}"
        );
    }

    /// A netlist may declare the same output twice; the boundary map may not.
    #[test]
    fn a_duplicated_declared_output_is_refused_by_type() {
        let net = fanout_node();
        let mut duplicated = net.clone();
        duplicated.outputs.push("c".into());
        let search = SearchConfig::checked_defaults();

        let refusal = synthesise_packed_node(
            &duplicated,
            &certified_children(&net),
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            CertificationWorkers::serial(),
        )
        .expect_err("a port declared twice has one cell and two demands");
        assert!(
            matches!(
                refusal,
                PackedNodeError::DuplicatePort {
                    ref signal,
                    role: PortRole::Output
                } if signal == "c"
            ),
            "unexpected refusal: {refusal}"
        );
    }

    /// A node output that is also read inside the node: its child's caller
    /// cell cannot be both the trunk's source and the caller's receiver, so the
    /// node builds its own handover as one more sink of that trunk. The export
    /// must certify, nest into a parent that reads it, stay worker-invariant,
    /// and die with the trunk that feeds it.
    #[test]
    fn a_node_output_read_inside_the_node_is_exported_through_its_own_handover() {
        let inner = Netlist {
            outputs: vec!["b".into(), "c".into(), "a".into()],
            ..fanout_node()
        };
        let sibling = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["d".into()],
            gates: vec![Gate::nor("d", &["a"])],
        };
        let outer = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into(), "c".into(), "d".into()],
            gates: [inner.gates.clone(), sibling.gates.clone()].concat(),
        };
        let parent = root_chunk_id(&outer).unwrap();
        let inner_id = node_chunk_id(&inner, &parent).unwrap();
        let leaves = leaves_under(&inner, &inner_id);
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let build = |workers| {
            let node = synthesise_packed_node(
                &inner,
                &leaves,
                &DurablePhysicalRouter,
                &search,
                &certification,
                CertificationWorkers::bounded(workers),
            )
            .unwrap();
            let export = node
                .root_ports
                .iter()
                .find(|root| root.port.signal == "a")
                .unwrap();
            assert_eq!(export.port.role, PortRole::Output);
            assert!(export.interface.is_none(), "the node built this handover");
            let trunk = node
                .trunk_signals
                .iter()
                .position(|signal| signal == "a")
                .unwrap();
            assert_eq!(node.trunks[trunk].branches.len(), 3);
            let child = into_parent_connectable(&node, &parent).unwrap();
            let mut children = leaves_under(&sibling, &parent);
            children.push(child);
            let outer_node = synthesise_packed_node(
                &outer,
                &children,
                &DurablePhysicalRouter,
                &search,
                &certification,
                CertificationWorkers::bounded(workers),
            )
            .unwrap();
            (node, trunk, outer_node)
        };
        let (node, trunk, outer_node) = build(1);
        let (parallel, _, parallel_outer) = build(4);
        assert_eq!(node.fingerprint, parallel.fingerprint);
        assert_eq!(outer_node.fingerprint, parallel_outer.fingerprint);
        assert!(!node.certificate.measurements.is_empty());
        assert!(!outer_node.certificate.measurements.is_empty());
        let mut cut = node.world.clone();
        for block in node.trunks[trunk].owned_blocks() {
            cut.set(
                block.at.x,
                block.at.y,
                block.at.z,
                crate::redstone::world::block::BlockState::air(),
            );
        }
        assert!(matches!(
            certify_root_world(
                &cut,
                &inner,
                &node
                    .root_ports
                    .iter()
                    .map(|root| root.port.clone())
                    .collect::<Vec<_>>(),
                &certification,
                CertificationWorkers::serial()
            ),
            Err(CandidateCertificationError::FunctionalMismatch { .. })
        ));
    }
}
