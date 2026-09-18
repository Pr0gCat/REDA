//! Turns the placed seed geometry into the channel routing plan's cells.
//!
//! The pure decisions (lanes, widths, crossing rows) live in
//! [`super::channel_plan`]; this module maps world anchors into the placement
//! frame, describes every net's presence in every channel, and produces two
//! reservation sets: the channel cells no route may ever use, and the cells
//! each net owns privately.  See
//! `.superpowers/sdd/2026-09-01-topology-aware-seed-v2/channel-routing-design.md`.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use super::candidate::ExpandedPhysicalCandidate;
use super::channel_plan::{
    bounded_turnaround_channel, channel_free_span, lane_forward, lane_forward_from_end,
    plan_channel, ChannelNet, ChannelPlanError, FORWARD_MARGIN, LEGACY_TURNAROUND_CHANNEL,
};
use super::identity::PhysicalEndpointId;
use super::identity::{InstanceId, PortId, RouteId, RoutedSinkId};
use super::placement::{
    horizontal_unit, project_horizontal, DeckId, LateralWindow, PlacementFrame,
    SeedPlacementAnalysis, ROW_GRID,
};
use super::seed::{reserve_route, step, step_many, SourceGeometry, TargetGeometry};
use crate::compile::geometry::Anchor;
use crate::compile::planner::IoFootprint;
use crate::compile::routing::{
    keep_out_typed, NonEmptyRouteSinks, PhysicalReservationKind, PhysicalReservationOwner,
    PhysicalReservations, PhysicalRouter, PlacedBlock, RealisedRouteTree, ReservationStore,
    RouteEndpoint, RouteSink, RouteTarget, RouteTerminalKind, RouterLimits, TerminalContract,
    TerminalRequirement, TransactionalRouteRequest,
};
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::Facing;

/// Cells of a lateral entry line between the terminal and the row the
/// channel reaches it on.  A junction's isolating repeater sits one cell
/// closer to the junction than a torch socket does, so its line is one cell
/// shorter and its row still lands on the grid.
fn entry_depth(geometry: &TargetGeometry) -> i32 {
    match geometry.requirement {
        TerminalRequirement::DirectedDust => 2,
        _ => 3,
    }
}

pub(crate) fn target_approach(geometry: &TargetGeometry) -> Anchor {
    step_many(
        geometry.terminal,
        geometry.allowed_entry,
        entry_depth(geometry),
    )
}

/// One net as the seed routes it: a source and its ordered sinks.
#[derive(Debug, Clone)]
pub(crate) struct NetGeometry {
    pub source: PhysicalEndpointId,
    pub source_geometry: SourceGeometry,
    pub sinks: Vec<(PhysicalEndpointId, TargetGeometry)>,
}

/// One sink as one deck sees it.
///
/// `level` is stated rather than inferred from `endpoint`: a deck plans its
/// own columns, and the forward level a landing belongs to is the caller's
/// fact, not something the layout should re-derive from an endpoint kind.
///
/// `synthetic_trunk` marks an end that is not a real terminal at all -- the
/// cell where a vertical trunk leaves or arrives on this deck. It carries the
/// net's owner as its `endpoint` so diagnostics still name the net, and it is
/// never offered a pinned-port stub, because no caller pinned it.
#[derive(Debug, Clone)]
pub(crate) struct DeckSinkGeometry {
    pub endpoint: PhysicalEndpointId,
    pub geometry: TargetGeometry,
    pub level: i64,
    pub synthetic_trunk: bool,
}

/// One net's presence on one deck: the source it is driven from there and
/// the ordered ends it has to reach there.
#[derive(Debug, Clone)]
pub(crate) struct DeckNetGeometry {
    /// The net's identity, unchanged across every deck it appears on.
    pub owner: PhysicalEndpointId,
    pub source: SourceGeometry,
    pub source_level: i64,
    pub source_is_synthetic_trunk: bool,
    pub sinks: Vec<DeckSinkGeometry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ChannelLayoutError {
    #[error("bounded channel materialization requires {required} cells but the limit is {limit}")]
    MaterializationLimitExceeded { required: u64, limit: u64 },
    #[error("channel {channel} plan failed: {error}")]
    Plan {
        channel: usize,
        error: ChannelPlanError<PhysicalEndpointId>,
    },
    #[error("column {column} crossing failed: {error}")]
    Crossing {
        column: usize,
        error: ChannelPlanError<PhysicalEndpointId>,
    },
    #[error("deck {deck:?} crossing at level {level} failed: {error}")]
    DeckCrossing {
        deck: DeckId,
        level: i64,
        error: ChannelPlanError<PhysicalEndpointId>,
    },
    #[error("deck {deck:?} channel before level {level} failed: {error}")]
    DeckPlan {
        deck: DeckId,
        level: i64,
        error: ChannelPlanError<PhysicalEndpointId>,
    },
    #[error(
        "channel {channel} after level {level} spans {available} forward cells but {lanes} lanes need {needed}"
    )]
    ChannelTooNarrow {
        channel: usize,
        /// Column level before the channel; the input boundary is one below
        /// the first level.
        level: i64,
        available: i32,
        lanes: usize,
        needed: i32,
    },
    #[error("endpoint {endpoint:?} has no column")]
    UnplacedEndpoint { endpoint: PhysicalEndpointId },
    #[error("column escape failed: {0}")]
    Escape(EscapeError),
    #[error("pinned output {endpoint:?} cannot be joined to its column edge")]
    BoxStub { endpoint: PhysicalEndpointId },
    /// The bounded form of [`ChannelLayoutError::ChannelTooNarrow`],
    /// appended after every legacy variant so their order is unchanged.
    /// Only a deck planned under an IO footprint reports it.
    #[error(
        "deck {deck:?} channel {channel} after level {level} spans {available} forward cells but {lanes} lanes need {needed}"
    )]
    DeckChannelTooNarrow {
        deck: DeckId,
        channel: usize,
        /// The analysis's stable global forward level before the channel;
        /// the input boundary is one below the first level.
        level: i64,
        available: i32,
        lanes: usize,
        needed: i32,
    },
    #[error("multiple bounded deck channels are too narrow")]
    DeckChannelsTooNarrow { refusals: Vec<ChannelLayoutError> },
    /// The deferrable form of [`ChannelLayoutError::DeckCrossing`], appended
    /// after every existing variant so their order is unchanged.
    ///
    /// `source` is one net in the refused column's Hall witness that this
    /// deck can still move: a vertical trunk drives it here and it feeds a
    /// pinned output, so landing that trunk's station later takes its source
    /// line out of the column instead of asking the column for another row.
    #[error("deck {deck:?} crossing at level {level} blocks trunk {source:?}: {error}")]
    DeckTrunkCrossing {
        deck: DeckId,
        level: i64,
        /// The net whose trunk may land late, *not* an error cause: the
        /// explicit `#[source]` below is what keeps `thiserror` from reading
        /// this field's name as one.
        source: PhysicalEndpointId,
        #[source]
        error: ChannelPlanError<PhysicalEndpointId>,
    },
    #[error("required route cell {at:?} is shared by {first:?} and {second:?}")]
    RequiredCellShared {
        at: Anchor,
        first: PhysicalEndpointId,
        second: PhysicalEndpointId,
    },
}

fn charge_materialization(used: &mut u64, limit: u64) -> Result<(), ChannelLayoutError> {
    if *used >= limit {
        return Err(ChannelLayoutError::MaterializationLimitExceeded {
            required: used.saturating_add(1),
            limit,
        });
    }
    *used += 1;
    Ok(())
}

/// A closed slab a bounded deck states as bounds rather than as cells: the
/// inclusive axis-aligned box `min..=max` is closed for every route except
/// at `openings`, which are that box's own holes.
///
/// Two regions overlapping is a union of their covers, so one region's
/// opening does not open another's cover.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClosedRegion {
    /// Component-wise lowest corner, inclusive.
    pub min: Anchor,
    /// Component-wise highest corner, inclusive.
    pub max: Anchor,
    /// Cells inside the box that stay open, all of them within it.
    pub openings: BTreeSet<Anchor>,
}

/// The cells the plan hands to the router.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChannelLayout {
    /// Channel cells closed for every route, one entry per cell.  A bounded
    /// deck states its slab in `regions` instead and leaves this empty.
    pub closed: BTreeSet<Anchor>,
    /// Closed slabs stated as bounds.  Ordered by their corners, so merging
    /// decks in any order yields the same set.
    pub regions: BTreeSet<ClosedRegion>,
    /// Cells one net owns; every other route sees them as keep-outs.
    pub private: BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>>,
    /// Lane forward coordinate per net, keyed by the deck and channel it
    /// belongs to, for tests and reports.  Equal channel indices on
    /// different decks are different keys, so merging two decks cannot
    /// silently concatenate them into one another's report.
    pub lanes: BTreeMap<(DeckId, usize), BTreeMap<PhysicalEndpointId, i32>>,
    /// Staircase floors a net's box stub needs, reserved for the net's route
    /// before any route runs so no route conducts through them.
    pub floors: BTreeMap<PhysicalEndpointId, Vec<PlacedBlock>>,
    /// Lane cells a branch of the net is planned to depart from (the lane
    /// cell at every descent and jog row and its two neighbours): the
    /// router places no refresh repeater on them.
    pub departures: BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>>,
}

impl ChannelLayout {
    /// Unions another deck's layout into this one.
    ///
    /// Every per-net collection is additive: one net may own cells on more
    /// than one deck, and its route sees all of them.  The lane report is
    /// not -- one `(deck, channel)` is planned exactly once -- so a
    /// duplicate key is a planning bug rather than something to overwrite.
    pub(crate) fn merge(&mut self, other: ChannelLayout) {
        self.closed.extend(other.closed);
        self.regions.extend(other.regions);
        for (owner, cells) in other.private {
            self.private.entry(owner).or_default().extend(cells);
        }
        for (owner, floors) in other.floors {
            self.floors.entry(owner).or_default().extend(floors);
        }
        for (owner, cells) in other.departures {
            self.departures.entry(owner).or_default().extend(cells);
        }
        for (key, lanes) in other.lanes {
            assert!(
                self.lanes.insert(key, lanes).is_none(),
                "duplicate deck channel",
            );
        }
    }
}

/// How far beyond the placed macros the closed channel layers extend, inside
/// the lateral window.
const LATERAL_MARGIN: i32 = 32;

/// How far a bounded deck's closed shell reaches beyond the geometry it
/// actually materialized: one cell on every side, enough that no route
/// slips around the edge of the plan and no more.  Legacy keeps its
/// `FORWARD_MARGIN` and `LATERAL_MARGIN`.
const CLOSED_SHELL: i32 = 1;
const BOUNDED_ROUTE_CLEARANCE: usize = 4;

pub(crate) fn bounded_route_clearance(mut cells: BTreeSet<Anchor>) -> BTreeSet<Anchor> {
    let mut frontier = cells.clone();
    for _ in 0..BOUNDED_ROUTE_CLEARANCE {
        let mut next = BTreeSet::new();
        for cell in frontier.into_iter().flat_map(keep_out_typed) {
            if cells.insert(cell) {
                next.insert(cell);
            }
        }
        frontier = next;
    }
    cells
}

#[derive(Debug, Clone, Copy)]
struct Frame {
    forward: Facing,
    lateral: Facing,
}

impl Frame {
    fn forward_of(self, at: Anchor) -> i32 {
        project_horizontal(at.x, at.z, self.forward)
    }

    fn lateral_of(self, at: Anchor) -> i32 {
        project_horizontal(at.x, at.z, self.lateral)
    }

    fn along_forward(self, direction: Facing) -> bool {
        direction == self.forward || direction == self.forward.opposite()
    }

    fn cell(self, forward: i32, lateral: i32, y: i32) -> Anchor {
        let (fx, fz) = horizontal_unit(self.forward);
        let (lx, lz) = horizontal_unit(self.lateral);
        Anchor {
            x: fx * forward + lx * lateral,
            y,
            z: fz * forward + lz * lateral,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Column {
    min_forward: i32,
    max_forward: i32,
    /// Laterals a crossing may not use: every occupied lateral and its
    /// neighbours.
    blocked_laterals: BTreeSet<i32>,
}

impl Column {
    fn widen(&mut self, forward: i32, lateral: i32, first: bool) {
        if first {
            self.min_forward = forward;
            self.max_forward = forward;
        } else {
            self.min_forward = self.min_forward.min(forward);
            self.max_forward = self.max_forward.max(forward);
        }
        for near in (lateral - 1)..=(lateral + 1) {
            self.blocked_laterals.insert(near);
        }
    }
}

/// An endpoint's entry line in frame terms.
#[derive(Debug, Clone, Copy)]
struct Line {
    /// Column the endpoint belongs to.
    column: usize,
    /// Channel the line is reached from.
    channel: usize,
    /// Lateral row of the ground line in that channel: the corridor lateral
    /// once escapes are assigned.
    row: i32,
    /// Lateral of the approach cell the corridor must reach.
    natural: i32,
    /// Forward coordinate of the approach cell.
    depth: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum EscapeError {
    #[error("no corridor reaches {endpoint:?} from either side of its column")]
    NoCorridor { endpoint: PhysicalEndpointId },
}

/// The forward level a deck member stands on: `None` when the instance is
/// not this deck's, or when the analysis has no facts for it.
///
/// The single definition of the membership filter, so no caller can drift
/// from the columns the deck actually plans.
fn member_level(
    analysis: &SeedPlacementAnalysis,
    members: &BTreeSet<InstanceId>,
    instance: InstanceId,
) -> Option<i64> {
    members
        .contains(&instance)
        .then(|| analysis.nodes.get(&instance))
        .flatten()
        .map(|facts| facts.forward_level as i64)
}

/// Every cell this deck's members own, each with the forward level of the
/// column it belongs to, in the order the columns are built from.
///
/// The single definition of the walk itself: an instance the analysis does
/// not know, or one that owns no cell, is absent from both the columns and
/// anything derived from them because it is absent here.
fn for_each_member_cell(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    members: &BTreeSet<InstanceId>,
    mut visit: impl FnMut(i64, Anchor),
) {
    for (primitive, placement) in &candidate.placements {
        if let Some(level) = member_level(analysis, members, primitive.instance) {
            for block in &placement.blocks {
                visit(level, block.at);
            }
        }
    }
    for junction in candidate.junctions.values() {
        if let Some(level) = member_level(analysis, members, junction.id) {
            for cell in &junction.cells {
                visit(level, cell.at);
            }
        }
    }
}

/// The lowest and highest forward coordinates occupied by one member level,
/// folded over the same walk the kernel builds its columns from.
///
/// The legacy wrapper needs these before the kernel runs, because a boundary
/// endpoint's level is defined relative to them.
fn member_levels(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    members: &BTreeSet<InstanceId>,
) -> Option<(i64, i64)> {
    let mut bounds: Option<(i64, i64)> = None;
    for_each_member_cell(candidate, analysis, members, |level, _| {
        bounds = Some(match bounds {
            None => (level, level),
            Some((min, max)) => (min.min(level), max.max(level)),
        });
    });
    bounds
}

pub(crate) fn deck_column_edges(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    placement_frame: PlacementFrame,
    members: &BTreeSet<InstanceId>,
    level: i64,
) -> Option<(i32, i32)> {
    let frame = Frame {
        forward: placement_frame.forward,
        lateral: placement_frame.lateral,
    };
    let mut edges: Option<(i32, i32)> = None;
    for_each_member_cell(candidate, analysis, members, |member_level, at| {
        if member_level != level {
            return;
        }
        let forward = frame.forward_of(at);
        edges = Some(match edges {
            None => (forward, forward),
            Some((min, max)) => (min.min(forward), max.max(forward)),
        });
    });
    edges
}

/// The flat, unbounded planner: one synthetic deck covering every instance
/// the analysis knows, the physical boundaries included, and no IO
/// footprint.  Its cells are exactly what this module produced before decks
/// existed.
pub(crate) fn plan_channel_layout(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    placement_frame: PlacementFrame,
    window: LateralWindow,
    nets: &[NetGeometry],
    router: &dyn PhysicalRouter,
    reservations: &mut PhysicalReservations,
    limits: RouterLimits,
) -> Result<ChannelLayout, ChannelLayoutError> {
    let ground = nets
        .iter()
        .map(|net| net.source_geometry.route_anchor.y)
        .min()
        .unwrap_or(1);
    // Every instance the kernel could be asked about: a flat design has one
    // deck, so nothing is filtered out.
    let members = analysis
        .nodes
        .keys()
        .copied()
        .chain(candidate.placements.keys().map(|id| id.instance))
        .chain(candidate.junctions.keys().copied())
        .collect::<BTreeSet<_>>();
    let Some((min_level, max_level)) = member_levels(candidate, analysis, &members) else {
        // The kernel returns the same empty layout, but it would do so only
        // after this wrapper had already resolved levels the columns do not
        // exist to hold.
        return Ok(ChannelLayout::default());
    };
    // The levels this module used to infer from the endpoint kind, resolved
    // once here so the kernel is told them.  A level the analysis does not
    // know still refuses the endpoint by name, in the same net-then-sink
    // order the line pass used to reach it in.
    let level_of_instance = |instance: InstanceId| member_level(analysis, &members, instance);
    let raw_level = |endpoint: PhysicalEndpointId| -> Option<i64> {
        match endpoint {
            PhysicalEndpointId::PrimaryInput(_) => Some(min_level - 1),
            PhysicalEndpointId::DeclaredOutput(_) => Some(max_level + 1),
            PhysicalEndpointId::PrimitiveOutput(primitive) => level_of_instance(primitive.instance),
            PhysicalEndpointId::Junction(instance) => level_of_instance(instance),
            PhysicalEndpointId::Landing(connection) => match connection {
                super::identity::ConnectionId::External { instance, .. }
                | super::identity::ConnectionId::Internal { instance, .. } => {
                    level_of_instance(instance)
                }
            },
        }
    };
    let mut deck_nets = Vec::with_capacity(nets.len());
    for net in nets {
        let source_level = raw_level(net.source).ok_or(ChannelLayoutError::UnplacedEndpoint {
            endpoint: net.source,
        })?;
        let mut sinks = Vec::with_capacity(net.sinks.len());
        for (endpoint, geometry) in &net.sinks {
            sinks.push(DeckSinkGeometry {
                endpoint: *endpoint,
                geometry: *geometry,
                level: raw_level(*endpoint).ok_or(ChannelLayoutError::UnplacedEndpoint {
                    endpoint: *endpoint,
                })?,
                synthetic_trunk: false,
            });
        }
        deck_nets.push(DeckNetGeometry {
            owner: net.source,
            source: net.source_geometry,
            source_level,
            source_is_synthetic_trunk: false,
            sinks,
        });
    }
    let mut materialized = 0;
    plan_deck_channel_layout(
        candidate,
        analysis,
        placement_frame,
        window,
        DeckId(0),
        ground,
        &members,
        true,
        None,
        &deck_nets,
        &BTreeMap::new(),
        &mut materialized,
        router,
        reservations,
        limits,
    )
}

/// Why one column could not seat every crossing demand.
///
/// `demand` is the first demand in the caller's order that no chain of moves
/// could seat, exactly as before.  `witness` is that demand together with
/// every demand holding a row the failed search reached -- a Hall witness:
/// those demands' candidate rows are all the rows there were to try, so no
/// reseating of them alone leaves `demand` a row.  Each of them is therefore
/// a net whose removal from this column would answer the refusal, and the
/// set is a `BTreeSet` of indices into the caller's own order, so which one a
/// caller picks is a function of that order alone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CrossingRefusal {
    demand: usize,
    witness: BTreeSet<usize>,
}

/// Seats one demand, moving the demands already seated if it has to.
///
/// The classic Kuhn augmenting step: the demand takes the first candidate row
/// nobody holds, or else takes one whose holder can move on to a candidate of
/// its own.  `visited` is the rows this search has already tried, so a chain
/// of moves never walks in a circle.
fn augment_crossing_row(
    demand: usize,
    candidates: &[Vec<i32>],
    row_owner: &mut BTreeMap<i32, usize>,
    visited: &mut BTreeSet<i32>,
) -> bool {
    for &row in &candidates[demand] {
        if !visited.insert(row) {
            continue;
        }
        let seated = match row_owner.get(&row).copied() {
            None => true,
            Some(holder) => augment_crossing_row(holder, candidates, row_owner, visited),
        };
        if seated {
            row_owner.insert(row, demand);
            return true;
        }
    }
    false
}

/// Which row every crossing demand takes, as `row -> demand`.
///
/// `candidates[demand]` is that demand's legal rows in the order it prefers
/// them, so the result is a pure function of the caller's own demand order
/// and of those lists.  A greedy pass over the same lists refuses a column
/// whenever an early demand takes the only row a later one could use; this
/// moves the early demand along instead, and only refuses when no chain of
/// moves seats the demand at all.
///
/// The error is the first demand that could not be seated, in the caller's
/// order, so the refusal a column reports never depends on which demand the
/// search happened to reach first, together with the demands jointly
/// responsible for it.
fn match_crossing_rows(candidates: &[Vec<i32>]) -> Result<BTreeMap<i32, usize>, CrossingRefusal> {
    let mut row_owner = BTreeMap::<i32, usize>::new();
    for demand in 0..candidates.len() {
        let mut visited = BTreeSet::new();
        if !augment_crossing_row(demand, candidates, &mut row_owner, &mut visited) {
            // The rows the failed search reached are exactly the candidate
            // rows of `demand` and of everyone it tried to move, and all of
            // them were held: their holders are the Hall witness.
            let mut witness = BTreeSet::from([demand]);
            witness.extend(visited.iter().filter_map(|row| row_owner.get(row).copied()));
            return Err(CrossingRefusal { demand, witness });
        }
    }
    Ok(row_owner)
}

/// One deck's channel layout: the columns its own macros form, the channels
/// between them and the cells every net owns there.
///
/// `deck`/`ground` say which horizontal slab this is and at what row;
/// `members` is the only source of macros, so two decks never see each
/// other's columns even when their local levels are equal.
/// `include_boundaries` is deck zero's privilege -- the caller's physical
/// input and output boundaries stand at one absolute height, so only the
/// deck they stand on plans around them.  `footprint` closes the slab to the
/// caller's board when a complete pin set drew one; `None` is the legacy,
/// open-ended layout.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_deck_channel_layout(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    placement_frame: PlacementFrame,
    window: LateralWindow,
    deck: DeckId,
    ground: i32,
    members: &BTreeSet<InstanceId>,
    include_boundaries: bool,
    footprint: Option<IoFootprint>,
    nets: &[DeckNetGeometry],
    owned: &BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>>,
    materialized: &mut u64,
    router: &dyn PhysicalRouter,
    reservations: &mut PhysicalReservations,
    limits: RouterLimits,
) -> Result<ChannelLayout, ChannelLayoutError> {
    let frame = Frame {
        forward: placement_frame.forward,
        lateral: placement_frame.lateral,
    };

    // ---- columns -------------------------------------------------------
    let level_of_instance = |instance: InstanceId| member_level(analysis, members, instance);
    let mut by_level = BTreeMap::<i64, Column>::new();
    let mut lateral_extent: Option<(i32, i32)> = None;
    let mut occupy = |by_level: &mut BTreeMap<i64, Column>, level: i64, at: Anchor| {
        let forward = frame.forward_of(at);
        let lateral = frame.lateral_of(at);
        let first = !by_level.contains_key(&level);
        by_level
            .entry(level)
            .or_default()
            .widen(forward, lateral, first);
        lateral_extent = Some(match lateral_extent {
            None => (lateral, lateral),
            Some((min, max)) => (min.min(lateral), max.max(lateral)),
        });
    };
    for_each_member_cell(candidate, analysis, members, |level, at| {
        occupy(&mut by_level, level, at);
    });
    let (min_level, max_level) = match (by_level.keys().next(), by_level.keys().last()) {
        (Some(&min), Some(&max)) => (min, max),
        _ => return Ok(ChannelLayout::default()),
    };
    let output_level = max_level + 1;
    if include_boundaries {
        for (endpoint, boundary) in &candidate.boundaries {
            let level = match endpoint {
                PhysicalEndpointId::PrimaryInput(_) => min_level - 1,
                PhysicalEndpointId::DeclaredOutput(_) => output_level,
                _ => continue,
            };
            for block in &boundary.blocks {
                occupy(&mut by_level, level, block.at);
            }
        }
    }
    // Endpoint cells belong to their columns as well, at the level the
    // caller stated for them.
    for net in nets {
        if !net.source_is_synthetic_trunk {
            occupy(&mut by_level, net.source_level, net.source.route_anchor);
        }
        for sink in &net.sinks {
            let geometry = &sink.geometry;
            let level = if matches!(sink.endpoint, PhysicalEndpointId::DeclaredOutput(_)) {
                output_level
            } else {
                sink.level
            };
            if sink.synthetic_trunk {
                continue;
            }
            occupy(&mut by_level, level, geometry.terminal);
            occupy(&mut by_level, level, geometry.support);
            if !frame.along_forward(geometry.allowed_entry) {
                for distance in 1..=(entry_depth(geometry) + 1) {
                    occupy(
                        &mut by_level,
                        level,
                        step_many(geometry.terminal, geometry.allowed_entry, distance),
                    );
                }
            }
        }
    }
    // Pinned inputs and pinned outputs whose forward extents overlap form
    // one pin column: the levels march away from both, and the output nets
    // come back to it through every column in between.
    let mut merged_output_level = None;
    if let (Some(inputs), Some(outputs)) = (
        by_level.get(&(min_level - 1)).cloned(),
        by_level.get(&output_level).cloned(),
    ) {
        if inputs.min_forward <= outputs.max_forward && outputs.min_forward <= inputs.max_forward {
            by_level.remove(&output_level);
            let column = by_level
                .get_mut(&(min_level - 1))
                .expect("the input column was just read");
            column.min_forward = column.min_forward.min(outputs.min_forward);
            column.max_forward = column.max_forward.max(outputs.max_forward);
            column.blocked_laterals.extend(outputs.blocked_laterals);
            merged_output_level = Some(min_level - 1);
        }
    }
    // The caller's level, except for the bounded switchbox/output column when
    // it was folded into the pin column.  Every endpoint stated on that same
    // column follows the alias, including synthetic vertical-trunk endpoints.
    let endpoint_level = |endpoint: PhysicalEndpointId, stated: i64| -> i64 {
        let stated = if matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)) {
            output_level
        } else {
            stated
        };
        merged_output_level
            .filter(|_| stated == output_level)
            .unwrap_or(stated)
    };
    // Columns in forward order: a pinned output column may sit between the
    // inputs and the first level when the levels were placed beyond it.
    let mut ordered = by_level.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(level, column)| (column.min_forward, *level));
    let mut levels = ordered.iter().map(|(level, _)| *level).collect::<Vec<_>>();
    let mut columns = ordered
        .into_iter()
        .map(|(_, column)| column)
        .collect::<Vec<_>>();
    let turnaround_width = |level| match footprint {
        Some(_) => {
            // An outer channel has only one real edge, so each touching net
            // contributes one unsplit piece and therefore at most one lane.
            let lane_bound = nets
                .iter()
                .filter(|net| {
                    endpoint_level(net.owner, net.source_level) == level
                        || net
                            .sinks
                            .iter()
                            .any(|sink| endpoint_level(sink.endpoint, sink.level) == level)
                })
                .count();
            bounded_turnaround_channel(lane_bound)
        }
        None => LEGACY_TURNAROUND_CHANNEL,
    };
    // A bounded upper deck may begin with a synthetic trunk source that
    // leaves backward. Give that first real column the missing channel on
    // its leading side; the legacy unbounded layout remains byte-for-byte
    // unchanged.
    let needs_leading_channel = footprint.is_some()
        && levels.first().is_some_and(|&first_level| {
            nets.iter().any(|net| {
                (endpoint_level(net.owner, net.source_level) == first_level
                    && net.source.allowed_exit == frame.forward.opposite())
                    || net.sinks.iter().any(|sink| {
                        endpoint_level(sink.endpoint, sink.level) == first_level
                            && sink.geometry.allowed_entry == frame.forward.opposite()
                    })
            })
        });
    if needs_leading_channel {
        let first = columns
            .first()
            .cloned()
            .expect("a first level has a column");
        let end = first.min_forward - 1;
        let width = turnaround_width(levels[0]);
        levels.insert(0, levels[0] - 1);
        columns.insert(
            0,
            Column {
                min_forward: end - width,
                max_forward: end - width,
                blocked_laterals: BTreeSet::new(),
            },
        );
    }
    // A virtual empty column beyond the last one gives the last level's
    // sources a channel to leave into; a net whose sinks all lie behind its
    // source climbs onto a lane there, runs to a free crossing row, and
    // comes back through the last column at ground.
    if let Some(last) = columns.last().cloned() {
        let start = last.max_forward + 1;
        let closing = levels.last().copied().unwrap_or(0);
        let width = turnaround_width(closing);
        levels.push(closing + 1);
        columns.push(Column {
            min_forward: start + width,
            max_forward: start + width,
            blocked_laterals: BTreeSet::new(),
        });
    }
    let column_index = |level: i64| levels.iter().position(|&known| known == level);
    let Some((lateral_min, lateral_max)) = lateral_extent else {
        return Ok(ChannelLayout::default());
    };
    // Everything the plan adds beside the macros stays inside the window.
    // The placer measures laterals from the frame origin; this layout
    // measures them from the world origin.
    let origin_lateral = frame.lateral_of(placement_frame.origin);
    let window = LateralWindow {
        min: window.min.map(|min| min + origin_lateral),
        max: window.max.map(|max| max + origin_lateral),
    };
    let (lateral_lo, lateral_hi) =
        window.clamp(lateral_min - LATERAL_MARGIN, lateral_max + LATERAL_MARGIN);
    let channel_count = columns.len().saturating_sub(1);
    let channel_start = |channel: usize| columns[channel].max_forward + 1;
    let channel_end = |channel: usize| columns[channel + 1].min_forward - 1;

    // ---- endpoint lines ------------------------------------------------
    let source_line = |net: &DeckNetGeometry| -> Result<Line, ChannelLayoutError> {
        let level = endpoint_level(net.owner, net.source_level);
        let column = column_index(level).ok_or(ChannelLayoutError::UnplacedEndpoint {
            endpoint: net.owner,
        })?;
        let geometry = net.source;
        let (channel, row) = if net.source_is_synthetic_trunk {
            (
                if frame.forward_of(geometry.route_anchor) < columns[column].min_forward {
                    column.saturating_sub(1)
                } else {
                    column
                },
                frame.lateral_of(geometry.route_anchor),
            )
        } else if geometry.allowed_exit == frame.forward.opposite() {
            (
                column.saturating_sub(1),
                frame.lateral_of(geometry.route_anchor),
            )
        } else if frame.along_forward(geometry.allowed_exit) {
            (column, frame.lateral_of(geometry.route_anchor))
        } else {
            (
                column,
                frame.lateral_of(step_many(geometry.route_anchor, geometry.allowed_exit, 3)),
            )
        };
        Ok(Line {
            column,
            channel: channel.min(channel_count.saturating_sub(1)),
            row,
            natural: row,
            depth: frame.forward_of(if net.source_is_synthetic_trunk {
                step_many(geometry.route_anchor, geometry.allowed_exit, 3)
            } else {
                geometry.route_anchor
            }),
        })
    };
    let sink_line =
        |sink: &DeckSinkGeometry, source_column: usize| -> Result<Line, ChannelLayoutError> {
            let endpoint = sink.endpoint;
            let geometry = &sink.geometry;
            let level = endpoint_level(endpoint, sink.level);
            let column =
                column_index(level).ok_or(ChannelLayoutError::UnplacedEndpoint { endpoint })?;
            let approach = target_approach(geometry);
            let (channel, row) = if geometry.allowed_entry == frame.forward {
                (column, frame.lateral_of(geometry.terminal))
            } else if frame.along_forward(geometry.allowed_entry) {
                (
                    column.saturating_sub(1),
                    frame.lateral_of(geometry.terminal),
                )
            } else {
                let channel = if source_column < column {
                    column.saturating_sub(1)
                } else {
                    column
                };
                (channel, frame.lateral_of(approach))
            };
            Ok(Line {
                column,
                channel: channel.min(channel_count.saturating_sub(1)),
                row,
                natural: row,
                depth: frame.forward_of(approach),
            })
        };

    struct NetLines {
        source: Line,
        sinks: Vec<Line>,
        first_channel: usize,
        last_channel: usize,
    }
    let mut lines = BTreeMap::<PhysicalEndpointId, NetLines>::new();
    for net in nets {
        let source = source_line(net)?;
        let mut sinks = Vec::new();
        for sink in &net.sinks {
            sinks.push(sink_line(sink, source.column)?);
        }
        let first_channel = sinks
            .iter()
            .map(|line| line.channel)
            .chain([source.channel])
            .min()
            .unwrap_or(source.channel);
        let last_channel = sinks
            .iter()
            .map(|line| line.channel)
            .chain([source.channel])
            .max()
            .unwrap_or(source.channel);
        lines.insert(
            net.owner,
            NetLines {
                source,
                sinks,
                first_channel,
                last_channel,
            },
        );
    }

    // ---- pinned port stubs ---------------------------------------------
    // A pinned port faces outward, so its wire enters (or leaves) on the far
    // side of the box the pinned ports form; that box is too tight for
    // planar corridors, and pinned inputs beside each other along the
    // forward axis would all claim the same row.  Every pinned output whose
    // entry lies inside its column, and every pinned input whose exit does
    // not run along the forward axis, is therefore joined to the column edge
    // by the physical router itself, over the component reservations, the
    // stubs already routed and a keep-out ring around every other pinned
    // port's terminal, support and entry cells.  Stubs are routed nearest
    // the channel edge first, so no later stub can wall an earlier port in;
    // each takes the free edge lateral nearest its approach, which becomes
    // its channel row.  The stub floors are reserved for the real route
    // before any route runs, so a route laid under another's bridge or over
    // it finds the same physics in either order.
    struct Stub {
        cells: BTreeSet<Anchor>,
        floors: Vec<PlacedBlock>,
    }
    #[derive(Clone, Copy)]
    struct StubPort {
        net: PhysicalEndpointId,
        /// `None` for the net's source, the sink index otherwise.
        sink: Option<usize>,
        endpoint: PhysicalEndpointId,
        /// Where the stub starts and the direction it leaves in.
        anchor: Anchor,
        exit: Facing,
        /// The block behind the anchor (lamp or handover repeater).
        support: Anchor,
        entry_depth: i32,
        requirement: TerminalRequirement,
        route_from_edge: bool,
        synthetic_trunk: bool,
    }
    // Rows the plan chooses share the parity of the macro row grid, which
    // the placer lays from the frame origin in steps of four.
    let grid_phase = origin_lateral.rem_euclid(2);
    let mut stubs = BTreeMap::<(PhysicalEndpointId, Option<usize>), Stub>::new();
    {
        let edge_of = |line: &Line| -> i32 {
            let column = &columns[line.column];
            if line.channel == line.column {
                column.max_forward
            } else {
                column.min_forward
            }
        };
        let line_of = |lines: &BTreeMap<PhysicalEndpointId, NetLines>, port: &StubPort| -> Line {
            match port.sink {
                Some(index) => lines[&port.net].sinks[index],
                None => lines[&port.net].source,
            }
        };
        let pinned = |endpoint: PhysicalEndpointId| candidate.pin_contracts.contains_key(&endpoint);
        let mut stub_ports = Vec::new();
        for net in nets {
            let source = &net.source;
            if net.source_is_synthetic_trunk
                || (matches!(net.owner, PhysicalEndpointId::PrimaryInput(_))
                    && pinned(net.owner)
                    && source.allowed_exit != frame.forward)
            {
                stub_ports.push(StubPort {
                    net: net.owner,
                    sink: None,
                    endpoint: net.owner,
                    anchor: source.route_anchor,
                    exit: source.allowed_exit,
                    support: step(source.route_anchor, source.allowed_exit.opposite()),
                    entry_depth: 3,
                    requirement: TerminalRequirement::DirectedDust,
                    route_from_edge: net.source_is_synthetic_trunk,
                    synthetic_trunk: net.source_is_synthetic_trunk,
                });
            }
            for (index, sink) in net.sinks.iter().enumerate() {
                let geometry = &sink.geometry;
                let line = lines[&net.owner].sinks[index];
                let entry = frame.forward_of(step(geometry.terminal, geometry.allowed_entry));
                let inside = if line.channel == line.column {
                    entry <= edge_of(&line)
                } else {
                    entry >= edge_of(&line)
                };
                if sink.synthetic_trunk
                    || (matches!(sink.endpoint, PhysicalEndpointId::DeclaredOutput(_))
                        && pinned(sink.endpoint)
                        && inside)
                {
                    stub_ports.push(StubPort {
                        net: net.owner,
                        sink: Some(index),
                        endpoint: sink.endpoint,
                        anchor: geometry.terminal,
                        exit: geometry.allowed_entry,
                        support: if sink.synthetic_trunk {
                            step(geometry.terminal, geometry.allowed_entry.opposite())
                        } else {
                            geometry.support
                        },
                        entry_depth: entry_depth(geometry),
                        requirement: if sink.synthetic_trunk {
                            TerminalRequirement::Exact(RouteTerminalKind::BareMergeDust)
                        } else {
                            geometry.requirement
                        },
                        route_from_edge: true,
                        synthetic_trunk: sink.synthetic_trunk,
                    });
                }
            }
        }
        stub_ports.sort_by_key(|port| {
            let line = line_of(&lines, port);
            let entry = frame.forward_of(step(port.anchor, port.exit));
            (
                (entry - edge_of(&line)).abs(),
                line.natural,
                port.net,
                port.sink,
            )
        });
        let mut committed = Vec::<RealisedRouteTree>::new();
        let mut used_rows = BTreeMap::<(usize, usize), Vec<i32>>::new();
        for (stub_index, port) in stub_ports.iter().enumerate() {
            let line = line_of(&lines, port);
            // The stub's end is only a label for the router; the real
            // terminal is the port's own.
            let label = match port.endpoint {
                PhysicalEndpointId::DeclaredOutput(id) | PhysicalEndpointId::PrimaryInput(id) => id,
                _ => PortId(0),
            };
            let route = RouteId(u32::MAX - u32::try_from(stub_index).unwrap_or(0));
            let own_entry = step(port.anchor, port.exit);
            let mut found = None;
            let merged_column = merged_output_level.and_then(column_index);
            let preferred = if merged_column == Some(line.column) {
                line.column.saturating_sub(1)
            } else {
                line.channel
            };
            let alternate = if preferred == line.column {
                line.column.saturating_sub(1)
            } else {
                line.column
            };
            for side in [preferred, alternate] {
                if side >= channel_count
                    || (side != line.column && side + 1 != line.column)
                    || (side == alternate && alternate == preferred)
                {
                    continue;
                }
                let edge = if side == line.column {
                    columns[line.column].max_forward
                } else {
                    columns[line.column].min_forward
                };
                let (entry_from, support_step) = if side == line.column {
                    (frame.forward, frame.forward.opposite())
                } else {
                    (frame.forward.opposite(), frame.forward)
                };
                let (stub_lateral_lo, stub_lateral_hi) = footprint
                    .filter(|_| !port.synthetic_trunk)
                    .map_or((lateral_lo, lateral_hi), |footprint| {
                        let (_, _, min_lateral, max_lateral) =
                            footprint.projected(frame.forward, frame.lateral);
                        (min_lateral, max_lateral)
                    });
                let mut candidates = (stub_lateral_lo..=stub_lateral_hi)
                    .filter(|c| c.rem_euclid(2) == grid_phase)
                    .filter(|c| {
                        used_rows
                            .get(&(line.column, side))
                            .is_none_or(|rows| rows.iter().all(|row| (row - c).abs() >= 2))
                    })
                    .collect::<Vec<_>>();
                candidates.sort_by_key(|c| {
                    (
                        port.synthetic_trunk
                            && c.rem_euclid(ROW_GRID) == origin_lateral.rem_euclid(ROW_GRID),
                        (c - line.natural).abs(),
                        *c,
                    )
                });
                for c in candidates {
                    let end = frame.cell(edge, c, ground);
                    let before = step(end, entry_from);
                    let support = step(end, support_step);
                    let mut scratch = reservations.transaction();
                    for tree in &committed {
                        reserve_route(&mut scratch, tree, &BTreeSet::new());
                    }
                    for (&other, cells) in owned {
                        if other == port.net {
                            continue;
                        }
                        for &cell in cells {
                            if scratch.get(&cell).is_none() {
                                scratch.reserve(
                                    cell,
                                    PhysicalReservationOwner::Endpoint(other),
                                    PhysicalReservationKind::KeepOut,
                                );
                            }
                        }
                    }
                    // Nothing may run next to another pinned port's anchor or
                    // support, and every other port's entry cells stay free with
                    // the same clearance a laid wire would get.
                    for other in &stub_ports {
                        let own = other.net == port.net && other.sink == port.sink;
                        let mut protect = |cell: Anchor, kind: PhysicalReservationKind| {
                            if scratch.get(&cell).is_none() {
                                scratch.reserve(
                                    cell,
                                    PhysicalReservationOwner::Endpoint(other.endpoint),
                                    kind,
                                );
                            }
                        };
                        for cell in [other.anchor, other.support] {
                            for direction in
                                [Facing::North, Facing::East, Facing::South, Facing::West]
                            {
                                let neighbour = step(cell, direction);
                                if own && neighbour == own_entry {
                                    continue;
                                }
                                protect(neighbour, PhysicalReservationKind::KeepOut);
                            }
                        }
                        if !own {
                            for distance in 1..=other.entry_depth {
                                let entry = step_many(other.anchor, other.exit, distance);
                                protect(
                                    entry,
                                    PhysicalReservationKind::Conductor(crate::compile::dust()),
                                );
                                for direction in
                                    [Facing::North, Facing::East, Facing::South, Facing::West]
                                {
                                    let neighbour = step(entry, direction);
                                    for dy in -1..=1 {
                                        if let Some(y) = neighbour.y.checked_add(dy) {
                                            protect(
                                                Anchor { y, ..neighbour },
                                                PhysicalReservationKind::KeepOut,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if scratch.get(&end).is_some() || scratch.get(&before).is_some() {
                        continue;
                    }
                    let (
                        source_anchor,
                        source_exit,
                        sink_anchor,
                        sink_entry,
                        sink_support,
                        sink_requirement,
                    ) = if port.route_from_edge {
                        (
                            end,
                            entry_from,
                            port.anchor,
                            port.exit,
                            port.support,
                            port.requirement,
                        )
                    } else {
                        (
                            port.anchor,
                            port.exit,
                            end,
                            entry_from,
                            support,
                            TerminalRequirement::Exact(RouteTerminalKind::BareMergeDust),
                        )
                    };
                    let sinks = vec![RouteSink {
                        id: RoutedSinkId { route, ordinal: 0 },
                        endpoint: port.endpoint,
                        anchor: sink_anchor,
                        allowed_entry: sink_entry,
                        terminal: TerminalContract::Sink {
                            target: RouteTarget::DeclaredOutput(label),
                            support: sink_support,
                            requirement: sink_requirement,
                        },
                    }];
                    let Ok(sinks) = NonEmptyRouteSinks::new(sinks) else {
                        continue;
                    };
                    let request = TransactionalRouteRequest {
                        id: route,
                        source: RouteEndpoint {
                            id: port.net,
                            anchor: source_anchor,
                            allowed_exit: source_exit,
                            terminal: TerminalContract::Source {
                                signal_strength: MAX_SIGNAL_STRENGTH,
                            },
                        },
                        sinks: &sinks,
                        reservations: &mut scratch,
                        limits,
                        no_refresh: None,
                        planned_bounds: None,
                        planned_cells: None,
                    };
                    match router.route_transactional(request) {
                        Ok(tree) => {
                            found = Some((c, side, edge, tree));
                            break;
                        }
                        Err(_) => {}
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            let Some((c, channel, edge, tree)) = found else {
                return Err(ChannelLayoutError::BoxStub {
                    endpoint: port.endpoint,
                });
            };
            // The conductors and the clearance above them stay open for the
            // real route; the staircase floors are reserved for it instead,
            // so neither it nor any other route conducts through them.
            let mut cells = BTreeSet::new();
            for block in &tree.cells {
                if block.at == port.anchor {
                    continue;
                }
                cells.insert(block.at);
                cells.insert(Anchor {
                    y: block.at.y + 1,
                    ..block.at
                });
            }
            let floors = tree.floors.clone();
            used_rows.entry((line.column, channel)).or_default().push(c);
            committed.push(tree);
            stubs.insert((port.net, port.sink), Stub { cells, floors });
            let net = lines.get_mut(&port.net).expect("net exists");
            let line = match port.sink {
                Some(index) => &mut net.sinks[index],
                None => &mut net.source,
            };
            line.channel = channel;
            line.row = c;
            line.natural = c;
            line.depth = edge;
        }
    }

    // ---- column escapes ------------------------------------------------
    // Every sink's approach cell is joined to a column edge by a corridor
    // along forward at some lateral, plus a run along lateral at the
    // approach's depth when that lateral is not the approach's own.  Per
    // column and edge the corridors are assigned shallowest first; a deeper
    // corridor never crosses a shallower run, corridors keep two cells
    // apart, and nothing touches a component or another net's line.  The
    // corridor lateral becomes the sink's channel row.
    let mut occupied_cells = BTreeSet::<(i32, i32)>::new();
    for (primitive, placement) in &candidate.placements {
        if level_of_instance(primitive.instance).is_some() {
            for block in &placement.blocks {
                occupied_cells.insert((frame.forward_of(block.at), frame.lateral_of(block.at)));
            }
        }
    }
    for junction in candidate.junctions.values() {
        if !members.contains(&junction.id) {
            continue;
        }
        for cell in &junction.cells {
            occupied_cells.insert((frame.forward_of(cell.at), frame.lateral_of(cell.at)));
        }
    }
    if include_boundaries {
        for boundary in candidate.boundaries.values() {
            for block in &boundary.blocks {
                occupied_cells.insert((frame.forward_of(block.at), frame.lateral_of(block.at)));
            }
        }
    }
    for net in nets {
        occupied_cells.insert((
            frame.forward_of(net.source.route_anchor),
            frame.lateral_of(net.source.route_anchor),
        ));
        // The source's own line from its anchor to the column edge is laid
        // exactly like the private cells below; a corridor or run touching
        // it would join the two nets.
        if let Some(net_lines) = lines.get(&net.owner) {
            let source = &net.source;
            let column = &columns[net_lines.source.column];
            let row = net_lines.source.row;
            let (from, to) = if net.source_is_synthetic_trunk {
                for distance in 1..=3 {
                    let cell = step_many(source.route_anchor, source.allowed_exit, distance);
                    occupied_cells.insert((frame.forward_of(cell), frame.lateral_of(cell)));
                }
                (1, 0)
            } else if frame.along_forward(source.allowed_exit) {
                let anchor = frame.forward_of(source.route_anchor);
                if source.allowed_exit == frame.forward {
                    (anchor + 1, column.max_forward)
                } else {
                    (column.min_forward, anchor - 1)
                }
            } else {
                for distance in 1..=3 {
                    let cell = step_many(source.route_anchor, source.allowed_exit, distance);
                    occupied_cells.insert((frame.forward_of(cell), frame.lateral_of(cell)));
                }
                let approach = step_many(source.route_anchor, source.allowed_exit, 3);
                if net_lines.source.channel >= net_lines.source.column {
                    (frame.forward_of(approach) + 1, column.max_forward)
                } else {
                    (column.min_forward, frame.forward_of(approach) - 1)
                }
            };
            for forward in from..=to {
                occupied_cells.insert((forward, row));
            }
        }
        for sink in &net.sinks {
            let geometry = &sink.geometry;
            occupied_cells.insert((
                frame.forward_of(geometry.terminal),
                frame.lateral_of(geometry.terminal),
            ));
            occupied_cells.insert((
                frame.forward_of(geometry.support),
                frame.lateral_of(geometry.support),
            ));
            for distance in 1..=entry_depth(geometry) {
                let cell = step_many(geometry.terminal, geometry.allowed_entry, distance);
                occupied_cells.insert((frame.forward_of(cell), frame.lateral_of(cell)));
            }
        }
    }
    // Each escaping endpoint's own terminal/support line cells, which its
    // corridor may touch.
    let mut own_line_cells =
        BTreeMap::<(PhysicalEndpointId, Option<usize>), BTreeSet<(i32, i32)>>::new();
    for net in nets {
        if net.source_is_synthetic_trunk {
            let mut own = BTreeSet::new();
            own.insert((
                frame.forward_of(net.source.route_anchor),
                frame.lateral_of(net.source.route_anchor),
            ));
            for distance in 1..=3 {
                let cell = step_many(net.source.route_anchor, net.source.allowed_exit, distance);
                own.insert((frame.forward_of(cell), frame.lateral_of(cell)));
            }
            own_line_cells.insert((net.owner, None), own);
        }
        for (index, sink) in net.sinks.iter().enumerate() {
            let geometry = &sink.geometry;
            let mut own = BTreeSet::new();
            own.insert((
                frame.forward_of(geometry.terminal),
                frame.lateral_of(geometry.terminal),
            ));
            own.insert((
                frame.forward_of(geometry.support),
                frame.lateral_of(geometry.support),
            ));
            for distance in 1..=entry_depth(geometry) {
                let cell = step_many(geometry.terminal, geometry.allowed_entry, distance);
                own.insert((frame.forward_of(cell), frame.lateral_of(cell)));
            }
            own_line_cells.insert((net.owner, Some(index)), own);
        }
    }
    struct Escape {
        corridor: i32,
        depth: i32,
        natural: i32,
        /// Forward coordinate of the column edge the corridor starts from.
        edge: i32,
    }
    let mut escapes = BTreeMap::<(PhysicalEndpointId, Option<usize>), Escape>::new();
    // Laterals an escape's corridor and run occupy, per column: crossing
    // rows through that column keep clear of them, as they do of macros.
    let mut escape_laterals = Vec::<(usize, PhysicalEndpointId, i32, i32)>::new();
    {
        // Endpoints grouped by column, in deterministic depth/row/id order.
        let mut per_column = BTreeMap::<usize, Vec<(PhysicalEndpointId, Option<usize>)>>::new();
        for (&id, net) in &lines {
            if footprint.is_some()
                && nets
                    .iter()
                    .find(|geometry| geometry.owner == id)
                    .is_some_and(|geometry| geometry.source_is_synthetic_trunk)
                && !stubs.contains_key(&(id, None))
            {
                per_column
                    .entry(net.source.column)
                    .or_default()
                    .push((id, None));
            }
            for (index, sink) in net.sinks.iter().enumerate() {
                if stubs.contains_key(&(id, Some(index))) {
                    continue;
                }
                per_column
                    .entry(sink.column)
                    .or_default()
                    .push((id, Some(index)));
            }
        }
        for (column_index, members) in per_column {
            let column = &columns[column_index];
            // (corridor lateral, run extent, depth from the edge) per side
            type PlacedCorridor = (i32, (i32, i32), i32);
            let mut placed: BTreeMap<usize, Vec<PlacedCorridor>> = BTreeMap::new();
            let mut order = members.clone();
            let preferred_side = |line: &Line| -> usize { line.channel };
            let depth_from = |line: &Line, side: usize| -> i32 {
                if side == column_index {
                    (column.max_forward - line.depth).abs()
                } else {
                    (line.depth - column.min_forward).abs()
                }
            };
            order.sort_by_key(|&(id, index)| {
                let line = match index {
                    Some(index) => lines[&id].sinks[index],
                    None => lines[&id].source,
                };
                (
                    depth_from(&line, preferred_side(&line)),
                    line.natural,
                    id,
                    index,
                )
            });
            let mut pending = order;
            let mut retry = Vec::new();
            for pass in 0..2 {
                for &(id, index) in &pending {
                    let line = match index {
                        Some(index) => lines[&id].sinks[index],
                        None => lines[&id].source,
                    };
                    let side = if pass == 0 {
                        preferred_side(&line)
                    } else if preferred_side(&line) == column_index {
                        column_index.saturating_sub(1)
                    } else {
                        column_index
                    };
                    if side >= channel_count || (side != column_index && side + 1 != column_index) {
                        retry.push((id, index));
                        continue;
                    }
                    let edge = if side == column_index {
                        column.max_forward
                    } else {
                        column.min_forward
                    };
                    let depth = depth_from(&line, side);
                    let run_forward = line.depth;
                    let corridor_cells = |c: i32| -> Vec<(i32, i32)> {
                        let (lo, hi) = (edge.min(run_forward), edge.max(run_forward));
                        (lo..=hi).map(|forward| (forward, c)).collect()
                    };
                    let run_cells = |c: i32| -> Vec<(i32, i32)> {
                        let (lo, hi) = (c.min(line.natural), c.max(line.natural));
                        (lo..=hi).map(|lateral| (run_forward, lateral)).collect()
                    };
                    let side_placed = placed.entry(side).or_default();
                    let mut candidates = ((lateral_min - LATERAL_MARGIN)
                        ..=(lateral_max + LATERAL_MARGIN))
                        .filter(|c| c.rem_euclid(2) == grid_phase)
                        .collect::<Vec<_>>();
                    let synthetic_trunk = match index {
                        None => true,
                        Some(index) => nets
                            .iter()
                            .find(|net| net.owner == id)
                            .is_some_and(|net| net.sinks[index].synthetic_trunk),
                    };
                    if synthetic_trunk {
                        candidates.push(line.natural);
                    }
                    candidates.sort_by_key(|c| ((c - line.natural).abs(), *c));
                    candidates.dedup();
                    let chosen = candidates.into_iter().find(|&c| {
                        let extent = (c.min(line.natural), c.max(line.natural));
                        // corridors two apart on this side
                        if side_placed
                            .iter()
                            .any(|(other, _, _)| (other - c).abs() < 2)
                        {
                            return false;
                        }
                        // never cross or touch a shallower or equal run
                        if side_placed.iter().any(|(_, other_extent, other_depth)| {
                            let touches = |lo: i32, hi: i32, x: i32| lo - 1 <= x && x <= hi + 1;
                            (*other_depth <= depth && touches(other_extent.0, other_extent.1, c))
                                || (*other_depth == depth
                                    && (touches(other_extent.0, other_extent.1, extent.0)
                                        || touches(other_extent.0, other_extent.1, extent.1)
                                        || touches(extent.0, extent.1, other_extent.0)))
                        }) {
                            return false;
                        }
                        // nothing orthogonally next to a component or another
                        // net's line; the sink's own terminal, support and line
                        // cells do not count
                        let own_cells = &own_line_cells[&(id, index)];
                        corridor_cells(c)
                            .into_iter()
                            .chain(run_cells(c))
                            .filter(|cell| !own_cells.contains(cell))
                            .all(|(forward, lateral)| {
                                [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)].into_iter().all(
                                    |(df, dl)| {
                                        let cell = (forward + df, lateral + dl);
                                        own_cells.contains(&cell) || !occupied_cells.contains(&cell)
                                    },
                                )
                            })
                    });
                    match chosen {
                        Some(c) => {
                            let extent = (c.min(line.natural), c.max(line.natural));
                            escape_laterals.push((column_index, id, extent.0, extent.1));
                            side_placed.push((c, extent, depth));
                            for cell in corridor_cells(c).into_iter().chain(run_cells(c)) {
                                occupied_cells.insert(cell);
                            }
                            escapes.insert(
                                (id, index),
                                Escape {
                                    corridor: c,
                                    depth: run_forward,
                                    natural: line.natural,
                                    edge,
                                },
                            );
                            let net = lines.get_mut(&id).expect("net exists");
                            let line = match index {
                                Some(index) => &mut net.sinks[index],
                                None => &mut net.source,
                            };
                            line.row = c;
                            line.channel = side;
                        }
                        None => retry.push((id, index)),
                    }
                }
                pending = std::mem::take(&mut retry);
                if pending.is_empty() {
                    break;
                }
            }
            if let Some(&(id, index)) = pending.first() {
                let line = match index {
                    Some(index) => lines[&id].sinks[index],
                    None => lines[&id].source,
                };
                let own = &own_line_cells[&(id, index)];
                let mut conflicts = Vec::new();
                for side in [column_index.saturating_sub(1), column_index] {
                    if side >= channel_count || (side != column_index && side + 1 != column_index) {
                        continue;
                    }
                    let edge = if side == column_index {
                        column.max_forward
                    } else {
                        column.min_forward
                    };
                    let (lo, hi) = (edge.min(line.depth), edge.max(line.depth));
                    for forward in lo..=hi {
                        for (df, dl) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
                            let cell = (forward + df, line.natural + dl);
                            if !own.contains(&cell) && occupied_cells.contains(&cell) {
                                conflicts.push((side, cell));
                            }
                        }
                    }
                }
                let endpoint = index
                    .and_then(|index| {
                        nets.iter()
                            .find(|net| net.owner == id)
                            .and_then(|net| net.sinks.get(index))
                            .map(|sink| sink.endpoint)
                    })
                    .unwrap_or(id);
                return Err(ChannelLayoutError::Escape(EscapeError::NoCorridor {
                    endpoint,
                }));
            }
        }
        for net in lines.values_mut() {
            net.first_channel = net
                .sinks
                .iter()
                .map(|line| line.channel)
                .chain([net.source.channel])
                .min()
                .unwrap_or(net.source.channel);
            net.last_channel = net
                .sinks
                .iter()
                .map(|line| line.channel)
                .chain([net.source.channel])
                .max()
                .unwrap_or(net.source.channel);
        }
    }

    let mut escape_blocked = BTreeMap::<usize, BTreeMap<i32, BTreeSet<PhysicalEndpointId>>>::new();
    for (column_index, id, low, high) in escape_laterals {
        for row in (low - 1)..=(high + 1) {
            escape_blocked
                .entry(column_index)
                .or_default()
                .entry(row)
                .or_default()
                .insert(id);
        }
    }

    // ---- crossings -----------------------------------------------------
    // A net alive in channels c..d crosses every column strictly between.
    let mut crossings = BTreeMap::<(PhysicalEndpointId, usize), i32>::new();
    let (crossing_lo, crossing_hi) = footprint
        .map(|footprint| {
            let (_, _, min, max) = footprint.projected(frame.forward, frame.lateral);
            (min, max)
        })
        .unwrap_or((lateral_lo, lateral_hi));
    for (column, geometry) in columns
        .iter()
        .enumerate()
        .take(columns.len().saturating_sub(1))
        .skip(1)
    {
        let demands = lines
            .iter()
            .filter(|(_, net)| net.first_channel < column && net.last_channel >= column)
            .map(|(&id, net)| {
                let preferred = if footprint.is_some() {
                    net.sinks
                        .iter()
                        .filter(|line| line.channel == column)
                        .map(|line| line.row)
                        .min_by_key(|row| ((row - net.source.row).abs(), *row))
                        .unwrap_or(net.source.row)
                } else {
                    net.source.row
                };
                (id, preferred)
            })
            .collect::<Vec<_>>();
        if demands.is_empty() {
            continue;
        }
        // Crossing rows share the parity of every endpoint row (the grid is
        // four cells, so the rows between grid rows keep two cells from
        // them), and no crossing may sit next to another net's row.
        let free = (crossing_lo..=crossing_hi)
            .filter(|lateral| lateral.rem_euclid(2) == grid_phase)
            .filter(|lateral| !geometry.blocked_laterals.contains(lateral))
            .collect::<BTreeSet<_>>();
        // A crossing row continues as a ground line into both channels
        // beside the column, so it also keeps two cells from every other
        // net's endpoint row there (side sockets sit one cell off the
        // grid); a net's own rows are where it wants to be.
        let neighbour_rows = lines
            .iter()
            .flat_map(|(&id, net)| {
                std::iter::once(&net.source)
                    .chain(net.sinks.iter())
                    .filter(move |line| line.channel + 1 == column || line.channel == column)
                    .map(move |line| (id, line.row))
            })
            .collect::<Vec<_>>();
        let mut ordered = demands.clone();
        ordered.sort_by_key(|(id, preferred)| (*preferred, *id));
        // Every demand's legal rows, in the order it prefers them.  The two
        // conditions are the net's own: a row another net escapes through,
        // and a row beside another net's endpoint line, are the rows this net
        // may not cross on.  Keeping one crossing two cells from another is
        // not among them: every candidate comes from `free`, whose rows all
        // share one parity, so two distinct rows already differ by at least
        // two and the rule is exactly "no two demands on one row" -- which is
        // what a matching enforces by construction.
        let candidates = ordered
            .iter()
            .map(|&(id, preferred)| {
                let mut rows = free
                    .iter()
                    .copied()
                    .filter(|row| {
                        escape_blocked
                            .get(&column)
                            .and_then(|blocked| blocked.get(row))
                            .is_none_or(|owners| owners.iter().all(|owner| *owner == id))
                    })
                    .filter(|row| {
                        neighbour_rows
                            .iter()
                            .all(|(other, used)| *other == id || (used - row).abs() > 1)
                    })
                    .collect::<Vec<_>>();
                rows.sort_by_key(|row| ((row - preferred).abs(), *row));
                rows
            })
            .collect::<Vec<_>>();
        let seated = match_crossing_rows(&candidates).map_err(|refusal| {
            let (id, preferred) = ordered[refusal.demand];
            let error = ChannelPlanError::NoCrossingRow { net: id, preferred };
            // Every net in the witness is one whose absence from this column
            // would seat the rest, so any of them is a place to repair.  Only
            // a net a vertical trunk drives here and that feeds a pinned
            // output can actually leave: its trunk can land its station after
            // this column instead, which is what the deferrable refusal asks
            // the placer for.  The witness is ordered by the caller's own
            // demand order, so the first such net is the same one every time.
            let deferrable = refusal
                .witness
                .iter()
                .map(|&index| ordered[index].0)
                .find(|owner| {
                    nets.iter().any(|net| {
                        net.owner == *owner
                            && net.source_is_synthetic_trunk
                            && net.sinks.iter().any(|sink| {
                                !sink.synthetic_trunk
                                    && matches!(
                                        sink.endpoint,
                                        PhysicalEndpointId::DeclaredOutput(_)
                                    )
                            })
                    })
                });
            match footprint {
                Some(_) => match deferrable {
                    Some(source) => ChannelLayoutError::DeckTrunkCrossing {
                        deck,
                        level: levels[column],
                        source,
                        error,
                    },
                    None => ChannelLayoutError::DeckCrossing {
                        deck,
                        level: levels[column],
                        error,
                    },
                },
                None => ChannelLayoutError::Crossing { column, error },
            }
        })?;
        for (row, demand) in seated {
            crossings.insert((ordered[demand].0, column), row);
        }
    }

    // ---- lanes per channel ---------------------------------------------
    let mut layout = ChannelLayout::default();
    let mut private = |id: PhysicalEndpointId, cell: Anchor| -> Result<(), ChannelLayoutError> {
        if owned.get(&id).is_some_and(|cells| cells.contains(&cell)) {
            return Ok(());
        }
        if footprint.is_some()
            && !layout
                .private
                .get(&id)
                .is_some_and(|cells| cells.contains(&cell))
        {
            charge_materialization(materialized, limits.max_queue_entries)?;
        }
        layout.private.entry(id).or_default().insert(cell);
        Ok(())
    };
    // The cell under a lane cell at a climb or descent row is the riser of
    // that staircase: it is reserved as the net's floor, never opened, so
    // the router cannot conduct through it and strand the lane above.
    let riser = |floors: &mut BTreeMap<PhysicalEndpointId, Vec<PlacedBlock>>,
                 id: PhysicalEndpointId,
                 at: Anchor| {
        floors.entry(id).or_default().push(PlacedBlock {
            at,
            state: crate::compile::stone(),
        });
    };
    for (channel, &level) in levels.iter().enumerate().take(channel_count) {
        let start = channel_start(channel);
        let end = channel_end(channel);
        let mut channel_nets = Vec::new();
        let mut straight_rows = Vec::<i32>::new();
        for (&id, net) in &lines {
            if net.first_channel > channel || net.last_channel < channel {
                continue;
            }
            // Rows are classified by the channel edge their ground line
            // touches, not by endpoint kind: a backward net's sink sits in the
            // column before the channel and its line runs from the start edge
            // exactly like a source line.
            let mut source_rows = Vec::new();
            let mut sink_rows = Vec::new();
            let mut classify = |line: &Line| {
                if line.channel != channel {
                    return;
                }
                if line.column == channel {
                    source_rows.push(line.row);
                } else {
                    sink_rows.push(line.row);
                }
            };
            classify(&net.source);
            for sink in &net.sinks {
                classify(sink);
            }
            if let Some(&row) = crossings.get(&(id, channel)) {
                source_rows.push(row);
            }
            if let Some(&row) = crossings.get(&(id, channel + 1)) {
                sink_rows.push(row);
            }
            let rows = source_rows.iter().chain(sink_rows.iter()).copied();
            let interval = match (rows.clone().min(), rows.max()) {
                (Some(min), Some(max)) => (min, max),
                _ => continue,
            };
            if interval.0 == interval.1 && !source_rows.is_empty() && !sink_rows.is_empty() {
                // A net that enters and leaves this channel on one row is a
                // straight ground line; it needs no lane.
                for forward in start..=end {
                    private(id, frame.cell(forward, interval.0, ground))?;
                }
                straight_rows.push(interval.0);
                continue;
            }
            channel_nets.push(ChannelNet {
                id,
                interval,
                source_rows,
                sink_rows,
            });
        }
        // Jog rows come from the row grid too: a jog's three departure cells
        // straddle its row, so it needs the same four-cell clearance from
        // every other row that endpoint rows have.  Straight lines' rows are
        // excluded as well.
        let grid_phase = origin_lateral.rem_euclid(ROW_GRID);
        // A jog's three departure cells straddle its row, so the row keeps
        // three cells from every other row in the channel (endpoint rows,
        // crossing rows and straight rows alike): the departure cells then
        // never touch another net's ground line.
        let every_row = channel_nets
            .iter()
            .flat_map(|net| net.source_rows.iter().chain(net.sink_rows.iter()).copied())
            .chain(straight_rows.iter().copied())
            .collect::<Vec<_>>();
        let free_jog_rows = (lateral_lo..=lateral_hi)
            .filter(|row| row.rem_euclid(ROW_GRID) == grid_phase)
            .filter(|row| every_row.iter().all(|used| (used - row).abs() >= 3))
            .collect::<BTreeSet<_>>();
        let plan = plan_channel(&channel_nets, &free_jog_rows).map_err(|error| {
            let next_level = levels[channel + 1];
            let split_level = if (min_level..=max_level).contains(&next_level) {
                next_level
            } else {
                levels[channel]
            };
            if footprint.is_some() && split_level > min_level && split_level <= max_level {
                ChannelLayoutError::DeckPlan {
                    deck,
                    level: split_level,
                    error,
                }
            } else {
                ChannelLayoutError::Plan { channel, error }
            }
        })?;
        // Lanes counted from the start edge and lanes counted from the end
        // edge must still keep one lane pitch between the two pools.
        let segment_forward = |segment: &super::channel_plan::Segment| -> i32 {
            if segment.from_end {
                lane_forward_from_end(end, segment.lane)
            } else {
                lane_forward(start, segment.lane)
            }
        };
        let needed = channel_free_span(plan.lane_count);
        if plan.lane_count > 0 && end - start + 1 < needed {
            // Only a deck planned under a footprint can name a deck in its
            // refusal; the legacy path keeps its own variant exactly.
            return Err(match footprint {
                Some(_) => ChannelLayoutError::DeckChannelTooNarrow {
                    deck,
                    channel,
                    level,
                    available: end - start + 1,
                    lanes: plan.lane_count,
                    needed,
                },
                None => ChannelLayoutError::ChannelTooNarrow {
                    channel,
                    level,
                    available: end - start + 1,
                    lanes: plan.lane_count,
                    needed,
                },
            });
        }
        // Stair cells the router checks when a path climbs from ground at
        // `lane - 2` onto the lane, or descends from the lane to ground at
        // `lane + 2` (and their mirror images toward lower forward).
        let climb_from_below = |lane: i32, row: i32| {
            [
                frame.cell(lane - 1, row, ground),
                frame.cell(lane - 2, row, ground + 1),
                frame.cell(lane - 1, row, ground + 1),
                frame.cell(lane - 1, row, ground + 2),
            ]
        };
        let climb_from_above = |lane: i32, row: i32| {
            [
                frame.cell(lane + 1, row, ground),
                frame.cell(lane + 2, row, ground + 1),
                frame.cell(lane + 1, row, ground + 1),
                frame.cell(lane + 1, row, ground + 2),
            ]
        };
        // A descent is also walked upward by a source whose line lies on
        // the end edge, so it carries the riser under the lane cell too.
        let descend_to_above = |lane: i32, row: i32| {
            [
                frame.cell(lane + 1, row, ground + 2),
                frame.cell(lane + 1, row, ground + 1),
                frame.cell(lane + 2, row, ground + 1),
                frame.cell(lane + 1, row, ground),
            ]
        };
        let descend_to_below = |lane: i32, row: i32| {
            [
                frame.cell(lane - 1, row, ground + 2),
                frame.cell(lane - 1, row, ground + 1),
                frame.cell(lane - 2, row, ground + 1),
                frame.cell(lane - 1, row, ground),
            ]
        };
        let channel_rows = channel_nets
            .iter()
            .flat_map(|net| net.source_rows.iter().chain(net.sink_rows.iter()).copied())
            .chain(straight_rows.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut lane_cells = BTreeMap::new();
        for net in &channel_nets {
            let segments = &plan.segments[&net.id];
            // A row climbs onto or leaves the lane of the segment that owns
            // it; only a row no segment names (a straight row) falls back
            // to the lane it lies in.
            let lane_owning =
                |row: i32, owned: fn(&super::channel_plan::Segment) -> &Vec<i32>| -> i32 {
                    let segment = segments
                        .iter()
                        .find(|segment| owned(segment).contains(&row))
                        .or_else(|| {
                            segments.iter().find(|segment| {
                                segment.interval.0 <= row && row <= segment.interval.1
                            })
                        })
                        .unwrap_or(&segments[0]);
                    segment_forward(segment)
                };
            let source_lane_at = |row: i32| lane_owning(row, |segment| &segment.source_rows);
            let sink_lane_at = |row: i32| lane_owning(row, |segment| &segment.sink_rows);
            lane_cells.insert(net.id, segment_forward(&segments[0]));
            for segment in segments {
                let lane = segment_forward(segment);
                for lateral in segment.interval.0..=segment.interval.1 {
                    private(net.id, frame.cell(lane, lateral, ground + 2))?;
                }
            }
            for &row in &net.source_rows {
                let lane = source_lane_at(row);
                for forward in start..=(lane - 2) {
                    private(net.id, frame.cell(forward, row, ground))?;
                }
                // A crossing row on the start edge is a departure from the
                // lane like any sink row, so it gets the same three
                // candidate cells; a real source's climb simply uses the
                // middle one.
                let clear = |candidate: i32| {
                    channel_rows
                        .iter()
                        .all(|other| *other == row || (other - candidate).abs() > 1)
                };
                for departure in [row - 1, row, row + 1] {
                    if departure != row && !clear(departure) {
                        continue;
                    }
                    for cell in climb_from_below(lane, departure) {
                        private(net.id, cell)?;
                    }
                    riser(
                        &mut layout.floors,
                        net.id,
                        frame.cell(lane, departure, ground + 1),
                    );
                    private(net.id, frame.cell(lane - 2, departure, ground))?;
                }
            }
            // A descent may leave the lane on the row itself or one cell to
            // either side and then step onto the row at ground: the trunk's
            // own repeaters (never on two neighbouring cells) can then block
            // at most one of the three departures.
            for &row in &net.sink_rows {
                let lane = sink_lane_at(row);
                for departure in [row - 1, row, row + 1] {
                    layout
                        .departures
                        .entry(net.id)
                        .or_default()
                        .insert(frame.cell(lane, departure, ground + 2));
                }
                for forward in (lane + 2)..=end {
                    private(net.id, frame.cell(forward, row, ground))?;
                }
                // A side departure is only offered when no other row sits
                // within two cells on that side; otherwise its cells would
                // hug that row's ground line.
                let clear = |candidate: i32| {
                    channel_rows
                        .iter()
                        .all(|other| *other == row || (other - candidate).abs() > 1)
                };
                for departure in [row - 1, row, row + 1] {
                    if departure != row && !clear(departure) {
                        continue;
                    }
                    for cell in descend_to_above(lane, departure) {
                        private(net.id, cell)?;
                    }
                    riser(
                        &mut layout.floors,
                        net.id,
                        frame.cell(lane, departure, ground + 1),
                    );
                    private(net.id, frame.cell(lane + 2, departure, ground))?;
                }
            }
            for pair in segments.windows(2) {
                let Some(jog) = pair[0].jog else {
                    continue;
                };
                let from = segment_forward(&pair[0]);
                let to = segment_forward(&pair[1]);
                for departure in [jog - 1, jog, jog + 1] {
                    layout
                        .departures
                        .entry(net.id)
                        .or_default()
                        .insert(frame.cell(from, departure, ground + 2));
                }
                if from < to {
                    for departure in [jog - 1, jog, jog + 1] {
                        for cell in descend_to_above(from, departure) {
                            private(net.id, cell)?;
                        }
                        riser(
                            &mut layout.floors,
                            net.id,
                            frame.cell(from, departure, ground + 1),
                        );
                        private(net.id, frame.cell(from + 2, departure, ground))?;
                    }
                    for forward in (from + 2)..=(to - 2) {
                        private(net.id, frame.cell(forward, jog, ground))?;
                    }
                    for cell in climb_from_below(to, jog) {
                        private(net.id, cell)?;
                    }
                    riser(&mut layout.floors, net.id, frame.cell(to, jog, ground + 1));
                } else {
                    for departure in [jog - 1, jog, jog + 1] {
                        for cell in descend_to_below(from, departure) {
                            private(net.id, cell)?;
                        }
                        riser(
                            &mut layout.floors,
                            net.id,
                            frame.cell(from, departure, ground + 1),
                        );
                        private(net.id, frame.cell(from - 2, departure, ground))?;
                    }
                    for forward in (to + 2)..=(from - 2) {
                        private(net.id, frame.cell(forward, jog, ground))?;
                    }
                    for cell in climb_from_above(to, jog) {
                        private(net.id, cell)?;
                    }
                    riser(&mut layout.floors, net.id, frame.cell(to, jog, ground + 1));
                }
            }
        }
        layout.lanes.insert((deck, channel), lane_cells);
    }

    // ---- lines inside columns and column crossings ---------------------
    for net in nets {
        let Some(net_lines) = lines.get(&net.owner) else {
            continue;
        };
        let source = &net.source;
        let column = &columns[net_lines.source.column];
        if let Some(stub) = stubs.get(&(net.owner, None)) {
            for &cell in &stub.cells {
                private(net.owner, cell)?;
            }
            layout
                .floors
                .entry(net.owner)
                .or_default()
                .extend(stub.floors.iter().cloned());
        } else if let Some(escape) = escapes.get(&(net.owner, None)) {
            for distance in 1..=3 {
                private(
                    net.owner,
                    step_many(source.route_anchor, source.allowed_exit, distance),
                )?;
            }
            let (lo, hi) = (escape.edge.min(escape.depth), escape.edge.max(escape.depth));
            for forward in lo..=hi {
                private(net.owner, frame.cell(forward, escape.corridor, ground))?;
            }
            let (lo, hi) = (
                escape.corridor.min(escape.natural),
                escape.corridor.max(escape.natural),
            );
            for lateral in lo..=hi {
                private(net.owner, frame.cell(escape.depth, lateral, ground))?;
            }
        } else if frame.along_forward(source.allowed_exit) {
            let anchor = frame.forward_of(source.route_anchor);
            let (from, to) = if source.allowed_exit == frame.forward {
                (anchor + 1, column.max_forward)
            } else {
                (column.min_forward, anchor - 1)
            };
            for forward in from..=to {
                private(net.owner, frame.cell(forward, net_lines.source.row, ground))?;
            }
        } else {
            for distance in 1..=3 {
                private(
                    net.owner,
                    step_many(source.route_anchor, source.allowed_exit, distance),
                )?;
            }
            let approach = step_many(source.route_anchor, source.allowed_exit, 3);
            let (from, to) = if net_lines.source.channel >= net_lines.source.column {
                (frame.forward_of(approach) + 1, column.max_forward)
            } else {
                (column.min_forward, frame.forward_of(approach) - 1)
            };
            for forward in from..=to {
                private(net.owner, frame.cell(forward, net_lines.source.row, ground))?;
            }
        }
        for (index, sink) in net.sinks.iter().enumerate() {
            let geometry = &sink.geometry;
            if let Some(stub) = stubs.get(&(net.owner, Some(index))) {
                for &cell in &stub.cells {
                    private(net.owner, cell)?;
                }
                layout
                    .floors
                    .entry(net.owner)
                    .or_default()
                    .extend(stub.floors.iter().cloned());
                continue;
            }
            // The fixed entry line itself.
            for distance in 1..=entry_depth(geometry) {
                private(
                    net.owner,
                    step_many(geometry.terminal, geometry.allowed_entry, distance),
                )?;
            }
            // The corridor from the column edge and the run to the approach.
            if let Some(escape) = escapes.get(&(net.owner, Some(index))) {
                let (lo, hi) = (escape.edge.min(escape.depth), escape.edge.max(escape.depth));
                for forward in lo..=hi {
                    private(net.owner, frame.cell(forward, escape.corridor, ground))?;
                }
                let (lo, hi) = (
                    escape.corridor.min(escape.natural),
                    escape.corridor.max(escape.natural),
                );
                for lateral in lo..=hi {
                    private(net.owner, frame.cell(escape.depth, lateral, ground))?;
                }
            }
        }
        for (column, geometry) in columns
            .iter()
            .enumerate()
            .take(columns.len().saturating_sub(1))
            .skip(1)
        {
            if let Some(&row) = crossings.get(&(net.owner, column)) {
                for forward in geometry.min_forward..=geometry.max_forward {
                    private(net.owner, frame.cell(forward, row, ground))?;
                }
            }
        }
    }

    // ---- closed layers ---------------------------------------------------
    // Everything the plan did not hand to a net is closed: channel cells
    // outside the planned lines and lanes, the layers above the columns, and
    // the gaps between macros.  The router may climb one layer above the
    // lanes, so that layer is closed as well.  Component cells are already
    // reserved and are skipped when the seed applies this set.
    let all_private = layout
        .private
        .values()
        .flat_map(|cells| cells.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut forward_min = columns.first().map_or(0, |column| column.min_forward) - FORWARD_MARGIN;
    let mut forward_max = columns.last().map_or(0, |column| column.max_forward) + FORWARD_MARGIN;
    let mut closed_lateral_min = lateral_min - LATERAL_MARGIN;
    let mut closed_lateral_max = lateral_max + LATERAL_MARGIN;
    // A complete pin set drew a board, and the cells beyond it are not this
    // candidate's to close: the perimeter outside them is a separate
    // reservation, and closing the caller's own ground here would claim
    // rows nobody asked for.
    if let Some(footprint) = footprint {
        let (min_forward, max_forward, min_lateral, max_lateral) =
            footprint.projected(frame.forward, frame.lateral);
        // A bounded deck shells its own plan rather than padding it: the
        // closed box stands one cell beyond the furthest cell this deck
        // materialized -- every column it planned and every private cell it
        // handed out.  A typed trunk corridor on this deck's own four rows
        // widens only the lateral wall it pierces, so it keeps its collar and
        // the deck stops closing the FORWARD_MARGIN and LATERAL_MARGIN
        // cells of ground it never planned in.  An owned cell outside those
        // rows belongs to another deck, so it widens nothing here.  The
        // perimeter and the window still clamp whichever side of the shell
        // falls outside them.
        let deck_rows = ground..=(ground + 3);
        let mut planned_min = columns.first().map_or(0, |column| column.min_forward);
        let mut planned_max = columns.last().map_or(0, |column| column.max_forward);
        let mut shell_lateral_min = lateral_min;
        let mut shell_lateral_max = lateral_max;
        for column in &columns {
            planned_min = planned_min.min(column.min_forward);
            planned_max = planned_max.max(column.max_forward);
        }
        for cell in &all_private {
            let forward = frame.forward_of(*cell);
            let lateral = frame.lateral_of(*cell);
            planned_min = planned_min.min(forward);
            planned_max = planned_max.max(forward);
            shell_lateral_min = shell_lateral_min.min(lateral);
            shell_lateral_max = shell_lateral_max.max(lateral);
        }
        // The vertical shaft is intentionally allowed to stand beyond a
        // deck's forward wall.  Only its lateral coordinate widens this box:
        // that keeps the one-cell collar where the typed corridor pierces
        // the slab without filling the rectangle all the way to the shaft.
        for cell in owned
            .values()
            .flat_map(|cells| cells.iter())
            .filter(|cell| deck_rows.contains(&cell.y))
        {
            let lateral = frame.lateral_of(*cell);
            shell_lateral_min = shell_lateral_min.min(lateral);
            shell_lateral_max = shell_lateral_max.max(lateral);
        }
        forward_min = (planned_min - CLOSED_SHELL).max(min_forward);
        forward_max = (planned_max + CLOSED_SHELL).min(max_forward);
        closed_lateral_min = (shell_lateral_min - CLOSED_SHELL)
            .max(lateral_lo)
            .max(min_lateral);
        closed_lateral_max = (shell_lateral_max + CLOSED_SHELL)
            .min(lateral_hi)
            .min(max_lateral);
    }
    // A bounded deck closes its channel window; typed private corridors own
    // the holes through that wall into the trunk band, and the footprint
    // perimeter closes the board outside it.  Legacy keeps its full margin.
    // Every deck closes its own four rows and no more: the deck below has
    // already closed its ceiling, and the support plane at ground - 1
    // belongs to the macros standing on it.
    let closed_y_min = ground;
    // A bounded deck states its slab as bounds and never enumerates it: the
    // frame rectangle's two opposite corners normalize component-wise into
    // one inclusive axis-aligned box, whichever cardinal direction the frame
    // faces.  The cells this deck did hand out are that box's openings; an
    // explicit reservation needs none, because `PhysicalReservations::get`
    // already gives a reserved cell precedence over a keep-out box.  An
    // inverted range is no box at all, so it states nothing and costs
    // nothing.
    if footprint.is_some() {
        if forward_min <= forward_max
            && closed_lateral_min <= closed_lateral_max
            && closed_y_min <= ground + 3
        {
            let near = frame.cell(forward_min, closed_lateral_min, closed_y_min);
            let far = frame.cell(forward_max, closed_lateral_max, ground + 3);
            let min = Anchor {
                x: near.x.min(far.x),
                y: near.y.min(far.y),
                z: near.z.min(far.z),
            };
            let max = Anchor {
                x: near.x.max(far.x),
                y: near.y.max(far.y),
                z: near.z.max(far.z),
            };
            let inside = |cell: &Anchor| {
                (min.x..=max.x).contains(&cell.x)
                    && (min.y..=max.y).contains(&cell.y)
                    && (min.z..=max.z).contains(&cell.z)
            };
            let route_cells = all_private
                .iter()
                .chain(owned.values().flatten())
                .copied()
                .collect::<BTreeSet<_>>();
            let mut openings: BTreeSet<Anchor> = route_cells
                .iter()
                .copied()
                .filter(inside)
                .collect();
            for halo in bounded_route_clearance(route_cells.clone()) {
                if route_cells.contains(&halo) {
                    continue;
                }
                if inside(&halo) && openings.insert(halo) {
                    charge_materialization(materialized, limits.max_queue_entries)?;
                }
            }
            // One slab, one unit: the cap counts what the deck states, and
            // a compact slab is one statement.
            charge_materialization(materialized, limits.max_queue_entries)?;
            layout.regions.insert(ClosedRegion { min, max, openings });
        }
        return Ok(layout);
    }
    for forward in forward_min..=forward_max {
        for lateral in closed_lateral_min..=closed_lateral_max {
            for y in closed_y_min..=(ground + 3) {
                let cell = frame.cell(forward, lateral, y);
                if !all_private.contains(&cell)
                    && !owned.values().any(|cells| cells.contains(&cell))
                    && reservations.get(&cell).is_none()
                {
                    layout.closed.insert(cell);
                }
            }
        }
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::candidate::{BoundaryPlacement, PrimitivePlacement};
    use crate::compile::fragment_synth::identity::{
        InstanceId, PortId, PrimitiveId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::instance_graph::InstanceGraph;
    use crate::compile::fragment_synth::placement::{DeckId, NodeFacts};
    use crate::compile::geometry::CellFacing;
    use crate::compile::planner::PortPlacements;
    use crate::compile::routing::GuardedPhysicalRouter;

    /// Deck zero's ground in these fixtures, and the ground deck_grounds
    /// derives for deck one from two (-1, 3) local intervals: one one-cell
    /// macro with its support below and the channel slab above.
    const LOWER_GROUND: i32 = 1;
    const UPPER_GROUND: i32 = 6;

    fn frame() -> PlacementFrame {
        PlacementFrame {
            forward: Facing::East,
            lateral: Facing::South,
            origin: Anchor {
                x: 0,
                y: LOWER_GROUND,
                z: 0,
            },
        }
    }

    fn limits() -> RouterLimits {
        RouterLimits {
            max_node_expansions: 262_144,
            max_queue_entries: 262_144,
        }
    }

    fn empty_graph() -> InstanceGraph {
        InstanceGraph {
            instances: Vec::new(),
            assignments: Vec::new(),
            primary_inputs: Vec::new(),
            declared_outputs: Vec::new(),
            blocks: Vec::new(),
        }
    }

    /// A candidate reduced to what a column is made of: one dust cell per
    /// instance, at the deck that instance stands on.
    fn one_cell_macros(cells: &[(InstanceId, Anchor)]) -> ExpandedPhysicalCandidate {
        let mut candidate =
            ExpandedPhysicalCandidate::empty(empty_graph(), PortPlacements::default());
        for &(instance, at) in cells {
            let id = PrimitiveId {
                instance,
                node: TopologyNodeId(0),
            };
            candidate.placements.insert(
                id,
                PrimitivePlacement {
                    id,
                    variant: 0,
                    facing: CellFacing::EAST,
                    anchor: at,
                    delayed: None,
                    blocks: vec![PlacedBlock {
                        at,
                        state: crate::compile::dust(),
                    }],
                },
            );
        }
        candidate
    }

    /// Every instance on the same forward level, so two decks' macros carry
    /// equal local levels -- which is exactly the aliasing risk.
    fn flat_analysis(instances: &[InstanceId]) -> SeedPlacementAnalysis {
        SeedPlacementAnalysis {
            order: instances.to_vec(),
            nodes: instances
                .iter()
                .map(|&instance| {
                    (
                        instance,
                        NodeFacts {
                            predecessors: Vec::new(),
                            successors: Vec::new(),
                            forward_level: 0,
                            reverse_level: 0,
                            head_ticks: 0,
                            tail_ticks: 0,
                            deck: DeckId(0),
                        },
                    )
                })
                .collect(),
            edges: Vec::new(),
            critical_delay_ticks: 0,
        }
    }

    /// The closed slab one lone macro draws, by hand: its own column at
    /// forward at.x, the virtual turnaround column one cell beyond it plus
    /// LEGACY_TURNAROUND_CHANNEL, FORWARD_MARGIN past both ends,
    /// LATERAL_MARGIN beside the single occupied lateral, and the four rows
    /// ground..=ground + 3.
    fn expected_slab(ground: i32, at: Anchor) -> BTreeSet<Anchor> {
        let forward_min = at.x - FORWARD_MARGIN;
        let forward_max = at.x + 1 + LEGACY_TURNAROUND_CHANNEL + FORWARD_MARGIN;
        let mut cells = BTreeSet::new();
        for x in forward_min..=forward_max {
            for z in (at.z - LATERAL_MARGIN)..=(at.z + LATERAL_MARGIN) {
                for y in ground..=(ground + 3) {
                    cells.insert(Anchor { x, y, z });
                }
            }
        }
        cells
    }

    /// The one slab a bounded deck states, asserting on the way that it
    /// stated bounds rather than cells.
    fn only_region(layout: &ChannelLayout) -> &ClosedRegion {
        assert!(
            layout.closed.is_empty(),
            "a bounded deck enumerates no closed cell",
        );
        assert_eq!(layout.regions.len(), 1, "a bounded deck states one slab");
        layout.regions.iter().next().expect("one slab")
    }

    /// Whether the slab blocks `at`: inside its box and not one of its own
    /// openings, the same cover [`PhysicalReservations::get`] applies.
    fn closes(region: &ClosedRegion, at: Anchor) -> bool {
        (region.min.x..=region.max.x).contains(&at.x)
            && (region.min.y..=region.max.y).contains(&at.y)
            && (region.min.z..=region.max.z).contains(&at.z)
            && !region.openings.contains(&at)
    }

    #[test]
    fn legacy_channel_layout_wrapper_keeps_the_existing_layout() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let mut reservations = PhysicalReservations::new();

        let layout = plan_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            &[],
            &GuardedPhysicalRouter,
            &mut reservations,
            limits(),
        )
        .expect("one macro and no nets plan");

        assert_eq!(layout.closed, expected_slab(LOWER_GROUND, at));
        assert!(layout.private.is_empty());
        assert!(layout.floors.is_empty());
        assert!(layout.departures.is_empty());
        assert_eq!(
            layout.lanes,
            BTreeMap::from([((DeckId(0), 0), BTreeMap::new())]),
        );
    }

    #[test]
    fn equal_levels_on_two_decks_never_alias_channel_state() {
        let lower_at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let upper_at = Anchor {
            x: 4,
            y: UPPER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), lower_at), (InstanceId(1), upper_at)]);
        let analysis = flat_analysis(&[InstanceId(0), InstanceId(1)]);
        let mut reservations = PhysicalReservations::new();

        let lower = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &BTreeSet::from([InstanceId(0)]),
            true,
            None,
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut reservations,
            limits(),
        )
        .expect("deck zero plans");
        let upper = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(1),
            UPPER_GROUND,
            &BTreeSet::from([InstanceId(1)]),
            false,
            None,
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut reservations,
            limits(),
        )
        .expect("deck one plans");

        assert!(lower.closed.iter().all(|at| at.y <= LOWER_GROUND + 3));
        assert!(upper.closed.iter().all(|at| at.y >= UPPER_GROUND));
        assert!(lower.closed.is_disjoint(&upper.closed));
        assert!(lower.lanes.contains_key(&(DeckId(0), 0)));
        assert!(upper.lanes.contains_key(&(DeckId(1), 0)));

        // Each deck saw only its own member, so neither slab is the union
        // of the two macros' columns.
        assert_eq!(lower.closed, expected_slab(LOWER_GROUND, lower_at));
        assert_eq!(upper.closed, expected_slab(UPPER_GROUND, upper_at));
    }

    #[test]
    fn bounded_footprint_clamps_the_closed_slab() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: 4,
            max_x: 12,
            min_z: 0,
            max_z: 9,
        };
        let window = LateralWindow {
            min: Some(4),
            max: Some(4),
        };
        let members = BTreeSet::from([InstanceId(0)]);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            at,
            PhysicalReservationOwner::KeepOut(7),
            PhysicalReservationKind::KeepOut,
        );

        let error = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            window,
            DeckId(0),
            LOWER_GROUND,
            &members,
            true,
            Some(footprint),
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            RouterLimits {
                max_queue_entries: 0,
                ..limits()
            },
        )
        .expect_err("bounded slab materialization obeys the existing queue cap");
        assert_eq!(
            error,
            ChannelLayoutError::MaterializationLimitExceeded {
                required: 1,
                limit: 0,
            }
        );

        let mut materialized = 0;
        let bounded = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            window,
            DeckId(0),
            LOWER_GROUND,
            &members,
            true,
            Some(footprint),
            &[],
            &BTreeMap::new(),
            &mut materialized,
            &GuardedPhysicalRouter,
            &mut reservations,
            limits(),
        )
        .expect("the bounded deck plans");

        assert_eq!(
            only_region(&bounded),
            &ClosedRegion {
                min: Anchor {
                    x: footprint.min_x,
                    y: LOWER_GROUND,
                    z: window.min.unwrap(),
                },
                max: Anchor {
                    x: footprint.max_x,
                    y: LOWER_GROUND + 3,
                    z: window.max.unwrap(),
                },
                openings: BTreeSet::new(),
            },
            "the slab is the clamped box, with no opening for a cell it did              not hand out",
        );
        assert_eq!(materialized, 1, "one slab costs one unit");
        // The macro's own cell stays inside the box: it is reserved
        // explicitly, and `PhysicalReservations::get` gives that reservation
        // precedence over any keep-out box covering it.
        assert!(closes(only_region(&bounded), at));
        assert!(reservations
            .get(&at)
            .is_some_and(|claim| claim.owner == PhysicalReservationOwner::KeepOut(7)));

        let unbounded = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &members,
            true,
            None,
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut reservations,
            limits(),
        )
        .expect("the unbounded deck plans");
        assert!(unbounded
            .closed
            .iter()
            .any(|cell| !footprint.contains_xz(*cell)));
    }

    /// The compact form is the bounded form only: a bounded deck states one
    /// slab and enumerates nothing, while the legacy deck keeps enumerating
    /// its cells and states no slab.  Both cover the same wall -- the cell
    /// just outside the macro's own column is closed either way.
    #[test]
    fn a_bounded_deck_states_one_slab_where_legacy_enumerates_cells() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let members = BTreeSet::from([InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: -100,
            max_x: 100,
            min_z: -100,
            max_z: 100,
        };
        let plan = |footprint, materialized: &mut u64| {
            plan_deck_channel_layout(
                &candidate,
                &analysis,
                frame(),
                LateralWindow::default(),
                DeckId(0),
                LOWER_GROUND,
                &members,
                true,
                footprint,
                &[],
                &BTreeMap::new(),
                materialized,
                &GuardedPhysicalRouter,
                &mut PhysicalReservations::new(),
                limits(),
            )
            .expect("the deck plans")
        };

        let mut materialized = 0;
        let bounded = plan(Some(footprint), &mut materialized);
        let region = only_region(&bounded);
        assert_eq!(materialized, 1, "the whole slab costs one unit");
        assert!(closes(region, Anchor { z: at.z - 1, ..at }));

        let legacy = plan(None, &mut materialized);
        assert!(legacy.regions.is_empty(), "a legacy deck states no slab");
        assert!(
            legacy.closed.contains(&Anchor { z: at.z - 1, ..at }),
            "a legacy deck still enumerates its closed cells",
        );
        assert_eq!(
            materialized, 1,
            "a legacy deck charges nothing for its closed cells",
        );
    }

    #[test]
    fn bounded_upper_deck_closes_only_its_own_slab() {
        let at = Anchor {
            x: 4,
            y: UPPER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: 0,
            max_x: 20,
            min_z: 0,
            max_z: 9,
        };
        let window = LateralWindow {
            min: Some(2),
            max: Some(7),
        };

        let upper = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            window,
            DeckId(1),
            UPPER_GROUND,
            &BTreeSet::from([InstanceId(0)]),
            false,
            Some(footprint),
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the bounded upper deck plans");

        // The slab a deck closes is its own four rows and nothing below
        // them: the deck beneath already closed its own ceiling, and the
        // support plane at ground - 1 belongs to the macros standing on it.
        let region = only_region(&upper);
        assert_eq!(
            region.min.y, UPPER_GROUND,
            "a bounded upper deck closes no cell below its own ground",
        );
        assert_eq!(region.max.y, UPPER_GROUND + 3);
        assert!(
            region.openings.iter().all(|cell| cell.y >= UPPER_GROUND),
            "every opening lies inside the slab it opens",
        );
    }

    #[test]
    fn bounded_empty_deck_uses_a_lane_sized_trailing_turnaround() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: 0,
            max_x: 100,
            min_z: 0,
            max_z: 9,
        };

        let layout = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &BTreeSet::from([InstanceId(0)]),
            true,
            Some(footprint),
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the bounded empty deck plans");

        assert_eq!(
            only_region(&layout).max.x,
            at.x + 1 + bounded_turnaround_channel(0) + CLOSED_SHELL,
        );
    }

    /// A bounded deck walls its slab instead of padding it: the closed set
    /// reaches exactly one forward cell past the furthest cell the deck
    /// materialized -- every column it planned and every private cell it
    /// handed out -- rather than FORWARD_MARGIN cells past it.  This lone
    /// macro plans its own column at `at.x` and the virtual lane-sized
    /// turnaround column beyond it and hands out no private cell, so those
    /// two columns are the whole plan; the footprint is wide enough that
    /// the perimeter clamp never binds.
    #[test]
    fn bounded_closed_slab_walls_the_plan_by_one_forward_cell() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: -100,
            max_x: 100,
            min_z: -100,
            max_z: 100,
        };

        let layout = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &BTreeSet::from([InstanceId(0)]),
            true,
            Some(footprint),
            &[],
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the bounded deck plans");

        let planned = layout
            .private
            .values()
            .flat_map(|cells| cells.iter().map(|cell| cell.x))
            .chain([at.x, at.x + 1 + bounded_turnaround_channel(0)])
            .collect::<Vec<_>>();
        let planned_min = *planned.iter().min().expect("the deck planned a column");
        let planned_max = *planned.iter().max().expect("the deck planned a column");
        let region = only_region(&layout);
        assert_eq!(
            region.min.x,
            planned_min - 1,
            "the leading wall is one cell, not FORWARD_MARGIN",
        );
        assert_eq!(
            region.max.x,
            planned_max + 1,
            "the trailing wall is one cell, not FORWARD_MARGIN",
        );
    }

    /// The bounded shell is a rectangle: it stands one cell beyond the
    /// lateral extent this deck materialized as well -- the laterals its
    /// macros occupy, the private cells it handed out and the owned cells a
    /// typed trunk corridor left on this deck's own four rows -- so every
    /// corridor keeps a one-cell collar.  An owned cell below those rows
    /// belongs to another deck and widens nothing.  The window and the
    /// footprint are wide enough here that neither clamp binds.
    #[test]
    fn bounded_deck_shells_its_plan_laterally() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: -100,
            max_x: 100,
            min_z: -100,
            max_z: 100,
        };
        // One owned corridor cell on this deck's rows, and one far beside it
        // on the support plane the deck below owns.
        let owner = PhysicalEndpointId::PrimaryInput(PortId(0));
        let collar = Anchor {
            x: at.x + 30,
            y: LOWER_GROUND + 2,
            z: 12,
        };
        let below = Anchor {
            x: at.x,
            y: LOWER_GROUND - 1,
            z: 30,
        };
        let owned = BTreeMap::from([(owner, BTreeSet::from([collar, below]))]);

        let layout = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &BTreeSet::from([InstanceId(0)]),
            true,
            Some(footprint),
            &[],
            &owned,
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the bounded deck plans");

        let region = only_region(&layout);
        assert_eq!(
            region.min.z,
            at.z - CLOSED_SHELL,
            "the near shell is one cell, not LATERAL_MARGIN",
        );
        assert_eq!(
            region.max.z,
            collar.z + CLOSED_SHELL,
            "the corridor keeps its collar and the plane below widens nothing",
        );
        assert_eq!(
            region.max.x,
            at.x + 1 + bounded_turnaround_channel(0) + CLOSED_SHELL,
            "the shaft may stand beyond the deck without filling up to it",
        );
        assert!(
            !closes(region, collar),
            "an owned corridor cell stays open inside its own collar",
        );
        assert!(
            region.openings.is_empty(),
            "neither owned cell lies inside the slab, so the slab has no hole",
        );
    }

    #[test]
    fn bounded_deck_opens_the_typed_halo_of_owned_cells() {
        let at = Anchor {
            x: 4,
            y: LOWER_GROUND,
            z: 4,
        };
        let candidate = one_cell_macros(&[(InstanceId(0), at)]);
        let analysis = flat_analysis(&[InstanceId(0)]);
        let footprint = IoFootprint {
            min_x: -100,
            max_x: 100,
            min_z: -100,
            max_z: 100,
        };
        let owner = PhysicalEndpointId::PrimaryInput(PortId(0));
        let lower = Anchor {
            x: at.x + 1,
            y: LOWER_GROUND,
            z: at.z,
        };
        let upper = Anchor {
            x: at.x + 2,
            y: LOWER_GROUND + 1,
            z: at.z,
        };
        let owned = BTreeMap::from([(owner, BTreeSet::from([lower, upper]))]);

        let layout = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &BTreeSet::from([InstanceId(0)]),
            true,
            Some(footprint),
            &[],
            &owned,
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the bounded deck plans");

        let region = only_region(&layout);
        let side = Anchor {
            z: lower.z + 1,
            ..lower
        };
        assert!(
            !closes(region, side),
            "an owned cell keeps its planned typed halo open at {side:?}",
        );
        for clearance in [Anchor { y: lower.y, ..upper }, Anchor { y: upper.y, ..lower }] {
            assert!(
                !closes(region, clearance),
                "an owned stair keeps its physical clearance open at {clearance:?}",
            );
        }
    }

    #[test]
    fn bounded_route_clearance_is_exactly_four_cells_wide() {
        let origin = Anchor { x: 4, y: 8, z: 12 };
        let clearance = bounded_route_clearance(BTreeSet::from([origin]));

        assert!(clearance.contains(&Anchor { x: 8, ..origin }));
        assert!(!clearance.contains(&Anchor { x: 9, ..origin }));
    }

    #[test]
    fn bounded_first_column_backward_source_gets_a_leading_channel() {
        let instance = InstanceId(0);
        let owner = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance,
            node: TopologyNodeId(0),
        });
        let opening = Anchor {
            x: 14,
            y: UPPER_GROUND,
            z: 8,
        };
        let candidate = one_cell_macros(&[(instance, opening)]);
        let analysis = flat_analysis(&[instance]);
        let nets = [DeckNetGeometry {
            owner,
            source: SourceGeometry {
                route_anchor: Anchor { z: 18, ..opening },
                allowed_exit: Facing::West,
            },
            source_level: 0,
            source_is_synthetic_trunk: true,
            sinks: vec![DeckSinkGeometry {
                endpoint: owner,
                geometry: TargetGeometry {
                    terminal: opening,
                    allowed_entry: Facing::East,
                    support: Anchor {
                        y: UPPER_GROUND - 1,
                        ..opening
                    },
                    requirement: TerminalRequirement::DirectedDust,
                },
                level: 0,
                synthetic_trunk: false,
            }],
        }];
        let trunk_cell = Anchor {
            x: 11,
            y: UPPER_GROUND - 1,
            z: 18,
        };
        let owned = BTreeMap::from([(owner, BTreeSet::from([trunk_cell]))]);

        let layout = plan_deck_channel_layout(
            &candidate,
            &analysis,
            frame(),
            LateralWindow::default(),
            DeckId(1),
            UPPER_GROUND,
            &BTreeSet::from([instance]),
            false,
            Some(IoFootprint {
                min_x: 0,
                max_x: 40,
                min_z: 0,
                max_z: 30,
            }),
            &nets,
            &owned,
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the upper deck plans");

        let lane = layout.lanes[&(DeckId(1), 0)][&owner];
        assert!(lane < opening.x);
        assert!((0..=40).contains(&lane));
        // The slab is this deck's own four rows; the support plane at
        // ground - 1 is the trunk's and the macros', not a keep-out.
        let region = only_region(&layout);
        assert_eq!(region.min.y, UPPER_GROUND);
        assert!(!closes(region, trunk_cell));
    }

    #[test]
    fn merged_output_column_keeps_a_synthetic_trunk_endpoint() {
        let instance = InstanceId(0);
        let owner = PhysicalEndpointId::PrimaryInput(PortId(1));
        let macro_at = Anchor {
            x: 14,
            y: LOWER_GROUND,
            z: 8,
        };
        let mut candidate = one_cell_macros(&[(instance, macro_at)]);
        for endpoint in [
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            PhysicalEndpointId::DeclaredOutput(PortId(0)),
        ] {
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed: None,
                    blocks: vec![PlacedBlock {
                        at: Anchor { x: 4, ..macro_at },
                        state: crate::compile::dust(),
                    }],
                },
            );
        }
        let nets = [DeckNetGeometry {
            owner,
            source: SourceGeometry {
                route_anchor: Anchor {
                    x: 4,
                    z: 18,
                    ..macro_at
                },
                allowed_exit: Facing::West,
            },
            source_level: 1,
            source_is_synthetic_trunk: true,
            sinks: vec![DeckSinkGeometry {
                endpoint: owner,
                geometry: TargetGeometry {
                    terminal: macro_at,
                    allowed_entry: Facing::East,
                    support: Anchor {
                        y: LOWER_GROUND - 1,
                        ..macro_at
                    },
                    requirement: TerminalRequirement::DirectedDust,
                },
                level: 0,
                synthetic_trunk: false,
            }],
        }];

        let layout = plan_deck_channel_layout(
            &candidate,
            &flat_analysis(&[instance]),
            frame(),
            LateralWindow::default(),
            DeckId(0),
            LOWER_GROUND,
            &BTreeSet::from([instance]),
            true,
            Some(IoFootprint {
                min_x: 0,
                max_x: 40,
                min_z: 0,
                max_z: 30,
            }),
            &nets,
            &BTreeMap::new(),
            &mut 0,
            &GuardedPhysicalRouter,
            &mut PhysicalReservations::new(),
            limits(),
        )
        .expect("the merged column keeps the synthetic trunk endpoint");
        assert!(layout.lanes[&(DeckId(0), 0)].contains_key(&owner));
    }

    /// Two demands over the same rows, where the first one's first choice is
    /// the second one's only choice.  A greedy pass seats demand 0 on row 0
    /// and then has nothing left for demand 1; the augmenting path moves
    /// demand 0 on to row 2 and seats both.
    #[test]
    fn crossing_rows_seat_by_augmenting_path() {
        let candidates = vec![vec![0, 2], vec![0]];
        let seated = match_crossing_rows(&candidates).expect("both demands seat");
        assert_eq!(seated, BTreeMap::from([(0, 1), (2, 0)]));
        // The same lists always seat the same way.
        assert_eq!(match_crossing_rows(&candidates), Ok(seated));

        // No chain of moves seats demand 1, and demand 1 is what the refusal
        // names: the first demand in the caller's order that cannot sit.
        assert_eq!(
            match_crossing_rows(&[vec![0], vec![0], vec![]]),
            Err(CrossingRefusal {
                demand: 1,
                witness: BTreeSet::from([0, 1]),
            }),
        );
    }

    /// The witness is the demands jointly responsible, not just the one that
    /// refused: demand 1 has only row 0, and demand 0 is sitting on it with
    /// nowhere else to go, so seating demand 1 needs one of *those two* to
    /// leave the column.  Demand 2 is never reached -- it has a row of its
    /// own and the refusal is raised before its turn -- so it is not named,
    /// and a caller repairing the witness never moves a net that is not part
    /// of the problem.
    #[test]
    fn a_crossing_refusal_names_every_demand_that_caused_it() {
        let refusal = match_crossing_rows(&[vec![0], vec![0], vec![2]])
            .expect_err("demand 1 has only a row demand 0 cannot leave");

        assert_eq!(
            refusal,
            CrossingRefusal {
                demand: 1,
                witness: BTreeSet::from([0, 1]),
            },
        );
    }

    #[test]
    fn merging_deck_layouts_unions_every_owner_and_keys_lanes_by_deck() {
        let net = PhysicalEndpointId::PrimaryInput(PortId(0));
        let cell = |y: i32| Anchor { x: 1, y, z: 2 };
        let deck_layout = |deck: DeckId, y: i32| ChannelLayout {
            closed: BTreeSet::from([cell(y)]),
            regions: BTreeSet::from([ClosedRegion {
                min: cell(y),
                max: cell(y + 3),
                openings: BTreeSet::from([cell(y + 1)]),
            }]),
            private: BTreeMap::from([(net, BTreeSet::from([cell(y + 1)]))]),
            lanes: BTreeMap::from([((deck, 0), BTreeMap::from([(net, y)]))]),
            floors: BTreeMap::from([(
                net,
                vec![PlacedBlock {
                    at: cell(y - 1),
                    state: crate::compile::stone(),
                }],
            )]),
            departures: BTreeMap::from([(net, BTreeSet::from([cell(y + 2)]))]),
        };

        let mut merged = deck_layout(DeckId(0), 1);
        merged.merge(deck_layout(DeckId(1), 6));

        assert_eq!(merged.closed, BTreeSet::from([cell(1), cell(6)]));
        assert_eq!(
            merged
                .regions
                .iter()
                .map(|region| (region.min, region.max))
                .collect::<Vec<_>>(),
            vec![(cell(1), cell(4)), (cell(6), cell(9))],
            "both decks' slabs survive the merge, ordered by their corners",
        );
        assert_eq!(
            merged.private[&net],
            BTreeSet::from([cell(2), cell(7)]),
            "one net owns cells on both decks",
        );
        assert_eq!(merged.departures[&net], BTreeSet::from([cell(3), cell(8)]));
        assert_eq!(
            merged.floors[&net]
                .iter()
                .map(|block| block.at)
                .collect::<Vec<_>>(),
            vec![cell(0), cell(5)],
        );
        assert_eq!(
            merged.lanes.keys().copied().collect::<Vec<_>>(),
            vec![(DeckId(0), 0), (DeckId(1), 0)],
        );
    }

    #[test]
    #[should_panic(expected = "duplicate deck channel")]
    fn merging_the_same_deck_channel_twice_is_a_planning_bug() {
        let lanes = |deck: DeckId| ChannelLayout {
            lanes: BTreeMap::from([((deck, 0), BTreeMap::new())]),
            ..ChannelLayout::default()
        };
        let mut merged = lanes(DeckId(0));
        merged.merge(lanes(DeckId(0)));
    }
}
