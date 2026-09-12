//! Pure topology analysis for topology-aware seed placement.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::channel_plan::{
    bounded_turnaround_allowance, channel_width, lane_count, legacy_turnaround_allowance,
};
use crate::compile::fragment_synth::identity::{ConnectionId, InstanceId, PrimitiveId};
use crate::compile::fragment_synth::identity::{PhysicalEndpointId, PortId};
use crate::compile::fragment_synth::instance_graph::{
    Instance, InstanceDriver, InstanceGraph, LogicalSignalId, PhysicalDriver, PhysicalSink,
};
use crate::compile::fragment_synth::topology::{
    ConnectionSource, ConnectionTarget, ContributorSpec, OutputSpec, ValidatedTopology,
};
use crate::compile::geometry::{Anchor, CellFacing};
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::physical::PortKind;
use crate::compile::planner::{IoFootprint, PortPin, PortRole};
use crate::compile::topology::Primitive;
use crate::compile::topology::{EmbeddingHint, TemplateNode};
use crate::compile::{geometry, physical};
use crate::redstone::simulator::component::{
    COMPARATOR_DELAY_GAME_TICKS, REPEATER_GAME_TICKS_PER_REDSTONE_TICK, TORCH_DELAY_GAME_TICKS,
};
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::Facing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct NodeFacts {
    pub predecessors: Vec<InstanceId>,
    pub successors: Vec<InstanceId>,
    pub forward_level: u64,
    pub reverse_level: u64,
    pub head_ticks: u64,
    pub tail_ticks: u64,
    /// The horizontal deck this macro stands on.  Analysis alone cannot
    /// know it -- it falls out of the board's own forward capacity -- so
    /// every node starts on the base deck and only the placer moves it.
    /// `forward_level` is deliberately untouched by that move: a column
    /// carried up a deck keeps the level it computes its channels from.
    pub deck: DeckId,
}

/// Which horizontal deck a macro stands on, counted up from the base deck
/// the frame origin sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct DeckId(pub u32);

/// One deck's ground row and the absolute vertical interval it reserves:
/// the macros' own cells, their supports below, and the channel slab and
/// router ceiling above.  `min_y`/`max_y` are world rows, not offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeckPlan {
    pub ground: i32,
    pub min_y: i32,
    pub max_y: i32,
}

/// What the plan says about the volume it asks for, before any route is
/// paid for.  `macro_volume / union_volume` is the planned macro fill the
/// density gate compares -- as an exact pair, never as a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct FloorplanMetrics {
    /// Sum of the macro envelopes' own volumes.
    pub macro_volume: u64,
    /// Volume of the bounding box of those envelopes where the plan puts
    /// them, in forward/lateral/Y.
    pub union_volume: u64,
    pub deck_count: u32,
    /// Nets that leave the deck they are driven from.  Zero until the
    /// vertical trunks that carry them are allocated.
    pub cross_deck_nets: u32,
    /// Vertical-trunk lanes reserved for those nets.  Zero until the trunk
    /// band exists.
    pub vertical_trunk_lanes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EdgeFacts {
    pub source: InstanceId,
    pub sink: InstanceId,
    pub structural_slack_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SeedPlacementAnalysis {
    pub order: Vec<InstanceId>,
    pub nodes: BTreeMap<InstanceId, NodeFacts>,
    pub edges: Vec<EdgeFacts>,
    pub critical_delay_ticks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum SeedPlacementError {
    #[error("instance identity {instance:?} appears more than once")]
    DuplicateInstance { instance: InstanceId },
    #[error("dependency names missing instance {instance:?}")]
    UnknownInstance { instance: InstanceId },
    #[error("instance dependency graph contains a cycle among {instances:?}")]
    DependencyCycle { instances: Vec<InstanceId> },
    #[error("selected topology for {instance:?} contains an unresolved primitive dependency")]
    UnresolvedTopology { instance: InstanceId },
    #[error("structural timing delay overflowed u64 game ticks")]
    TimingOverflow,
    #[error("placement coordinate overflowed i32")]
    CoordinateOverflow,
    #[error("primitive {primitive:?} has no physical variant")]
    MissingPhysicalVariant { primitive: Primitive },
    #[error("seed placer does not support layout repairs")]
    UnsupportedRepairs,
    #[error(
        "no placement frame keeps the levels inside the pins' inward half-spaces and the world"
    )]
    NoFrameFits,
    #[error("macro {instance:?} is wider than the lateral window the pins leave")]
    LateralWindowTooNarrow { instance: InstanceId },
    #[error("layout repair cannot move pinned or missing owner {owner:?}")]
    ImmovableRepairOwner { owner: LayoutOwner },
    #[error("layout repair cannot separate the same owner {owner:?}")]
    SameRepairOwner { owner: LayoutOwner },
    /// A complete pin set whose cells all share one X, or all share one Z,
    /// draws a rectangle with no interior.  That is not a zero-width window
    /// to squeeze a layout into -- it is a board the caller cannot have
    /// meant, and the placer says so rather than refusing every cell of it.
    #[error("the complete IO pin set draws a footprint with no interior: {footprint:?}")]
    DegenerateIoFootprint { footprint: IoFootprint },
    /// The board leaves no forward span to stand a deck in at all, so no
    /// ordering of the columns over decks can be tried.
    #[error("no ordered deck layout fits the IO footprint")]
    NoDeckLayoutFits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlacementFrame {
    pub forward: Facing,
    pub lateral: Facing,
    pub origin: Anchor,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SeedPlacementRequest<'a> {
    pub graph: &'a InstanceGraph,
    pub analysis: &'a SeedPlacementAnalysis,
    pub pins: &'a BTreeMap<PhysicalEndpointId, PortPin>,
    pub block_facts: &'a BTreeMap<InstanceId, BlockFacts>,
}

/// Per-block facts the placer needs, supplied by the seed: the block's
/// fixed east-facing footprint and its certified delay.  A block is an
/// opaque box -- it has no topology for the placer to measure, so these
/// facts (and `block_delays` passed to [`analyse_instance_dag`]) replace
/// what `macro_envelope`/`topology_delay_ticks` derive for an ordinary
/// instance.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockFacts {
    /// `max.x - min.x + 1` of the block's certified layout.
    pub width: i32,
    /// `max.z - min.z + 1` of the block's certified layout.
    pub depth: i32,
    /// `max.y - min.y + 1` of the block's certified layout: the deck a
    /// block stands on has to leave room for its floors and its roof too.
    pub height: i32,
    pub delay_ticks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum LayoutOwner {
    Boundary(PhysicalEndpointId),
    Instance(InstanceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum LayoutRepair {
    ExclusiveGuardedTrack {
        source: PhysicalEndpointId,
    },
    EarlyTreeSinkAndEscape {
        source: PhysicalEndpointId,
        sink: PhysicalEndpointId,
    },
    SeparateOwners {
        source_owner: LayoutOwner,
        sink_owner: LayoutOwner,
    },
    /// The channel after `level` (the input channel for `min_level - 1`)
    /// must offer at least `width` free forward cells: the channel routing
    /// plan found more lanes than the placer's estimate allowed for.
    WidenChannel {
        level: i64,
        width: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreferredInstancePose {
    pub preferred_origin: Anchor,
    pub facing: CellFacing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeedPlacementPlan {
    pub instances: BTreeMap<InstanceId, PreferredInstancePose>,
    pub automatic_inputs: BTreeMap<PortId, Anchor>,
    pub automatic_outputs: BTreeMap<PortId, Anchor>,
    pub fingerprint: Fingerprint,
    /// The frame every later stage (sockets, channel plan) measures in.
    pub frame: PlacementFrame,
    /// The analysis every later stage measures levels in: a level whose
    /// macros do not fit the lateral window side by side is folded into
    /// consecutive columns, and the levels after it move up.
    pub analysis: SeedPlacementAnalysis,
    /// Lateral bounds every block must respect.
    pub window: LateralWindow,
    /// The rectangle a complete pin set draws, and `None` for every partial
    /// or unpinned set.  `Some` is what closed `window` on both sides and
    /// bounded the forward extent; `None` leaves both exactly as they were
    /// before the caller's board had a boundary.
    pub io_footprint: Option<IoFootprint>,
    /// Every deck the columns were packed onto, lowest first.  An
    /// unbounded or partially pinned plan keeps all of its columns on
    /// `DeckId(0)`, so this is one entry and the plan is the flat one it
    /// always was.
    pub decks: BTreeMap<DeckId, DeckPlan>,
    /// What the plan asks of the board before any route is paid for: the
    /// density gate's two exact volumes and the deck/trunk counts.
    pub floorplan: FloorplanMetrics,
}

/// Lateral bounds, in frame coordinates, that every block of the layout
/// must respect: the origin-based world edge, and the inward half-space of
/// every pinned input whose signal enters along the lateral axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub(crate) struct LateralWindow {
    pub min: Option<i32>,
    pub max: Option<i32>,
}

impl LateralWindow {
    /// `lo..=hi` cut down to the window.
    pub(crate) fn clamp(self, lo: i32, hi: i32) -> (i32, i32) {
        (
            self.min.map_or(lo, |min| lo.max(min)),
            self.max.map_or(hi, |max| hi.min(max)),
        )
    }

    fn width(self) -> Option<i32> {
        match (self.min, self.max) {
            (Some(min), Some(max)) => Some(max - min + 1),
            _ => None,
        }
    }
}

/// Cells kept free inside the lateral window on each side, for the closed
/// layers, escape corridors and stairs the channel plan puts beside the
/// outermost macros.
pub(crate) const WINDOW_MARGIN: i32 = 8;
/// Forward cells the channel plan needs beyond the last column: the
/// turnaround channel and the closed margin.
const TURNAROUND_ALLOWANCE: i32 = legacy_turnaround_allowance();

pub(crate) trait SeedPlacer {
    fn plan(
        &self,
        request: SeedPlacementRequest<'_>,
    ) -> Result<SeedPlacementPlan, SeedPlacementError>;

    fn plan_with_repairs(
        &self,
        request: SeedPlacementRequest<'_>,
        repairs: &[LayoutRepair],
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        if repairs.is_empty() {
            self.plan(request)
        } else {
            Err(SeedPlacementError::UnsupportedRepairs)
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TopologyAwareSeedPlacer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NetInterval {
    signal: LogicalSignalId,
    start: u64,
    end: u64,
    fanout: usize,
    slack: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MacroBounds {
    min_forward: i32,
    max_forward: i32,
    min_lateral: i32,
    max_lateral: i32,
    /// Local Y bounds, relative to the deck ground the macro stands on.  A
    /// turn is about Y, so these are the envelope's own bounds whatever the
    /// facing and whatever way the frame points.
    min_y: i32,
    max_y: i32,
}

impl MacroBounds {
    fn forward_span(self) -> i32 {
        self.max_forward - self.min_forward + 1
    }

    fn lateral_span(self) -> i32 {
        self.max_lateral - self.min_lateral + 1
    }

    fn height(self) -> i32 {
        self.max_y - self.min_y + 1
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HorizontalBounds {
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MacroEnvelope {
    by_facing: [HorizontalBounds; 4],
    /// How far the macro reaches below and above its own ground row: a
    /// repeater's support is one row down, a torch's support shares its
    /// row.  A facing turns about Y, so one pair covers all four.
    min_y: i32,
    max_y: i32,
}

impl MacroEnvelope {
    fn oriented_bounds(self, facing: CellFacing, frame_forward: Facing) -> MacroBounds {
        let bounds = self.by_facing[usize::from(facing.index())];
        let frame_lateral = clockwise(frame_forward);
        let mut forward = Vec::with_capacity(4);
        let mut lateral = Vec::with_capacity(4);
        for (x, z) in [
            (bounds.min_x, bounds.min_z),
            (bounds.min_x, bounds.max_z),
            (bounds.max_x, bounds.min_z),
            (bounds.max_x, bounds.max_z),
        ] {
            forward.push(project_horizontal(x, z, frame_forward));
            lateral.push(project_horizontal(x, z, frame_lateral));
        }
        MacroBounds {
            min_forward: *forward.iter().min().expect("four corners"),
            max_forward: *forward.iter().max().expect("four corners"),
            min_lateral: *lateral.iter().min().expect("four corners"),
            max_lateral: *lateral.iter().max().expect("four corners"),
            min_y: self.min_y,
            max_y: self.max_y,
        }
    }
}

/// Nominal channel depth used only to score instance facings before the
/// real per-level channel widths are known.
const ROUTING_CHANNEL: i32 = 13;
/// Lateral margin added around every endpoint when estimating the trunk
/// interval of a net: a torch's north or south socket line ends four cells
/// from the support.
const ENDPOINT_ROW_MARGIN: i32 = 4;
/// Forward cells per channel taken by the source anchor and the sink
/// terminal that sit just outside their macro envelopes.
const ENDPOINT_CELLS_PER_CHANNEL: i32 = 2;
/// Free cells between neighbouring macros in the same level.  Two torches
/// stacked in one level may both use the sockets that face each other; each
/// of those entries is three cells deep, and the two entry lines must not
/// touch, so facing terminals need eight cells between them.
const LATERAL_GAP: i32 = 10;
/// Lateral pitch of net tracks; a multiple of the row grid so automatic
/// boundaries land on grid rows.
const TRACK_PITCH: i32 = 8;
/// Every endpoint row (source anchor, torch socket approach, junction input)
/// lies on this lateral grid relative to the frame origin.  Rows of
/// different nets can then never be adjacent, and a dogleg always finds a
/// free row between two grid rows.
pub(crate) const ROW_GRID: i32 = 4;

impl TopologyAwareSeedPlacer {
    /// The plan with explicit minimum channel widths (keyed like
    /// `derive_channel_widths`) layered over the estimated ones.
    fn plan_with_widths(
        &self,
        request: SeedPlacementRequest<'_>,
        minimum_widths: &BTreeMap<i64, i32>,
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        // The frame points from the inputs toward the outputs.  When the
        // levels with their channels do not fit ahead of the pins there (a
        // pinned output line too close, or the world edge), the frame turns
        // a quarter: the levels then march along the pin line, both pin
        // groups form one pin column at the start, and every level is folded
        // to the lateral room the pins' inward half-space leaves.  The
        // levels are never placed behind a pinned input.
        //
        // A complete pin set draws the board's own rectangle first: it is the
        // same in every candidate frame, so it is measured -- and refused
        // when it has no interior -- once, before any frame is tried.
        let footprint = io_footprint(request);
        if let Some(footprint) = footprint {
            if footprint.min_x == footprint.max_x || footprint.min_z == footprint.max_z {
                return Err(SeedPlacementError::DegenerateIoFootprint { footprint });
            }
        }
        let direct = derive_frame(request.pins);
        let turned = |forward: Facing| PlacementFrame {
            forward,
            lateral: clockwise(forward),
            origin: direct.origin,
        };
        let candidates = [
            direct,
            turned(clockwise(direct.forward)),
            turned(clockwise(clockwise(clockwise(direct.forward)))),
        ];
        for (index, frame) in candidates.into_iter().enumerate() {
            let plan = self.plan_in_frame(request, minimum_widths, frame, index == 0, footprint)?;
            if let Some(plan) = plan {
                return Ok(plan);
            }
        }
        Err(SeedPlacementError::NoFrameFits)
    }

    /// The plan in one frame, or `None` when the levels do not fit ahead of
    /// the pins.  In the direct frame pinned outputs ahead of the inputs form
    /// their own column the levels must stop short of; in a turned frame
    /// every pinned port lies beside the pin line and the levels start
    /// beyond all of them.
    fn plan_in_frame(
        &self,
        request: SeedPlacementRequest<'_>,
        minimum_widths: &BTreeMap<i64, i32>,
        frame: PlacementFrame,
        direct: bool,
        footprint: Option<IoFootprint>,
    ) -> Result<Option<SeedPlacementPlan>, SeedPlacementError> {
        let analysis = request.analysis;
        let intervals = net_intervals(request.graph, analysis);
        let tracks = colour_intervals(&intervals);
        let track_laterals = track_laterals(request.graph, request.pins, frame, &tracks);
        let mut envelopes = request
            .graph
            .instances
            .iter()
            .map(|instance| macro_envelope(instance).map(|size| (instance.id, size)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        for block in &request.graph.blocks {
            // The caller derives `block_facts` and the analysis's block
            // delays from the same walk, so a block present here without
            // facts is the same internal inconsistency
            // `analyse_instance_dag` already names this way -- a refusal,
            // not a panic.
            let facts = *request
                .block_facts
                .get(&block.id)
                .ok_or(SeedPlacementError::UnresolvedTopology { instance: block.id })?;
            envelopes.insert(block.id, block_envelope(facts));
        }

        let mut lanes = initial_lanes(request.graph, &track_laterals, analysis);
        barycentric_sweep(analysis, &mut lanes, true);
        barycentric_sweep(analysis, &mut lanes, false);
        let channel = ROUTING_CHANNEL;

        let mut facings = BTreeMap::new();
        for instance in &request.graph.instances {
            let lateral = lanes[&instance.id];
            let max_span = [
                CellFacing::NORTH,
                CellFacing::EAST,
                CellFacing::SOUTH,
                CellFacing::WEST,
            ]
            .into_iter()
            .map(|facing| {
                envelopes[&instance.id]
                    .oriented_bounds(facing, frame.forward)
                    .forward_span()
            })
            .max()
            .unwrap_or(1);
            let origin = frame_to_world(frame, 0, lateral);
            let source = frame_to_world(frame, -channel, lateral);
            let target = frame_to_world(frame, max_span + channel, lateral);
            facings.insert(
                instance.id,
                choose_instance_facing(instance, origin, source, target, frame.forward)?,
            );
        }
        // A block is an opaque, already-certified layout: it keeps its
        // fixed east-facing orientation, and neither `choose_instance_facing`
        // nor `macro_output_direction` apply -- both read `expanded.topology`,
        // which a block does not have.
        for block in &request.graph.blocks {
            facings.insert(block.id, CellFacing::EAST);
        }

        let bounds = request
            .graph
            .instances
            .iter()
            .map(|instance| instance.id)
            .chain(request.graph.blocks.iter().map(|block| block.id))
            .map(|id| {
                (
                    id,
                    envelopes[&id].oriented_bounds(facings[&id], frame.forward),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut level_bounds = BTreeMap::<u64, MacroBounds>::new();
        for (&id, &instance_bounds) in &bounds {
            let level = analysis.nodes[&id].forward_level;
            level_bounds
                .entry(level)
                .and_modify(|level_bounds| {
                    level_bounds.min_forward =
                        level_bounds.min_forward.min(instance_bounds.min_forward);
                    level_bounds.max_forward =
                        level_bounds.max_forward.max(instance_bounds.max_forward);
                })
                .or_insert(instance_bounds);
        }
        // Lateral positions first: they do not depend on the column spacing,
        // and the channel widths depend on them.
        let mut laterals = BTreeMap::<InstanceId, i32>::new();
        for &level in level_bounds.keys() {
            let mut ids = analysis
                .order
                .iter()
                .copied()
                .filter(|id| analysis.nodes[id].forward_level == level)
                .collect::<Vec<_>>();
            ids.sort_by_key(|id| (lanes[id], *id));
            let entries = ids
                .iter()
                .copied()
                .map(|id| (id, lanes[&id], bounds[&id]))
                .collect::<Vec<_>>();
            let legalized = legalize_laterals(&entries)?;
            for id in ids {
                laterals.insert(id, legalized[&id]);
            }
        }

        // ---- fold levels to the lateral window ----------------------------
        // A level whose macros do not fit the window side by side is cut,
        // in lateral order, into groups that do; each group becomes its own
        // column and the levels after it move up.  Every group is moved to
        // the window's first free lateral, on the row grid.
        let window = lateral_window(frame, request.pins, footprint);
        let budget = window
            .width()
            .map(|width| width - 2 * WINDOW_MARGIN - ROW_GRID);
        let mut folded = analysis.clone();
        if let Some(budget) = budget {
            if budget < 1 {
                return Ok(None);
            }
            let mut shift = 0u64;
            let original_levels = level_bounds.keys().copied().collect::<Vec<_>>();
            for level in original_levels {
                let mut ids = laterals
                    .keys()
                    .copied()
                    .filter(|id| analysis.nodes[id].forward_level == level)
                    .collect::<Vec<_>>();
                ids.sort_by_key(|id| (laterals[id], *id));
                let mut groups: Vec<(i32, Vec<InstanceId>)> = Vec::new();
                for id in ids {
                    let lo = laterals[&id] + bounds[&id].min_lateral;
                    let hi = laterals[&id] + bounds[&id].max_lateral;
                    if hi - lo >= budget {
                        return Err(SeedPlacementError::LateralWindowTooNarrow { instance: id });
                    }
                    match groups.last_mut() {
                        Some((start, members)) if hi - *start < budget => members.push(id),
                        _ => groups.push((lo, vec![id])),
                    }
                }
                // Every group starts at the window's first free lateral, so
                // the columns share one lateral extent that fits the window.
                let base = window.min.map_or_else(
                    || groups.first().map_or(0, |(start, _)| *start),
                    |min| min + WINDOW_MARGIN,
                );
                for (sub, (start, members)) in groups.iter().enumerate() {
                    let delta = -((start - base).div_euclid(ROW_GRID)) * ROW_GRID;
                    for &id in members {
                        folded
                            .nodes
                            .get_mut(&id)
                            .expect("folded analysis covers every instance")
                            .forward_level = level + shift + sub as u64;
                        if delta != 0 {
                            let lateral = laterals
                                .get_mut(&id)
                                .expect("legalized laterals cover every instance");
                            *lateral = lateral
                                .checked_add(delta)
                                .ok_or(SeedPlacementError::CoordinateOverflow)?;
                        }
                    }
                }
                shift += groups.len() as u64 - 1;
            }
        }
        let analysis = &folded;
        let mut level_bounds = BTreeMap::<u64, MacroBounds>::new();
        for (&id, &instance_bounds) in &bounds {
            let level = analysis.nodes[&id].forward_level;
            level_bounds
                .entry(level)
                .and_modify(|level_bounds| {
                    level_bounds.min_forward =
                        level_bounds.min_forward.min(instance_bounds.min_forward);
                    level_bounds.max_forward =
                        level_bounds.max_forward.max(instance_bounds.max_forward);
                })
                .or_insert(instance_bounds);
        }
        // Automatic ports share the shift: they sit on the free tracks the
        // macros were placed around.
        // A declared input no gate reads has no net, so no track: it still
        // gets a lever (`automatic_input_ports` below is every primary
        // input, unfiltered by consumers), and it stands on lateral 0 like
        // any other trackless signal -- the same fallback `track_lateral`
        // and `automatic_output_lateral` already use. Indexing here instead
        // panicked on an unused port, which is ordinary hardware
        // description.
        let automatic_input_lateral = |port: PortId| -> i32 {
            track_laterals
                .get(&LogicalSignalId::PrimaryInput(port))
                .copied()
                .unwrap_or(0)
        };
        let automatic_output_lateral = |port: PortId| -> i32 {
            request
                .graph
                .assignments
                .iter()
                .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                .and_then(|assignment| track_laterals.get(&assignment.signal).copied())
                .unwrap_or(0)
        };
        let automatic_input_ports = request
            .graph
            .primary_inputs
            .iter()
            .copied()
            .filter(|port| {
                !request
                    .pins
                    .contains_key(&PhysicalEndpointId::PrimaryInput(*port))
            })
            .collect::<Vec<_>>();
        let automatic_output_ports = request
            .graph
            .declared_outputs
            .iter()
            .copied()
            .filter(|port| {
                !request
                    .pins
                    .contains_key(&PhysicalEndpointId::DeclaredOutput(*port))
            })
            .collect::<Vec<_>>();
        let lateral_shift = if request.pins.is_empty() {
            0
        } else {
            let mut port_laterals = automatic_input_ports
                .iter()
                .map(|&port| automatic_input_lateral(port))
                .chain(
                    automatic_output_ports
                        .iter()
                        .map(|&port| automatic_output_lateral(port)),
                )
                .collect::<Vec<_>>();
            // Folded levels start at the window's first free lateral, so the
            // automatic ports' tracks start there as well.
            let rebase = match (window.min, port_laterals.iter().min()) {
                (Some(min), Some(&lowest)) if budget.is_some() => min + WINDOW_MARGIN - lowest,
                _ => 0,
            };
            for lateral in &mut port_laterals {
                *lateral += rebase;
            }
            rebase + confine_laterals(window, &bounds, &port_laterals, &mut laterals)?
        };

        let (mut channels, channel_lanes) =
            derive_channel_widths(request, analysis, &level_bounds, &laterals, &track_laterals);
        // The channel beyond the last column is the one whose trunks the
        // turnaround carries; `channel_lanes` is keyed by channel level, so
        // that is its last entry.
        let last_lanes = channel_lanes.values().next_back().copied().unwrap_or(0);
        for (&level, &width) in minimum_widths {
            channels
                .entry(level)
                .and_modify(|known| *known = (*known).max(width))
                .or_insert(width);
        }
        let min_level = level_bounds.keys().next().copied().unwrap_or(0);
        let input_channel = channels
            .get(&(min_level as i64 - 1))
            .copied()
            .unwrap_or_else(|| channel_width(1));

        // The first level starts one input channel beyond the pin column:
        // the pinned inputs' cells and every pinned output cell that does
        // not lie ahead of them.  Automatic inputs are placed one input
        // channel behind the origin instead.
        let pinned_inputs = request
            .pins
            .keys()
            .any(|endpoint| matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)));
        let mut cursor = if pinned_inputs {
            pin_column_forward_max(request, frame, direct)
                .checked_add(1)
                .and_then(|value| value.checked_add(input_channel))
                .ok_or(SeedPlacementError::CoordinateOverflow)?
        } else {
            0i32
        };
        let total = level_bounds
            .iter()
            .map(|(&level, bounds)| {
                bounds.forward_span()
                    + channels
                        .get(&(level as i64))
                        .copied()
                        .unwrap_or_else(|| channel_width(1))
            })
            .sum::<i32>();
        // Pinned outputs ahead of the levels must leave room for every
        // column and channel, and the levels must end inside the world and
        // ahead of every pinned input; otherwise the caller turns the frame.
        if direct {
            if let Some((output_min, _)) = pinned_output_forward_extent(request, frame) {
                if output_min > cursor && cursor + total > output_min {
                    return Ok(None);
                }
            }
        }
        // Beyond the last column the channel plan still turns every net
        // around.  Without a board boundary that reservation stays the fixed
        // legacy one; a bounded board pays for the lanes the last channel's
        // own crossing intervals actually need, which is the only reason a
        // closed forward limit is reachable at all.
        let turnaround = match footprint {
            Some(_) => bounded_turnaround_allowance(last_lanes),
            None => TURNAROUND_ALLOWANCE,
        };
        let limit = forward_limit(frame, request.pins, footprint);
        // Ordered columns, one per level, exactly as the levels were folded
        // laterally.  A board folds the ones that do not fit ahead of its
        // far wall onto decks above -- every deck reusing the same
        // projected start and capacity -- where an unbounded layout has
        // only the one run of columns it always had.
        let ordered_levels = level_bounds.keys().copied().collect::<Vec<_>>();
        let column_width = |level: u64| {
            channels
                .get(&(level as i64))
                .copied()
                .unwrap_or_else(|| channel_width(1))
        };
        let column_decks = match (footprint, limit) {
            (Some(_), Some(limit)) => {
                let columns = ordered_levels
                    .iter()
                    .map(|&level| {
                        let level_bounds = level_bounds[&level];
                        // The macro too big to be folded anywhere is the
                        // one that reaches furthest forward in its column.
                        let owner = bounds
                            .iter()
                            .filter(|(id, _)| analysis.nodes[id].forward_level == level)
                            .max_by_key(|(id, macro_bounds)| {
                                (macro_bounds.forward_span(), std::cmp::Reverse(**id))
                            })
                            .map(|(&id, _)| id)
                            .unwrap_or(InstanceId(0));
                        DeckColumn {
                            owner,
                            lead: channels
                                .get(&(level as i64 - 1))
                                .copied()
                                .unwrap_or_else(|| channel_width(1)),
                            cost: level_bounds.forward_span() + column_width(level),
                            close: bounded_turnaround_allowance(
                                channel_lanes.get(&(level as i64)).copied().unwrap_or(0),
                            ),
                        }
                    })
                    .collect::<Vec<_>>();
                let capacity = limit
                    .checked_sub(cursor)
                    .ok_or(SeedPlacementError::CoordinateOverflow)?;
                pack_decks(&columns, capacity)?
            }
            _ => {
                if let Some(limit) = limit {
                    if cursor + total + turnaround > limit {
                        return Ok(None);
                    }
                }
                vec![DeckId(0); ordered_levels.len()]
            }
        };

        // The pack's inclusive cost is what it weighed a column at; the
        // origins themselves keep the step they have always taken, and a
        // deck above the base one simply starts the cursor over.
        let start = cursor;
        let mut columns = BTreeMap::new();
        let mut level_decks = BTreeMap::<u64, DeckId>::new();
        let mut open = DeckId(0);
        for (index, &level) in ordered_levels.iter().enumerate() {
            let level_bounds = level_bounds[&level];
            let deck = column_decks.get(index).copied().unwrap_or(DeckId(0));
            if deck != open {
                cursor = start;
                open = deck;
            }
            level_decks.insert(level, deck);
            let column = cursor
                .checked_sub(level_bounds.min_forward)
                .ok_or(SeedPlacementError::CoordinateOverflow)?;
            columns.insert(level, column);
            cursor = column
                .checked_add(level_bounds.max_forward)
                .and_then(|value| value.checked_add(column_width(level)))
                .ok_or(SeedPlacementError::CoordinateOverflow)?;
        }

        let mut frame_origins = BTreeMap::<InstanceId, (i32, i32)>::new();
        let mut instance_decks = BTreeMap::<InstanceId, DeckId>::new();
        for (&level, &column) in &columns {
            for (&id, &lateral) in &laterals {
                if analysis.nodes[&id].forward_level == level {
                    frame_origins.insert(id, (column, lateral));
                    instance_decks.insert(id, level_decks[&level]);
                }
            }
        }

        // Each deck reserves the rows its own macros need: one row below
        // the lowest macro cell at the least, and the channel slab or the
        // router's ceiling above the highest, whichever reaches further.
        let mut deck_locals = BTreeMap::<DeckId, (i32, i32)>::new();
        for (&id, &deck) in &instance_decks {
            let macro_bounds = bounds[&id];
            deck_locals
                .entry(deck)
                .and_modify(|(min, max)| {
                    *min = (*min).min(macro_bounds.min_y);
                    *max = (*max).max(macro_bounds.max_y + 3);
                })
                .or_insert((macro_bounds.min_y.min(-1), (macro_bounds.max_y + 3).max(3)));
        }
        if deck_locals.is_empty() {
            // Nothing to stand on a deck still stands on the base one.
            deck_locals.insert(DeckId(0), (-1, 3));
        }
        let locals = deck_locals.values().copied().collect::<Vec<_>>();
        let decks = deck_locals
            .keys()
            .copied()
            .zip(deck_grounds(frame.origin.y, &locals)?)
            .collect::<BTreeMap<DeckId, DeckPlan>>();
        let floorplan = floorplan_metrics(&bounds, &frame_origins, &instance_decks, &decks)?;

        let mut instances = BTreeMap::new();
        for (&id, &(forward, lateral)) in &frame_origins {
            let origin = frame_to_world(frame, forward, lateral);
            instances.insert(
                id,
                PreferredInstancePose {
                    preferred_origin: origin,
                    facing: facings[&id],
                },
            );
        }

        // Automatic inputs sit one input channel behind the origin in the
        // direct frame; in a turned frame they join the pin line instead,
        // since behind the origin is the world edge or the callers' side.
        let input_forward = if direct { -input_channel } else { 0 };
        let output_forward = cursor;
        // An automatic port sits on the row of the macros it is wired to
        // (the median of their origins), so a port feeding one macro gets a
        // straight ground line with no lane and no extra repeater.  Ports
        // wanting the same row are spread a row-grid step apart, nearest
        // first, like every other pair of rows.
        let mut taken_rows = Vec::<i32>::new();
        let mut settle_row = |wanted: i32| -> i32 {
            let free = |row: i32| {
                taken_rows
                    .iter()
                    .all(|taken| (taken - row).abs() >= ROW_GRID)
            };
            let row = (0..)
                .flat_map(|step| [wanted + step, wanted - step])
                .find(|&row| free(row))
                .unwrap_or(wanted);
            taken_rows.push(row);
            row
        };
        let median = |mut rows: Vec<i32>| -> Option<i32> {
            rows.sort_unstable();
            rows.get(rows.len() / 2).copied()
        };
        let automatic_inputs = automatic_input_ports
            .iter()
            .map(|&port| {
                let wired = request
                    .graph
                    .assignments
                    .iter()
                    .filter(|assignment| {
                        matches!(&assignment.driver, PhysicalDriver::PrimaryInput(driver) if *driver == port)
                    })
                    .filter_map(|assignment| match assignment.sink {
                        PhysicalSink::InstanceInput { instance, .. } => laterals.get(&instance).copied(),
                        PhysicalSink::DeclaredOutput(_) => None,
                    })
                    .collect::<Vec<_>>();
                let wanted = match median(wired) {
                    Some(row) => row,
                    None => automatic_input_lateral(port)
                        .checked_add(lateral_shift)
                        .ok_or(SeedPlacementError::CoordinateOverflow)?,
                };
                let lateral = settle_row(wanted);
                Ok((port, frame_to_world(frame, input_forward, lateral)))
            })
            .collect::<Result<BTreeMap<_, _>, SeedPlacementError>>()?;
        let mut taken_rows = Vec::<i32>::new();
        let mut settle_row = |wanted: i32| -> i32 {
            let free = |row: i32| {
                taken_rows
                    .iter()
                    .all(|taken| (taken - row).abs() >= ROW_GRID)
            };
            let row = (0..)
                .flat_map(|step| [wanted + step, wanted - step])
                .find(|&row| free(row))
                .unwrap_or(wanted);
            taken_rows.push(row);
            row
        };
        let automatic_outputs = automatic_output_ports
            .iter()
            .map(|&port| {
                let driver = request
                    .graph
                    .assignments
                    .iter()
                    .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                    .and_then(|assignment| match &assignment.driver {
                        PhysicalDriver::Instance(driver) => {
                            laterals.get(&instance_driver_owner(driver)).copied()
                        }
                        PhysicalDriver::PrimaryInput(_) => None,
                    });
                let wanted = match driver {
                    Some(row) => row,
                    None => automatic_output_lateral(port)
                        .checked_add(lateral_shift)
                        .ok_or(SeedPlacementError::CoordinateOverflow)?,
                };
                let lateral = settle_row(wanted);
                Ok((port, frame_to_world(frame, output_forward, lateral)))
            })
            .collect::<Result<BTreeMap<_, _>, SeedPlacementError>>()?;

        // The one fact the fold left to the packer: which deck each macro
        // ended up on.  Nothing else in the analysis moves -- a column
        // carried up a deck keeps the forward level its channels are
        // derived from.
        for (&id, &deck) in &instance_decks {
            folded
                .nodes
                .get_mut(&id)
                .expect("folded analysis covers every instance")
                .deck = deck;
        }

        let fingerprint = plan_fingerprint(&instances, &automatic_inputs, &automatic_outputs, &[]);
        Ok(Some(SeedPlacementPlan {
            instances,
            automatic_inputs,
            automatic_outputs,
            fingerprint,
            frame,
            analysis: folded,
            window,
            io_footprint: footprint,
            decks,
            floorplan,
        }))
    }
}

impl SeedPlacer for TopologyAwareSeedPlacer {
    fn plan(
        &self,
        request: SeedPlacementRequest<'_>,
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        self.plan_with_widths(request, &BTreeMap::new())
    }

    fn plan_with_repairs(
        &self,
        request: SeedPlacementRequest<'_>,
        repairs: &[LayoutRepair],
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        let mut minimum_widths = BTreeMap::<i64, i32>::new();
        for repair in repairs {
            if let LayoutRepair::WidenChannel { level, width } = *repair {
                minimum_widths
                    .entry(level)
                    .and_modify(|known| *known = (*known).max(width))
                    .or_insert(width);
            }
        }
        let mut plan = self.plan_with_widths(request, &minimum_widths)?;
        let repairs = repairs.iter().copied().collect::<BTreeSet<_>>();
        if repairs.is_empty() {
            return Ok(plan);
        }
        let frame = plan.frame;
        for repair in &repairs {
            match *repair {
                LayoutRepair::ExclusiveGuardedTrack { source } => {
                    let owner = endpoint_layout_owner(source);
                    require_move_owner(
                        &mut plan,
                        owner,
                        request.pins,
                        frame.lateral,
                        TRACK_PITCH.saturating_mul(2),
                    )?;
                }
                LayoutRepair::EarlyTreeSinkAndEscape { source, sink } => {
                    let source_owner = endpoint_layout_owner(source);
                    if !move_owner(
                        &mut plan,
                        source_owner,
                        request.pins,
                        frame.lateral,
                        TRACK_PITCH.saturating_mul(2),
                    )? {
                        require_move_owner(
                            &mut plan,
                            endpoint_layout_owner(sink),
                            request.pins,
                            frame.lateral,
                            TRACK_PITCH.saturating_mul(2),
                        )?;
                    }
                }
                LayoutRepair::WidenChannel { .. } => {}
                LayoutRepair::SeparateOwners {
                    source_owner,
                    sink_owner,
                } => {
                    if source_owner == sink_owner {
                        return Err(SeedPlacementError::SameRepairOwner {
                            owner: source_owner,
                        });
                    }
                    if !move_owner(
                        &mut plan,
                        sink_owner,
                        request.pins,
                        frame.lateral,
                        TRACK_PITCH,
                    )? {
                        require_move_owner(
                            &mut plan,
                            source_owner,
                            request.pins,
                            frame.lateral,
                            -TRACK_PITCH,
                        )?;
                    }
                }
            }
        }
        let repairs = repairs.into_iter().collect::<Vec<_>>();
        plan.fingerprint = plan_fingerprint(
            &plan.instances,
            &plan.automatic_inputs,
            &plan.automatic_outputs,
            &repairs,
        );
        Ok(plan)
    }
}

fn endpoint_layout_owner(endpoint: PhysicalEndpointId) -> LayoutOwner {
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

fn require_move_owner(
    plan: &mut SeedPlacementPlan,
    owner: LayoutOwner,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
    lateral: Facing,
    distance: i32,
) -> Result<(), SeedPlacementError> {
    if move_owner(plan, owner, pins, lateral, distance)? {
        Ok(())
    } else {
        Err(SeedPlacementError::ImmovableRepairOwner { owner })
    }
}

fn move_owner(
    plan: &mut SeedPlacementPlan,
    owner: LayoutOwner,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
    lateral: Facing,
    distance: i32,
) -> Result<bool, SeedPlacementError> {
    let footprint = plan.io_footprint;
    let anchor = match owner {
        LayoutOwner::Instance(instance) => plan
            .instances
            .get_mut(&instance)
            .map(|pose| &mut pose.preferred_origin),
        LayoutOwner::Boundary(endpoint) if pins.contains_key(&endpoint) => None,
        LayoutOwner::Boundary(PhysicalEndpointId::PrimaryInput(port)) => {
            plan.automatic_inputs.get_mut(&port)
        }
        LayoutOwner::Boundary(PhysicalEndpointId::DeclaredOutput(port)) => {
            plan.automatic_outputs.get_mut(&port)
        }
        LayoutOwner::Boundary(_) => None,
    };
    let Some(anchor) = anchor else {
        return Ok(false);
    };
    let moved = checked_step_many(*anchor, lateral, distance)?;
    // The board a complete pin set drew bounds a repair exactly as it
    // bounded the plan: an owner whose track step would land off it is
    // immovable, the same answer an already pinned boundary gives, so the
    // caller's existing fallback -- and `ImmovableRepairOwner` when there
    // is none -- carries the refusal unchanged.  The pose stays where it
    // stood: a refused move leaves nothing half-applied.
    if footprint.is_some_and(|footprint| !footprint.contains_xz(moved)) {
        return Ok(false);
    }
    *anchor = moved;
    Ok(true)
}

fn checked_step_many(
    anchor: Anchor,
    direction: Facing,
    distance: i32,
) -> Result<Anchor, SeedPlacementError> {
    let (dx, dz) = match direction {
        Facing::North => (0, -distance),
        Facing::South => (0, distance),
        Facing::East => (distance, 0),
        Facing::West => (-distance, 0),
        Facing::Up | Facing::Down => (0, 0),
    };
    Ok(Anchor {
        x: anchor
            .x
            .checked_add(dx)
            .ok_or(SeedPlacementError::CoordinateOverflow)?,
        z: anchor
            .z
            .checked_add(dz)
            .ok_or(SeedPlacementError::CoordinateOverflow)?,
        ..anchor
    })
}

/// One ordered column offered to the shelf pack.
///
/// It carries only what the pack weighs: who to name when the column alone
/// is too big, the channel an upper deck starts behind, the column's own
/// forward cost, and the turnaround the deck pays if this column closes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeckColumn {
    owner: InstanceId,
    lead: i32,
    cost: i32,
    close: i32,
}

/// One `DeckId` per ordered column: consecutive columns fill the deck they
/// are offered until the next one would not fit, and then -- and only then
/// -- the next deck opens.
///
/// This is a shelf pack, not a search: a column never moves back to a deck
/// an earlier column left, no alternative packing is enumerated, and the
/// same columns always produce the same decks.  Sums are `i64` because a
/// board's capacity and a column's cost are both `i32` and their sum is
/// not.
fn pack_decks(columns: &[DeckColumn], capacity: i32) -> Result<Vec<DeckId>, SeedPlacementError> {
    let mut assigned = Vec::with_capacity(columns.len());
    if columns.is_empty() {
        return Ok(assigned);
    }
    if capacity <= 0 {
        return Err(SeedPlacementError::NoDeckLayoutFits);
    }
    let capacity = i64::from(capacity);
    let mut deck = 0u32;
    // Deck zero begins at the board's own forward start; every deck above
    // it begins behind the channel its first column follows.
    let mut lead = 0i64;
    let mut used = 0i64;
    let mut placed = 0usize;
    for column in columns {
        let cost = i64::from(column.cost);
        let close = i64::from(column.close);
        if placed > 0 && lead + used + cost + close > capacity {
            deck = deck
                .checked_add(1)
                .ok_or(SeedPlacementError::NoDeckLayoutFits)?;
            lead = i64::from(column.lead);
            used = 0;
            placed = 0;
        }
        if placed == 0 && lead + cost + close > capacity {
            return Err(SeedPlacementError::LateralWindowTooNarrow {
                instance: column.owner,
            });
        }
        used += cost;
        placed += 1;
        assigned.push(DeckId(deck));
    }
    Ok(assigned)
}

/// The absolute interval every deck reserves, from `base` upward.
///
/// `locals` are each deck's own untranslated `(min, max)` rows: how far its
/// macros reach below their ground and how far its channel slab and router
/// ceiling reach above it.  A deck's ground is the first integer that lifts
/// its whole interval past the deck below -- the height-aware separation,
/// not a fixed gap.
fn deck_grounds(base: i32, locals: &[(i32, i32)]) -> Result<Vec<DeckPlan>, SeedPlacementError> {
    let overflow = || SeedPlacementError::CoordinateOverflow;
    let mut plans = Vec::with_capacity(locals.len());
    let mut previous: Option<(i32, i32)> = None;
    for &(local_min, local_max) in locals {
        let ground = match previous {
            None => base,
            Some((previous_ground, previous_max)) => previous_ground
                .checked_add(previous_max)
                .and_then(|top| top.checked_sub(local_min))
                .and_then(|lifted| lifted.checked_add(1))
                .ok_or_else(overflow)?,
        };
        plans.push(DeckPlan {
            ground,
            min_y: ground.checked_add(local_min).ok_or_else(overflow)?,
            max_y: ground.checked_add(local_max).ok_or_else(overflow)?,
        });
        previous = Some((ground, local_max));
    }
    Ok(plans)
}

/// What the plan asks of the board: the macro envelopes' own volume, the
/// volume of the box they occupy where the plan put them, and the deck
/// count.
///
/// The two volumes are the exact pair the density gate divides; neither is
/// ever turned into a float here.  A macro is measured at its own deck's
/// ground, not over that deck's whole reservation interval: the interval
/// also holds the channel slab and the router's ceiling, which are not
/// macro fill.
fn floorplan_metrics(
    bounds: &BTreeMap<InstanceId, MacroBounds>,
    frame_origins: &BTreeMap<InstanceId, (i32, i32)>,
    instance_decks: &BTreeMap<InstanceId, DeckId>,
    decks: &BTreeMap<DeckId, DeckPlan>,
) -> Result<FloorplanMetrics, SeedPlacementError> {
    let overflow = || SeedPlacementError::CoordinateOverflow;
    let mut macro_volume = 0u64;
    let mut union: Option<(i32, i32, i32, i32, i32, i32)> = None;
    for (&id, &(forward, lateral)) in frame_origins {
        let macro_bounds = bounds[&id];
        let ground = instance_decks
            .get(&id)
            .and_then(|deck| decks.get(deck))
            .map_or(0, |deck| deck.ground);
        let volume = u64::try_from(macro_bounds.forward_span())
            .ok()
            .and_then(|span| span.checked_mul(u64::try_from(macro_bounds.lateral_span()).ok()?))
            .and_then(|area| area.checked_mul(u64::try_from(macro_bounds.height()).ok()?))
            .ok_or_else(overflow)?;
        macro_volume = macro_volume.checked_add(volume).ok_or_else(overflow)?;
        let min_forward = forward
            .checked_add(macro_bounds.min_forward)
            .ok_or_else(overflow)?;
        let max_forward = forward
            .checked_add(macro_bounds.max_forward)
            .ok_or_else(overflow)?;
        let min_lateral = lateral
            .checked_add(macro_bounds.min_lateral)
            .ok_or_else(overflow)?;
        let max_lateral = lateral
            .checked_add(macro_bounds.max_lateral)
            .ok_or_else(overflow)?;
        let min_y = ground.checked_add(macro_bounds.min_y).ok_or_else(overflow)?;
        let max_y = ground.checked_add(macro_bounds.max_y).ok_or_else(overflow)?;
        union = Some(match union {
            None => (
                min_forward,
                max_forward,
                min_lateral,
                max_lateral,
                min_y,
                max_y,
            ),
            Some(known) => (
                known.0.min(min_forward),
                known.1.max(max_forward),
                known.2.min(min_lateral),
                known.3.max(max_lateral),
                known.4.min(min_y),
                known.5.max(max_y),
            ),
        });
    }
    let union_volume = match union {
        None => 0,
        Some((min_forward, max_forward, min_lateral, max_lateral, min_y, max_y)) => {
            let span = |low: i32, high: i32| {
                high.checked_sub(low)
                    .and_then(|span| span.checked_add(1))
                    .and_then(|span| u64::try_from(span).ok())
                    .ok_or_else(overflow)
            };
            let height = span(min_y, max_y)?;
            span(min_forward, max_forward)?
                .checked_mul(span(min_lateral, max_lateral)?)
                .and_then(|area| area.checked_mul(height))
                .ok_or_else(overflow)?
        }
    };
    Ok(FloorplanMetrics {
        macro_volume,
        union_volume,
        deck_count: u32::try_from(decks.len()).map_err(|_| SeedPlacementError::NoDeckLayoutFits)?,
        cross_deck_nets: 0,
        vertical_trunk_lanes: 0,
    })
}

/// Free forward cells after every level (keyed by that level; the input
/// channel is keyed by `min_level - 1`), beside the lane count each of
/// those channels was measured from.
///
/// A channel needs one lane per trunk that crosses it at the same lateral
/// range, so the width comes from the left-edge lane count over the lateral
/// intervals of the nets alive in that channel.  When both inputs and
/// outputs are pinned the columns must still fit between the pin lines, so
/// the widths are scaled down proportionally when their sum does not fit;
/// the seed reports a typed refusal if a channel then cannot hold its lanes.
///
/// The lane counts are returned rather than read back out of the widths:
/// the turnaround beyond a deck's last column carries those same trunks,
/// and a width is a rounded-up cell budget no lane count can be recovered
/// from.
fn derive_channel_widths(
    request: SeedPlacementRequest<'_>,
    analysis: &SeedPlacementAnalysis,
    level_bounds: &BTreeMap<u64, MacroBounds>,
    laterals: &BTreeMap<InstanceId, i32>,
    track_laterals: &BTreeMap<LogicalSignalId, i32>,
) -> (BTreeMap<i64, i32>, BTreeMap<i64, usize>) {
    let intervals = net_intervals(request.graph, analysis);
    let min_level = level_bounds.keys().next().copied().unwrap_or(0) as i64;
    let max_level = level_bounds.keys().last().copied().unwrap_or(0) as i64;
    let mut widths = BTreeMap::new();
    let mut lanes_by_channel = BTreeMap::new();
    for channel_level in (min_level - 1)..=max_level {
        let mut crossing = Vec::new();
        for interval in &intervals {
            let start = interval.start as i64;
            let end = interval.end as i64;
            // A primary input lives in the input column, one before level 0;
            // `net_intervals` reports its start as level 0.
            let source_level = if request.graph.assignments.iter().any(|assignment| {
                assignment.signal == interval.signal
                    && matches!(assignment.driver, PhysicalDriver::PrimaryInput(_))
            }) {
                min_level - 1
            } else {
                start
            };
            if source_level > channel_level || end <= channel_level {
                continue;
            }
            let mut lateral_extent: Option<(i32, i32)> = None;
            let mut widen = |lateral: i32| {
                lateral_extent = Some(match lateral_extent {
                    None => (lateral - ENDPOINT_ROW_MARGIN, lateral + ENDPOINT_ROW_MARGIN),
                    Some((min, max)) => (
                        min.min(lateral - ENDPOINT_ROW_MARGIN),
                        max.max(lateral + ENDPOINT_ROW_MARGIN),
                    ),
                });
            };
            for assignment in &request.graph.assignments {
                if assignment.signal != interval.signal {
                    continue;
                }
                match &assignment.driver {
                    PhysicalDriver::PrimaryInput(_) => {
                        if let Some(lateral) = track_laterals.get(&interval.signal) {
                            widen(*lateral);
                        }
                    }
                    PhysicalDriver::Instance(driver) => {
                        if let Some(lateral) = laterals.get(&instance_driver_owner(driver)) {
                            widen(*lateral);
                        }
                    }
                }
                match assignment.sink {
                    PhysicalSink::InstanceInput { instance, .. } => {
                        if let Some(lateral) = laterals.get(&instance) {
                            widen(*lateral);
                        }
                    }
                    PhysicalSink::DeclaredOutput(_) => {
                        if let Some(lateral) = track_laterals.get(&interval.signal) {
                            widen(*lateral);
                        }
                    }
                }
            }
            if let Some(extent) = lateral_extent {
                crossing.push(extent);
            }
        }
        // The source anchor on one side and the sink terminal on the other
        // each take one more cell than the macro envelope.
        let lanes = lane_count(&crossing);
        lanes_by_channel.insert(channel_level, lanes);
        widths.insert(
            channel_level,
            channel_width(lanes) + ENDPOINT_CELLS_PER_CHANNEL,
        );
    }

    (widths, lanes_by_channel)
}

/// The rectangle a complete pin set draws, or `None` for a partial one.
///
/// Complete means every port the graph declares: the same question the
/// planner asks of the same cells when it refuses a pin outside the board.
/// A partial set is deliberately no rectangle at all -- a handful of fixed
/// ports says nothing about where the board ends -- and leaves the placer on
/// the branches it took before boards had boundaries.
fn io_footprint(request: SeedPlacementRequest<'_>) -> Option<IoFootprint> {
    let declared = request.graph.primary_inputs.len() + request.graph.declared_outputs.len();
    IoFootprint::from_complete(
        declared,
        request
            .pins
            .iter()
            .filter(|(endpoint, _)| {
                matches!(
                    endpoint,
                    PhysicalEndpointId::PrimaryInput(_) | PhysicalEndpointId::DeclaredOutput(_)
                )
            })
            .map(|(_, pin)| pin.at),
    )
}

/// Lateral bounds from the world edge, from every pinned input whose signal
/// enters along the lateral axis (the circuit lies on the side its signal
/// heads to), and -- for a complete pin set -- from the board's own
/// rectangle, which closes whichever side the pins left open.
fn lateral_window(
    frame: PlacementFrame,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
    footprint: Option<IoFootprint>,
) -> LateralWindow {
    let (lx, lz) = horizontal_unit(frame.lateral);
    let (sign, origin) = if lx != 0 {
        (lx, frame.origin.x)
    } else {
        (lz, frame.origin.z)
    };
    // World coordinate along the lateral axis: `origin + sign * lateral >= 0`.
    let mut window = if sign > 0 {
        LateralWindow {
            min: Some(-origin),
            max: None,
        }
    } else {
        LateralWindow {
            min: None,
            max: Some(origin),
        }
    };
    for (endpoint, pin) in pins {
        if !matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)) {
            continue;
        }
        let lateral = lateral_projection(frame, pin.at);
        if pin.toward == frame.lateral {
            window.min = Some(window.min.map_or(lateral + 1, |min| min.max(lateral + 1)));
        } else if pin.toward == frame.lateral.opposite() {
            window.max = Some(window.max.map_or(lateral - 1, |max| max.min(lateral - 1)));
        }
    }
    // The board's walls are the caller's own cells, so the layout may stand
    // on them: the rectangle bounds inclusively, unlike the half-space a pin
    // opens one cell past itself.  All four corners are projected because
    // the frame may have turned the rectangle.
    if let Some(footprint) = footprint {
        let origin_lateral = project_horizontal(frame.origin.x, frame.origin.z, frame.lateral);
        let (_, _, lateral_min, lateral_max) = footprint.projected(frame.forward, frame.lateral);
        let (min, max) = (lateral_min - origin_lateral, lateral_max - origin_lateral);
        window.min = Some(window.min.map_or(min, |known| known.max(min)));
        window.max = Some(window.max.map_or(max, |known| known.min(max)));
    }
    window
}

/// Largest forward coordinate the layout may reach: the world edge when the
/// forward axis runs toward it, one cell before any pinned input whose
/// signal enters against the forward axis, and -- for a complete pin set --
/// the far wall of the board's own rectangle.
fn forward_limit(
    frame: PlacementFrame,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
    footprint: Option<IoFootprint>,
) -> Option<i32> {
    let (fx, fz) = horizontal_unit(frame.forward);
    let (sign, origin) = if fx != 0 {
        (fx, frame.origin.x)
    } else {
        (fz, frame.origin.z)
    };
    let mut limit = (sign < 0).then_some(origin);
    let origin_forward = project_horizontal(frame.origin.x, frame.origin.z, frame.forward);
    for (endpoint, pin) in pins {
        if !matches!(endpoint, PhysicalEndpointId::PrimaryInput(_))
            || pin.toward != frame.forward.opposite()
        {
            continue;
        }
        let forward = project_horizontal(pin.at.x, pin.at.z, frame.forward) - origin_forward - 1;
        limit = Some(limit.map_or(forward, |known| known.min(forward)));
    }
    if let Some(footprint) = footprint {
        let (_, forward_max, _, _) = footprint.projected(frame.forward, frame.lateral);
        let forward = forward_max - origin_forward;
        limit = Some(limit.map_or(forward, |known| known.min(forward)));
    }
    limit
}

/// Forward coordinate, relative to the frame origin, of the last cell of the
/// pin column: every pinned input cell, and every pinned output cell that
/// does not lie ahead of the inputs (every pinned output in a turned frame).
fn pin_column_forward_max(
    request: SeedPlacementRequest<'_>,
    frame: PlacementFrame,
    direct: bool,
) -> i32 {
    let origin = project_horizontal(frame.origin.x, frame.origin.z, frame.forward);
    let forward_of = |at: Anchor| project_horizontal(at.x, at.z, frame.forward) - origin;
    let cells = |pin: &PortPin, role: PortRole| [pin.at, pin.handover(role), pin.net_cell(role)];
    let inputs_max = request
        .pins
        .iter()
        .filter(|(endpoint, _)| matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)))
        .flat_map(|(_, pin)| cells(pin, PortRole::Input))
        .map(forward_of)
        .max()
        .unwrap_or(0);
    request
        .pins
        .iter()
        .filter(|(endpoint, _)| matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)))
        .flat_map(|(_, pin)| cells(pin, PortRole::Output))
        .map(forward_of)
        .filter(|&forward| !direct || forward <= inputs_max)
        .max()
        .unwrap_or(inputs_max)
        .max(inputs_max)
}

/// Forward extent, relative to the frame origin, of the pinned output net
/// cells: `None` without pinned outputs.
fn pinned_output_forward_extent(
    request: SeedPlacementRequest<'_>,
    frame: PlacementFrame,
) -> Option<(i32, i32)> {
    let origin = project_horizontal(frame.origin.x, frame.origin.z, frame.forward);
    request
        .pins
        .iter()
        .filter(|(endpoint, _)| matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)))
        .map(|(_, pin)| {
            let cell = pin.net_cell(PortRole::Output);
            project_horizontal(cell.x, cell.z, frame.forward) - origin
        })
        .fold(None, |extent, forward| {
            Some(match extent {
                None => (forward, forward),
                Some((min, max)) => (min.min(forward), max.max(forward)),
            })
        })
}

pub(crate) fn derive_frame(pins: &BTreeMap<PhysicalEndpointId, PortPin>) -> PlacementFrame {
    let inputs = pins
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)).then_some(*pin)
        })
        .collect::<Vec<_>>();
    let outputs = pins
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)).then_some(*pin)
        })
        .collect::<Vec<_>>();
    let forward = if !inputs.is_empty() && !outputs.is_empty() {
        let from = median_anchor(inputs.iter().map(|pin| pin.net_cell(PortRole::Input)));
        let to = median_anchor(outputs.iter().map(|pin| pin.net_cell(PortRole::Output)));
        dominant_direction(from, to)
    } else if !inputs.is_empty() {
        majority_direction(inputs.iter().map(|pin| pin.toward))
    } else if !outputs.is_empty() {
        majority_direction(outputs.iter().map(|pin| pin.toward)).opposite()
    } else {
        Facing::East
    };
    let origin = if !inputs.is_empty() {
        median_anchor(inputs.iter().map(|pin| pin.net_cell(PortRole::Input)))
    } else if !outputs.is_empty() {
        median_anchor(outputs.iter().map(|pin| pin.net_cell(PortRole::Output)))
    } else {
        Anchor { x: 0, y: 1, z: 0 }
    };
    PlacementFrame {
        forward,
        lateral: clockwise(forward),
        origin,
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

fn dominant_direction(from: Anchor, to: Anchor) -> Facing {
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

const fn clockwise(direction: Facing) -> Facing {
    match direction {
        Facing::North => Facing::East,
        Facing::East => Facing::South,
        Facing::South => Facing::West,
        Facing::West => Facing::North,
        Facing::Up | Facing::Down => unreachable!(),
    }
}

fn frame_to_world(frame: PlacementFrame, forward: i32, lateral: i32) -> Anchor {
    let (fx, fz) = horizontal_unit(frame.forward);
    let (lx, lz) = horizontal_unit(frame.lateral);
    Anchor {
        x: frame.origin.x + fx * forward + lx * lateral,
        y: frame.origin.y,
        z: frame.origin.z + fz * forward + lz * lateral,
    }
}

pub(crate) const fn horizontal_unit(direction: Facing) -> (i32, i32) {
    match direction {
        Facing::North => (0, -1),
        Facing::South => (0, 1),
        Facing::East => (1, 0),
        Facing::West => (-1, 0),
        Facing::Up | Facing::Down => unreachable!(),
    }
}

pub(crate) const fn project_horizontal(x: i32, z: i32, direction: Facing) -> i32 {
    match direction {
        Facing::North => -z,
        Facing::South => z,
        Facing::East => x,
        Facing::West => -x,
        Facing::Up | Facing::Down => unreachable!(),
    }
}

/// Shifts every macro's lateral so the layout, with the margin the channel
/// plan needs beside it, stays inside the lateral window.  Pinned ports fix
/// the frame origin, so only the macros and the automatic ports (at
/// `port_laterals`) can move; they all move together, by whole row-grid
/// steps, and only when a window edge is on their side.  Returns the shift
/// the caller applies to the ports.
fn confine_laterals(
    window: LateralWindow,
    bounds: &BTreeMap<InstanceId, MacroBounds>,
    port_laterals: &[i32],
    laterals: &mut BTreeMap<InstanceId, i32>,
) -> Result<i32, SeedPlacementError> {
    let extent_min = laterals
        .iter()
        .map(|(id, lateral)| lateral + bounds[id].min_lateral)
        .chain(
            port_laterals
                .iter()
                .map(|lateral| lateral - ENDPOINT_ROW_MARGIN),
        )
        .min();
    let extent_max = laterals
        .iter()
        .map(|(id, lateral)| lateral + bounds[id].max_lateral)
        .chain(
            port_laterals
                .iter()
                .map(|lateral| lateral + ENDPOINT_ROW_MARGIN),
        )
        .max();
    let (Some(extent_min), Some(extent_max)) = (extent_min, extent_max) else {
        return Ok(0);
    };
    let below = window
        .min
        .map_or(0, |min| (min + WINDOW_MARGIN - extent_min).max(0));
    let above = window
        .max
        .map_or(0, |max| (extent_max - (max - WINDOW_MARGIN)).max(0));
    let (shortfall, sign) = if below > 0 {
        (below, 1)
    } else if above > 0 {
        (above, -1)
    } else {
        return Ok(0);
    };
    let steps = (shortfall + ROW_GRID - 1) / ROW_GRID;
    let shift = steps
        .checked_mul(ROW_GRID)
        .map(|shift| shift * sign)
        .ok_or(SeedPlacementError::CoordinateOverflow)?;
    for lateral in laterals.values_mut() {
        *lateral = lateral
            .checked_add(shift)
            .ok_or(SeedPlacementError::CoordinateOverflow)?;
    }
    Ok(shift)
}

fn legalize_laterals(
    entries: &[(InstanceId, i32, MacroBounds)],
) -> Result<BTreeMap<InstanceId, i32>, SeedPlacementError> {
    let mut next_min_lateral: Option<i32> = None;
    let mut origins = BTreeMap::new();
    for &(instance, preferred, bounds) in entries {
        let required_origin = next_min_lateral
            .map(|minimum| {
                minimum
                    .checked_sub(bounds.min_lateral)
                    .ok_or(SeedPlacementError::CoordinateOverflow)
            })
            .transpose()?
            .unwrap_or(preferred);
        let origin = preferred.max(required_origin);
        // Snap up to the row grid: every macro's supports and anchors sit on
        // multiples of the grid relative to its origin.
        let origin = origin
            .checked_add(ROW_GRID - 1)
            .map(|value| value.div_euclid(ROW_GRID) * ROW_GRID)
            .ok_or(SeedPlacementError::CoordinateOverflow)?;
        origins.insert(instance, origin);
        next_min_lateral = Some(
            origin
                .checked_add(bounds.max_lateral)
                .and_then(|maximum| maximum.checked_add(LATERAL_GAP))
                .ok_or(SeedPlacementError::CoordinateOverflow)?,
        );
    }
    Ok(origins)
}

fn net_intervals(graph: &InstanceGraph, analysis: &SeedPlacementAnalysis) -> Vec<NetInterval> {
    let max_level = analysis
        .nodes
        .values()
        .map(|node| node.forward_level)
        .max()
        .unwrap_or(0);
    let mut grouped = BTreeMap::<
        LogicalSignalId,
        Vec<&crate::compile::fragment_synth::instance_graph::SinkAssignment>,
    >::new();
    for assignment in &graph.assignments {
        grouped
            .entry(assignment.signal)
            .or_default()
            .push(assignment);
    }
    grouped
        .into_iter()
        .map(|(signal, assignments)| {
            let start = assignments
                .iter()
                .map(|assignment| match &assignment.driver {
                    PhysicalDriver::PrimaryInput(_) => 0,
                    PhysicalDriver::Instance(driver) => {
                        analysis.nodes[&instance_driver_owner(driver)].forward_level
                    }
                })
                .min()
                .unwrap_or(0);
            let end = assignments
                .iter()
                .map(|assignment| match assignment.sink {
                    PhysicalSink::InstanceInput { instance, .. } => {
                        analysis.nodes[&instance].forward_level
                    }
                    PhysicalSink::DeclaredOutput(_) => max_level + 1,
                })
                .max()
                .unwrap_or(start);
            let slack = assignments
                .iter()
                .filter_map(|assignment| {
                    let PhysicalDriver::Instance(driver) = &assignment.driver else {
                        return None;
                    };
                    let PhysicalSink::InstanceInput { instance, .. } = assignment.sink else {
                        return None;
                    };
                    analysis
                        .edges
                        .iter()
                        .find(|edge| {
                            edge.source == instance_driver_owner(driver) && edge.sink == instance
                        })
                        .map(|edge| edge.structural_slack_ticks)
                })
                .min()
                .unwrap_or(0);
            NetInterval {
                signal,
                start,
                end,
                fanout: assignments.len(),
                slack,
            }
        })
        .collect()
}

fn colour_intervals(intervals: &[NetInterval]) -> BTreeMap<LogicalSignalId, usize> {
    let mut ordered = intervals.to_vec();
    ordered.sort_by_key(|interval| {
        (
            std::cmp::Reverse(interval.end - interval.start),
            std::cmp::Reverse(interval.fanout),
            interval.slack,
            interval.signal,
        )
    });
    let mut occupied = Vec::<Vec<(u64, u64)>>::new();
    let mut tracks = BTreeMap::new();
    for interval in ordered {
        let track = occupied
            .iter()
            .position(|uses| {
                uses.iter()
                    .all(|&(start, end)| interval.end < start || end < interval.start)
            })
            .unwrap_or(occupied.len());
        if track == occupied.len() {
            occupied.push(Vec::new());
        }
        occupied[track].push((interval.start, interval.end));
        tracks.insert(interval.signal, track);
    }
    tracks
}

fn track_lateral(tracks: &BTreeMap<LogicalSignalId, usize>, signal: LogicalSignalId) -> i32 {
    i32::try_from(tracks.get(&signal).copied().unwrap_or(0)).unwrap_or(i32::MAX / TRACK_PITCH)
        * TRACK_PITCH
}

fn track_laterals(
    graph: &InstanceGraph,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
    frame: PlacementFrame,
    tracks: &BTreeMap<LogicalSignalId, usize>,
) -> BTreeMap<LogicalSignalId, i32> {
    let mut laterals = tracks
        .keys()
        .copied()
        .map(|signal| (signal, track_lateral(tracks, signal)))
        .collect::<BTreeMap<_, _>>();
    for (endpoint, pin) in pins {
        let (signal, role) = match *endpoint {
            PhysicalEndpointId::PrimaryInput(port) => {
                (Some(LogicalSignalId::PrimaryInput(port)), PortRole::Input)
            }
            PhysicalEndpointId::DeclaredOutput(port) => (
                graph
                    .assignments
                    .iter()
                    .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                    .map(|assignment| assignment.signal),
                PortRole::Output,
            ),
            _ => continue,
        };
        if let Some(signal) = signal {
            laterals.insert(signal, lateral_projection(frame, pin.net_cell(role)));
        }
    }
    laterals
}

fn lateral_projection(frame: PlacementFrame, anchor: Anchor) -> i32 {
    let dx = anchor.x - frame.origin.x;
    let dz = anchor.z - frame.origin.z;
    let (lx, lz) = horizontal_unit(frame.lateral);
    dx * lx + dz * lz
}

fn initial_lanes(
    graph: &InstanceGraph,
    track_laterals: &BTreeMap<LogicalSignalId, i32>,
    analysis: &SeedPlacementAnalysis,
) -> BTreeMap<InstanceId, i32> {
    let fanout = graph.assignments.iter().fold(
        BTreeMap::<LogicalSignalId, usize>::new(),
        |mut counts, assignment| {
            *counts.entry(assignment.signal).or_default() += 1;
            counts
        },
    );
    graph
        .instances
        .iter()
        .map(|instance| instance.id)
        .chain(graph.blocks.iter().map(|block| block.id))
        .map(|id| {
        let mut values = Vec::new();
        for assignment in graph.assignments.iter().filter(|assignment| {
            matches!(assignment.sink, PhysicalSink::InstanceInput { instance: sink, .. } if sink == id)
                || matches!(&assignment.driver, PhysicalDriver::Instance(driver) if instance_driver_owner(driver) == id)
        }) {
            let critical = match (&assignment.driver, assignment.sink) {
                (PhysicalDriver::Instance(driver), PhysicalSink::InstanceInput { instance: sink, .. }) => analysis.edges.iter().any(|edge| edge.source == instance_driver_owner(driver) && edge.sink == sink && edge.structural_slack_ticks == 0),
                _ => false,
            };
            let weight = 1 + fanout[&assignment.signal] + usize::from(critical) * 4;
            values.extend(std::iter::repeat_n(track_laterals[&assignment.signal], weight));
        }
        values.sort();
        (id, values.get(values.len() / 2).copied().unwrap_or(0))
    }).collect()
}

fn barycentric_sweep(
    analysis: &SeedPlacementAnalysis,
    lanes: &mut BTreeMap<InstanceId, i32>,
    forward: bool,
) {
    let ids: Vec<_> = if forward {
        analysis.order.clone()
    } else {
        analysis.order.iter().rev().copied().collect()
    };
    for id in ids {
        let neighbours = if forward {
            &analysis.nodes[&id].predecessors
        } else {
            &analysis.nodes[&id].successors
        };
        if neighbours.is_empty() {
            continue;
        }
        let sum: i64 = neighbours.iter().map(|other| i64::from(lanes[other])).sum();
        let barycentre = (sum / i64::try_from(neighbours.len()).unwrap_or(1)) as i32;
        let incident = lanes[&id];
        lanes.insert(id, (barycentre + incident) / 2);
    }
}

fn primitive_positions(instance: &Instance) -> BTreeMap<PrimitiveId, Position> {
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

fn macro_envelope(instance: &Instance) -> Result<MacroEnvelope, SeedPlacementError> {
    let positions = primitive_positions(instance);
    let mut by_facing = [HorizontalBounds::default(); 4];
    let mut vertical = None::<(i32, i32)>;
    for facing in [
        CellFacing::NORTH,
        CellFacing::EAST,
        CellFacing::SOUTH,
        CellFacing::WEST,
    ] {
        let mut bounds = None::<HorizontalBounds>;
        for primitive in &instance.expanded.topology.primitives {
            let variants = physical::variants(primitive.primitive);
            if variants.is_empty() {
                return Err(SeedPlacementError::MissingPhysicalVariant {
                    primitive: primitive.primitive,
                });
            }
            let local = positions[&primitive.id];
            let (base_x, base_y, base_z) = geometry::rotate((local.x, local.y, local.z), facing);
            let variant = &variants[usize::from(facing.index())];
            for point in variant
                .blocks
                .iter()
                .map(|block| block.position)
                .chain(variant.ports.iter().map(|port| port.position))
            {
                let x = base_x + point.x;
                let z = base_z + point.z;
                let y = base_y + point.y;
                vertical = Some(match vertical {
                    Some((min, max)) => (min.min(y), max.max(y)),
                    None => (y, y),
                });
                bounds = Some(match bounds {
                    Some(bounds) => HorizontalBounds {
                        min_x: bounds.min_x.min(x),
                        max_x: bounds.max_x.max(x),
                        min_z: bounds.min_z.min(z),
                        max_z: bounds.max_z.max(z),
                    },
                    None => HorizontalBounds {
                        min_x: x,
                        max_x: x,
                        min_z: z,
                        max_z: z,
                    },
                });
            }
        }
        by_facing[usize::from(facing.index())] = bounds.unwrap_or_default();
    }
    let (min_y, max_y) = vertical.unwrap_or((0, 0));
    Ok(MacroEnvelope {
        by_facing,
        min_y,
        max_y,
    })
}

/// A block's envelope: a plain `width`x`depth`x`height` box, identical for
/// all four facings since a block always keeps its certified, east-facing
/// layout.
fn block_envelope(facts: BlockFacts) -> MacroEnvelope {
    let bounds = HorizontalBounds {
        min_x: 0,
        max_x: facts.width - 1,
        min_z: 0,
        max_z: facts.depth - 1,
    };
    MacroEnvelope {
        by_facing: [bounds; 4],
        min_y: 0,
        max_y: facts.height - 1,
    }
}

fn choose_instance_facing(
    instance: &Instance,
    origin: Anchor,
    source: Anchor,
    target: Anchor,
    forward: Facing,
) -> Result<CellFacing, SeedPlacementError> {
    let positions = primitive_positions(instance);
    let roles = instance
        .expanded
        .topology
        .primitives
        .iter()
        .map(|primitive| (primitive.role, positions[&primitive.id]))
        .collect::<BTreeMap<_, _>>();
    let mut best = None;
    // Only the facing whose output leaves forward is a candidate: every
    // macro output feeds a later level, so it must exit toward the channel
    // ahead; a sideways macro would also put its sockets and anchor off the
    // row grid.  The scoring still runs so the pose derivation stays
    // exercised.
    for (rank, facing) in [
        CellFacing::NORTH,
        CellFacing::EAST,
        CellFacing::SOUTH,
        CellFacing::WEST,
    ]
    .into_iter()
    .enumerate()
    .filter(|(_, facing)| macro_output_direction(instance, *facing) == forward)
    {
        let mut score = hint_penalty(
            &roles,
            &instance.expanded.topology.embedding_hints,
            facing,
            forward,
        ) * 8;
        for primitive in &instance.expanded.topology.primitives {
            let local = positions[&primitive.id];
            let at = primitive_world(origin, local, facing);
            score += manhattan(source, input_terminal(primitive.primitive, facing, at));
            score += manhattan(output_terminal(primitive.primitive, facing, at), target);
        }
        for connection in &instance.expanded.topology.connections {
            let ConnectionSource::Primitive(source_id) = connection.source else {
                continue;
            };
            let ConnectionTarget::Primitive(target_id) = connection.target else {
                continue;
            };
            let source_spec = instance
                .expanded
                .topology
                .primitives
                .iter()
                .find(|primitive| primitive.id == source_id)
                .expect("validated source");
            let target_spec = instance
                .expanded
                .topology
                .primitives
                .iter()
                .find(|primitive| primitive.id == target_id)
                .expect("validated target");
            let source_at = primitive_world(origin, positions[&source_id], facing);
            let target_at = primitive_world(origin, positions[&target_id], facing);
            let source_port = output_terminal(source_spec.primitive, facing, source_at);
            let target_port = input_terminal(target_spec.primitive, facing, target_at);
            score += manhattan(source_port, target_port);
            if forward_projection(target_port, forward) <= forward_projection(source_port, forward)
            {
                score += 4;
            }
        }
        let key = (score, rank);
        if best.map(|(old, _)| key < old).unwrap_or(true) {
            best = Some((key, facing));
        }
    }
    Ok(best
        .map(|(_, facing)| facing)
        .unwrap_or_else(|| facing_for_direction(forward)))
}

fn primitive_world(origin: Anchor, local: Position, facing: CellFacing) -> Anchor {
    let (x, y, z) = geometry::rotate((local.x, local.y, local.z), facing);
    Anchor {
        x: origin.x + x,
        y: origin.y + y,
        z: origin.z + z,
    }
}

fn forward_projection(anchor: Anchor, forward: Facing) -> i64 {
    match forward {
        Facing::North => -i64::from(anchor.z),
        Facing::South => i64::from(anchor.z),
        Facing::East => i64::from(anchor.x),
        Facing::West => -i64::from(anchor.x),
        Facing::Up | Facing::Down => 0,
    }
}

fn input_terminal(primitive: Primitive, facing: CellFacing, origin: Anchor) -> Anchor {
    let kind = match primitive {
        Primitive::Torch => PortKind::TorchInput,
        Primitive::Repeater => PortKind::RepeaterRear,
        Primitive::Comparator => PortKind::ComparatorRear,
        Primitive::Lamp => PortKind::LampInput,
        Primitive::Lever => PortKind::LeverOutput,
    };
    physical_terminal(primitive, facing, origin, kind)
}

/// The world direction a macro's output leaves in for this facing: the
/// output port's direction for a primitive output, the cell facing itself for
/// a junction.
fn macro_output_direction(instance: &Instance, facing: CellFacing) -> Facing {
    match &instance.expanded.topology.output {
        OutputSpec::Primitive(id) => {
            let primitive = instance
                .expanded
                .topology
                .primitives
                .iter()
                .find(|spec| spec.id == *id)
                .map(|spec| spec.primitive)
                .unwrap_or(Primitive::Torch);
            let kind = match primitive {
                Primitive::Torch => PortKind::TorchOutput,
                Primitive::Repeater => PortKind::RepeaterFront,
                Primitive::Comparator => PortKind::ComparatorFront,
                Primitive::Lever => PortKind::LeverOutput,
                Primitive::Lamp => PortKind::LampInput,
            };
            physical::variants(primitive)[usize::from(facing.index())]
                .port(kind)
                .direction
        }
        OutputSpec::Junction { .. } => facing.direction(),
    }
}

fn output_terminal(primitive: Primitive, facing: CellFacing, origin: Anchor) -> Anchor {
    let kind = match primitive {
        Primitive::Torch => PortKind::TorchOutput,
        Primitive::Repeater => PortKind::RepeaterFront,
        Primitive::Comparator => PortKind::ComparatorFront,
        Primitive::Lever => PortKind::LeverOutput,
        Primitive::Lamp => PortKind::LampInput,
    };
    physical_terminal(primitive, facing, origin, kind)
}

fn physical_terminal(
    primitive: Primitive,
    facing: CellFacing,
    origin: Anchor,
    kind: PortKind,
) -> Anchor {
    let port = physical::variants(primitive)[usize::from(facing.index())].port(kind);
    let at = Anchor {
        x: origin.x + port.position.x,
        y: origin.y + port.position.y,
        z: origin.z + port.position.z,
    };
    step_anchor(at, port.direction)
}

fn step_anchor(at: Anchor, direction: Facing) -> Anchor {
    match direction {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

fn hint_penalty(
    nodes: &BTreeMap<TemplateNode, Position>,
    hints: &[EmbeddingHint],
    facing: CellFacing,
    forward: Facing,
) -> i64 {
    hints
        .iter()
        .map(|hint| {
            let (left, right, opposite) = match *hint {
                EmbeddingHint::OppositeSides(left, right) => (left, right, true),
                EmbeddingHint::Coplanar(left, right) => (left, right, false),
            };
            let Some(left) = nodes.get(&left) else {
                return 0;
            };
            let Some(right) = nodes.get(&right) else {
                return 0;
            };
            let left = geometry::rotate((left.x, left.y, left.z), facing);
            let right = geometry::rotate((right.x, right.y, right.z), facing);
            let projection = match forward {
                Facing::East | Facing::West => (left.0 - right.0).abs(),
                Facing::North | Facing::South => (left.2 - right.2).abs(),
                Facing::Up | Facing::Down => 0,
            };
            if opposite {
                if projection == 0 {
                    8
                } else {
                    0
                }
            } else {
                i64::from(projection)
            }
        })
        .sum()
}

fn manhattan(left: Anchor, right: Anchor) -> i64 {
    i64::from((left.x - right.x).abs())
        + i64::from((left.y - right.y).abs())
        + i64::from((left.z - right.z).abs())
}

fn facing_for_direction(direction: Facing) -> CellFacing {
    match direction {
        Facing::North => CellFacing::NORTH,
        Facing::South => CellFacing::SOUTH,
        Facing::East => CellFacing::EAST,
        Facing::West => CellFacing::WEST,
        Facing::Up | Facing::Down => CellFacing::NORTH,
    }
}

fn plan_fingerprint(
    instances: &BTreeMap<InstanceId, PreferredInstancePose>,
    automatic_inputs: &BTreeMap<PortId, Anchor>,
    automatic_outputs: &BTreeMap<PortId, Anchor>,
    repairs: &[LayoutRepair],
) -> Fingerprint {
    let poses = instances
        .iter()
        .map(|(id, pose)| (*id, pose.preferred_origin, pose.facing.index()))
        .collect::<Vec<_>>();
    let bytes = if repairs.is_empty() {
        serde_json::to_vec(&(poses, automatic_inputs, automatic_outputs))
    } else {
        serde_json::to_vec(&(
            "topology-aware-seed-repairs-v1",
            poses,
            automatic_inputs,
            automatic_outputs,
            repairs,
        ))
    }
    .expect("placement plan payload serializes");
    canonical_fingerprint(&bytes)
}

pub(crate) fn analyse_instance_dag(
    graph: &InstanceGraph,
    block_delays: &BTreeMap<InstanceId, u64>,
) -> Result<SeedPlacementAnalysis, SeedPlacementError> {
    let all_ids = graph
        .instances
        .iter()
        .map(|instance| instance.id)
        .chain(graph.blocks.iter().map(|block| block.id))
        .collect::<Vec<_>>();
    let ids = all_ids.iter().copied().collect::<BTreeSet<_>>();
    if ids.len() != all_ids.len() {
        let mut seen = BTreeSet::new();
        let instance = all_ids
            .iter()
            .copied()
            .find(|instance| !seen.insert(*instance))
            .expect("different instance and identity counts imply a duplicate");
        return Err(SeedPlacementError::DuplicateInstance { instance });
    }

    let mut predecessors = ids
        .iter()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut successors = predecessors.clone();
    let mut structural_edges = BTreeSet::new();
    let mut declared_output_drivers = BTreeSet::new();

    for assignment in &graph.assignments {
        if let PhysicalSink::InstanceInput { instance, .. } = assignment.sink {
            require_instance(&ids, instance)?;
        }
        let PhysicalDriver::Instance(driver) = &assignment.driver else {
            continue;
        };
        let source = instance_driver_owner(driver);
        require_instance(&ids, source)?;
        match assignment.sink {
            PhysicalSink::InstanceInput { instance: sink, .. } => {
                if structural_edges.insert((source, sink)) {
                    predecessors
                        .get_mut(&sink)
                        .expect("known sink has predecessor storage")
                        .insert(source);
                    successors
                        .get_mut(&source)
                        .expect("known source has successor storage")
                        .insert(sink);
                }
            }
            PhysicalSink::DeclaredOutput(_) => {
                declared_output_drivers.insert(source);
            }
        }
    }

    let mut indegree = predecessors
        .iter()
        .map(|(&id, incoming)| (id, incoming.len()))
        .collect::<BTreeMap<_, _>>();
    let mut ready = indegree
        .iter()
        .filter_map(|(&id, &degree)| (degree == 0).then_some(id))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(ids.len());
    while let Some(&next) = ready.iter().next() {
        ready.remove(&next);
        order.push(next);
        for &successor in &successors[&next] {
            let remaining = indegree
                .get_mut(&successor)
                .expect("known successor has an indegree");
            *remaining -= 1;
            if *remaining == 0 {
                ready.insert(successor);
            }
        }
    }
    if order.len() != ids.len() {
        let instances = indegree
            .into_iter()
            .filter_map(|(id, degree)| (degree > 0).then_some(id))
            .collect();
        return Err(SeedPlacementError::DependencyCycle { instances });
    }

    let instance_delays = graph
        .instances
        .iter()
        .map(|instance| {
            topology_delay_ticks(instance.id, &instance.expanded.topology)
                .map(|delay| (instance.id, delay))
        })
        .chain(graph.blocks.iter().map(|block| {
            block_delays
                .get(&block.id)
                .copied()
                .map(|delay| (block.id, delay))
                .ok_or(SeedPlacementError::UnresolvedTopology { instance: block.id })
        }))
        .collect::<Result<BTreeMap<_, _>, _>>()?;

    let mut forward_levels = BTreeMap::<InstanceId, u64>::new();
    let mut head_ticks = BTreeMap::<InstanceId, u64>::new();
    for &id in &order {
        let forward_level = predecessors[&id]
            .iter()
            .map(|predecessor| forward_levels[predecessor] + 1)
            .max()
            .unwrap_or(0);
        let upstream_ticks = predecessors[&id]
            .iter()
            .map(|predecessor| head_ticks[predecessor])
            .max()
            .unwrap_or(0);
        let head = upstream_ticks
            .checked_add(instance_delays[&id])
            .ok_or(SeedPlacementError::TimingOverflow)?;
        forward_levels.insert(id, forward_level);
        head_ticks.insert(id, head);
    }

    let critical_delay_ticks = declared_output_drivers
        .iter()
        .map(|driver| head_ticks[driver])
        .max()
        .unwrap_or(0);
    let mut reverse_levels = BTreeMap::<InstanceId, u64>::new();
    let mut tail_ticks = BTreeMap::<InstanceId, u64>::new();
    let mut reaches_declared_output = BTreeSet::new();
    for &id in order.iter().rev() {
        let mut reverse_level = declared_output_drivers.contains(&id).then_some(0);
        let mut downstream_ticks = declared_output_drivers.contains(&id).then_some(0);
        for successor in &successors[&id] {
            if !reaches_declared_output.contains(successor) {
                continue;
            }
            reverse_level = Some(
                reverse_level.unwrap_or(0).max(
                    reverse_levels[successor]
                        .checked_add(1)
                        .ok_or(SeedPlacementError::TimingOverflow)?,
                ),
            );
            downstream_ticks = Some(downstream_ticks.unwrap_or(0).max(tail_ticks[successor]));
        }
        let reaches_output = reverse_level.is_some();
        let tail = instance_delays[&id]
            .checked_add(downstream_ticks.unwrap_or(0))
            .ok_or(SeedPlacementError::TimingOverflow)?;
        reverse_levels.insert(id, reverse_level.unwrap_or(0));
        tail_ticks.insert(id, tail);
        if reaches_output {
            reaches_declared_output.insert(id);
        }
    }

    let nodes = ids
        .iter()
        .copied()
        .map(|id| {
            (
                id,
                NodeFacts {
                    predecessors: predecessors[&id].iter().copied().collect(),
                    successors: successors[&id].iter().copied().collect(),
                    forward_level: forward_levels[&id],
                    reverse_level: reverse_levels[&id],
                    head_ticks: head_ticks[&id],
                    tail_ticks: tail_ticks[&id],
                    // Topology says nothing about height; the placer is
                    // the only thing that moves a macro off the base deck.
                    deck: DeckId(0),
                },
            )
        })
        .collect();
    let edges = structural_edges
        .into_iter()
        .map(|(source, sink)| {
            let path_ticks = head_ticks[&source]
                .checked_add(tail_ticks[&sink])
                .ok_or(SeedPlacementError::TimingOverflow)?;
            Ok(EdgeFacts {
                source,
                sink,
                structural_slack_ticks: critical_delay_ticks.saturating_sub(path_ticks),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(SeedPlacementAnalysis {
        order,
        nodes,
        edges,
        critical_delay_ticks,
    })
}

fn instance_driver_owner(driver: &InstanceDriver) -> InstanceId {
    match driver {
        InstanceDriver::Primitive { logical_owner, .. }
        | InstanceDriver::Junction { logical_owner, .. } => *logical_owner,
    }
}

fn require_instance(
    ids: &BTreeSet<InstanceId>,
    instance: InstanceId,
) -> Result<(), SeedPlacementError> {
    if ids.contains(&instance) {
        Ok(())
    } else {
        Err(SeedPlacementError::UnknownInstance { instance })
    }
}

fn topology_delay_ticks(
    instance: InstanceId,
    topology: &ValidatedTopology,
) -> Result<u64, SeedPlacementError> {
    let mut delays = BTreeMap::<PrimitiveId, u64>::new();
    while delays.len() < topology.primitives.len() {
        let mut progressed = false;
        for specification in &topology.primitives {
            if delays.contains_key(&specification.id) {
                continue;
            }
            let mut upstream = 0;
            let mut unresolved = false;
            for connection in topology.connections.iter().filter(|connection| {
                connection.target == ConnectionTarget::Primitive(specification.id)
            }) {
                match connection.source {
                    ConnectionSource::ExternalInput { .. } => {}
                    ConnectionSource::Primitive(source) => {
                        let Some(&delay) = delays.get(&source) else {
                            unresolved = true;
                            break;
                        };
                        upstream = upstream.max(delay);
                    }
                }
            }
            if unresolved {
                continue;
            }
            let delay = upstream
                .checked_add(primitive_delay_ticks(specification.primitive))
                .ok_or(SeedPlacementError::TimingOverflow)?;
            delays.insert(specification.id, delay);
            progressed = true;
        }
        if !progressed {
            return Err(SeedPlacementError::UnresolvedTopology { instance });
        }
    }

    match &topology.output {
        OutputSpec::Primitive(primitive) => delays
            .get(primitive)
            .copied()
            .ok_or(SeedPlacementError::UnresolvedTopology { instance }),
        OutputSpec::Junction { contributors, .. } => contributors
            .iter()
            .map(|contributor| match *contributor {
                ContributorSpec::Primitive(primitive) => delays.get(&primitive).copied(),
                ContributorSpec::Landing(connection) => topology
                    .connections
                    .iter()
                    .find(|candidate| candidate.id == connection)
                    .and_then(|connection| match connection.source {
                        ConnectionSource::ExternalInput { .. } => Some(0),
                        ConnectionSource::Primitive(primitive) => delays.get(&primitive).copied(),
                    }),
            })
            .collect::<Option<Vec<_>>>()
            .map(|delays| delays.into_iter().max().unwrap_or(0))
            .ok_or(SeedPlacementError::UnresolvedTopology { instance }),
    }
}

const fn primitive_delay_ticks(primitive: Primitive) -> u64 {
    match primitive {
        Primitive::Torch => TORCH_DELAY_GAME_TICKS,
        Primitive::Repeater => REPEATER_GAME_TICKS_PER_REDSTONE_TICK,
        Primitive::Comparator => COMPARATOR_DELAY_GAME_TICKS,
        Primitive::Lever | Primitive::Lamp => 0,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinned_layouts_shift_inside_the_lateral_window_in_grid_steps() {
        use super::*;
        // Lateral runs west from a pin at x = 21, so the world edge caps the
        // laterals at 21: the macros at laterals 0..=35 move down until the
        // window margin fits, by whole grid steps.
        let frame = PlacementFrame {
            forward: Facing::South,
            lateral: Facing::West,
            origin: Anchor { x: 21, y: 1, z: 62 },
        };
        let window = lateral_window(frame, &BTreeMap::new(), None);
        assert_eq!(
            window,
            LateralWindow {
                min: None,
                max: Some(21)
            }
        );
        let bounds = MacroBounds {
            min_forward: 0,
            max_forward: 3,
            min_lateral: -1,
            max_lateral: 3,
            min_y: 0,
            max_y: 0,
        };
        let ids = [InstanceId(0), InstanceId(1)];
        let bounds = ids
            .iter()
            .map(|&id| (id, bounds))
            .collect::<BTreeMap<_, _>>();
        let mut laterals = BTreeMap::from([(ids[0], 0), (ids[1], 32)]);

        let shift = confine_laterals(window, &bounds, &[16], &mut laterals).unwrap();

        // extent_max = 32 + 3 = 35 must come down to 21 - 8 = 13: a shortfall
        // of 22 rounds up to six grid steps of four.
        assert_eq!(shift, -24);
        assert_eq!(laterals[&ids[0]], -24);
        assert_eq!(laterals[&ids[1]], 8);

        // Lateral running east from the same pin: the window starts at -21,
        // extent_min = -1 already clears the margin, so nothing moves.
        let frame = PlacementFrame {
            lateral: Facing::East,
            ..frame
        };
        let window = lateral_window(frame, &BTreeMap::new(), None);
        assert_eq!(
            window,
            LateralWindow {
                min: Some(-21),
                max: None
            }
        );
        let mut laterals = BTreeMap::from([(ids[0], 0), (ids[1], 32)]);
        assert_eq!(
            confine_laterals(window, &bounds, &[16], &mut laterals).unwrap(),
            0
        );
        assert_eq!(laterals[&ids[1]], 32);
    }

    #[test]
    fn a_pinned_input_facing_along_the_lateral_axis_caps_the_window() {
        use super::*;
        // Forward east, lateral south: an input at z = 120 whose signal
        // enters northward keeps every block at z <= 119, i.e. lateral <= 1
        // from the origin at z = 118; the world edge gives the other side.
        let frame = PlacementFrame {
            forward: Facing::East,
            lateral: Facing::South,
            origin: Anchor {
                x: 94,
                y: 1,
                z: 118,
            },
        };
        let pins = BTreeMap::from([(
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            PortPin {
                at: Anchor {
                    x: 76,
                    y: 1,
                    z: 120,
                },
                toward: Facing::North,
            },
        )]);
        assert_eq!(
            lateral_window(frame, &pins, None),
            LateralWindow {
                min: Some(-118),
                max: Some(1)
            }
        );
        // The same pin against the forward axis bounds the forward extent.
        let frame = PlacementFrame {
            forward: Facing::South,
            lateral: Facing::West,
            ..frame
        };
        assert_eq!(forward_limit(frame, &pins, None), Some(1));
        let frame = PlacementFrame {
            forward: Facing::North,
            lateral: Facing::East,
            ..frame
        };
        assert_eq!(forward_limit(frame, &pins, None), Some(118));
    }

    #[test]
    fn complete_pins_close_both_placement_axes() {
        use super::*;
        // Every declared port pinned draws one 41 x 41 rectangle, and that
        // rectangle -- not only the world edge -- is what the layout must
        // stay inside, on the lateral axis and ahead of the last column
        // alike.  A wire-through board has no level to fit between the pin
        // lines, so it is the bounds themselves this measures.
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["a".into()],
            gates: vec![],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let facts = BTreeMap::new();
        let board = |output_x: i32| {
            BTreeMap::from([
                (
                    PhysicalEndpointId::PrimaryInput(PortId(0)),
                    pin(Anchor { x: 10, y: 1, z: 20 }, Facing::East),
                ),
                (
                    PhysicalEndpointId::PrimaryInput(PortId(1)),
                    pin(Anchor { x: 10, y: 1, z: 60 }, Facing::East),
                ),
                (
                    PhysicalEndpointId::DeclaredOutput(PortId(0)),
                    pin(
                        Anchor {
                            x: output_x,
                            y: 1,
                            z: 40,
                        },
                        Facing::East,
                    ),
                ),
            ])
        };
        let complete = board(50);
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &complete,
                block_facts: &facts,
            })
            .expect("a complete board with no levels plans");

        assert_eq!(
            plan.io_footprint,
            Some(IoFootprint {
                min_x: 10,
                max_x: 50,
                min_z: 20,
                max_z: 60
            })
        );
        assert_eq!(plan.window.width(), Some(41));

        // Both pin lines on one X: the rectangle has no interior to place
        // in, which is a refusal rather than a zero-width window.
        let degenerate = board(10);
        assert!(matches!(
            TopologyAwareSeedPlacer.plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &degenerate,
                block_facts: &facts,
            }),
            Err(SeedPlacementError::DegenerateIoFootprint { .. })
        ));

        // The same rectangle is too short for even one level and its
        // channels: the forward axis is closed now, where before only the
        // world edge and a pinned input facing back could close it.  The
        // levels used to be refused as a frame that does not fit; now they
        // are offered to the decks first, and the first column alone is
        // what the board cannot hold.
        //
        // Forward runs south from the input's net cell at z = 20 to the
        // board's far wall at z = 60, so the limit is 40 and the first
        // column starts at `0 + 1 + 11 = 12`: a capacity of 28.  A
        // south-facing torch spans two forward cells, its channel is
        // `channel_width(1) + 2 = 11`, and the deck's turnaround is
        // `9 + 8 = 17`: `2 + 11 + 17 = 30` does not fit on any deck.
        let levels = two_stage_graph();
        let level_analysis = analyse_instance_dag(&levels, &BTreeMap::new()).unwrap();
        let corners = BTreeMap::from([
            (
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                pin(Anchor { x: 10, y: 1, z: 20 }, Facing::East),
            ),
            (
                PhysicalEndpointId::DeclaredOutput(PortId(0)),
                pin(Anchor { x: 50, y: 1, z: 60 }, Facing::East),
            ),
        ]);
        assert_eq!(
            TopologyAwareSeedPlacer.plan(SeedPlacementRequest {
                graph: &levels,
                analysis: &level_analysis,
                pins: &corners,
                block_facts: &facts,
            }),
            Err(SeedPlacementError::LateralWindowTooNarrow {
                instance: InstanceId(0)
            })
        );

        // A partial set draws no rectangle: one pin of two ports bounds
        // nothing, and no pin at all leaves the pre-bounded plan literally
        // unchanged.
        let partial = BTreeMap::from([(
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            pin(Anchor { x: 20, y: 1, z: 40 }, Facing::North),
        )]);
        let partial_plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &levels,
                analysis: &level_analysis,
                pins: &partial,
                block_facts: &facts,
            })
            .expect("a partially pinned board still plans");
        assert_eq!(partial_plan.io_footprint, None);

        let unpinned = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &levels,
                analysis: &level_analysis,
                pins: &BTreeMap::new(),
                block_facts: &facts,
            })
            .expect("an unpinned board still plans");
        assert_eq!(unpinned.io_footprint, None);
        assert_eq!(
            unpinned.fingerprint.as_str(),
            "24ef3d8e581982f52eeb7a40a6763ae2e8ad2493553aa4851fe309e55000bc93"
        );
    }

    use std::collections::{BTreeMap, BTreeSet};

    use crate::compile::fragment_synth::identity::{
        GateIndex, ImplementationKey, InputMask, InstanceId, PhysicalEndpointId, PortId,
        PrimitiveId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::instance_graph::{
        DuplicateRequest, InstanceGraph, LogicalSignalId, PhysicalDriver, PhysicalSink,
    };
    use crate::compile::geometry::{Anchor, CellFacing};
    use crate::compile::planner::{PortPin, PortRole};
    use crate::compile::topology::{EmbeddingHint, Primitive, TemplateNode};
    use crate::compile::topology::{GateKind, Library};
    use crate::compile::{Gate, Netlist};
    use crate::redstone::simulator::position::Position;
    use crate::redstone::world::block::Facing;

    use super::{
        analyse_instance_dag, choose_instance_facing, colour_intervals, derive_frame, hint_penalty,
        legalize_laterals, BlockFacts, EdgeFacts, LayoutOwner, LayoutRepair, MacroBounds,
        NetInterval, SeedPlacementError, SeedPlacementRequest, SeedPlacer, TopologyAwareSeedPlacer,
        TRACK_PITCH,
    };

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate::nor(output, inputs)
    }

    fn two_stage_graph() -> InstanceGraph {
        InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("middle", &["a"]), nor("y", &["middle"])],
            },
            &Library::default_library(),
        )
        .unwrap()
    }

    #[test]
    fn a_block_is_a_level_node_with_its_certified_delay_and_a_box_envelope() {
        use crate::compile::fragment_synth::instance_graph::tests::{
            planning_with_one_block, specs_of,
        };
        let (planning, owned) = planning_with_one_block();
        let graph =
            InstanceGraph::with_blocks(&planning, &Library::default_library(), &specs_of(&owned))
                .unwrap();
        let block = graph.blocks[0].id;
        let delays = BTreeMap::from([(block, 37u64)]);
        let analysis = analyse_instance_dag(&graph, &delays).expect("analyses");
        assert_eq!(analysis.nodes[&block].forward_level, 0);
        assert_eq!(analysis.nodes[&InstanceId(0)].forward_level, 1);
        assert_eq!(analysis.nodes[&block].head_ticks, 37);
        assert!(analysis.nodes[&InstanceId(0)].head_ticks > 37);
        let facts = BTreeMap::from([(
            block,
            BlockFacts {
                width: 30,
                depth: 20,
                height: 5,
                delay_ticks: 37,
            },
        )]);
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &BTreeMap::new(),
                block_facts: &facts,
            })
            .expect("plans");
        let pose = plan.instances[&block];
        assert_eq!(pose.facing, CellFacing::EAST);
        let gate = plan.instances[&InstanceId(0)];
        assert!(
            gate.preferred_origin.x >= pose.preferred_origin.x + 30,
            "the gate's column starts after the block's width plus a channel"
        );
    }

    fn primitive_source(instance: u32) -> PhysicalEndpointId {
        PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(instance),
            node: TopologyNodeId(0),
        })
    }

    fn landing(instance: u32) -> PhysicalEndpointId {
        PhysicalEndpointId::Landing(
            crate::compile::fragment_synth::identity::ConnectionId::External {
                instance: InstanceId(instance),
                input_index: 0,
            },
        )
    }

    #[test]
    fn canonical_layout_repairs_move_exact_owners_and_bind_the_fingerprint() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
            block_facts: &facts,
        };
        let baseline = TopologyAwareSeedPlacer.plan(request).unwrap();
        assert_eq!(
            baseline,
            TopologyAwareSeedPlacer
                .plan_with_repairs(request, &[])
                .unwrap()
        );

        let exclusive = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::ExclusiveGuardedTrack {
                    source: primitive_source(0),
                }],
            )
            .unwrap();
        assert_eq!(
            exclusive.instances[&InstanceId(0)].preferred_origin.z,
            baseline.instances[&InstanceId(0)].preferred_origin.z + 2 * TRACK_PITCH
        );
        assert_eq!(
            exclusive.instances[&InstanceId(1)],
            baseline.instances[&InstanceId(1)]
        );
        assert_ne!(exclusive.fingerprint, baseline.fingerprint);

        let early = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::EarlyTreeSinkAndEscape {
                    source: primitive_source(0),
                    sink: landing(1),
                }],
            )
            .unwrap();
        assert_eq!(
            early.instances[&InstanceId(0)].preferred_origin.z,
            baseline.instances[&InstanceId(0)].preferred_origin.z + 2 * TRACK_PITCH
        );
        assert_ne!(early.fingerprint, exclusive.fingerprint);

        let separated = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::SeparateOwners {
                    source_owner: LayoutOwner::Instance(InstanceId(0)),
                    sink_owner: LayoutOwner::Instance(InstanceId(1)),
                }],
            )
            .unwrap();
        assert_eq!(
            separated.instances[&InstanceId(1)].preferred_origin.z,
            baseline.instances[&InstanceId(1)].preferred_origin.z + TRACK_PITCH
        );
        assert_eq!(
            separated.instances[&InstanceId(0)],
            baseline.instances[&InstanceId(0)]
        );
    }

    /// A repair move is a post-plan movement like any other: the board a
    /// complete pin set drew bounds it too.  An owner whose track step
    /// would leave the board is immovable -- the same answer an already
    /// pinned boundary gives -- so the existing fallback and
    /// `ImmovableRepairOwner` paths carry the refusal unchanged, and the
    /// pose it refused is left exactly where it stood.
    #[test]
    fn bounded_post_plan_moves_never_cross_the_io_footprint_at_a_repair_move() {
        use super::*;
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let legacy = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &pins,
                block_facts: &facts,
            })
            .unwrap();
        let owner = LayoutOwner::Instance(InstanceId(0));
        let origin = legacy.instances[&InstanceId(0)].preferred_origin;
        // A board that ends one cell short of where a two-track repair
        // would put this instance, and a plan that carries it.
        let board = IoFootprint {
            min_x: origin.x - 64,
            max_x: origin.x + 64,
            min_z: origin.z - 64,
            max_z: origin.z + 2 * TRACK_PITCH - 1,
        };
        let bounded = || SeedPlacementPlan {
            io_footprint: Some(board),
            ..legacy.clone()
        };

        let mut plan = bounded();
        assert_eq!(
            move_owner(&mut plan, owner, &pins, Facing::South, 2 * TRACK_PITCH),
            Ok(false)
        );
        assert_eq!(
            plan.instances[&InstanceId(0)].preferred_origin,
            origin,
            "a refused move leaves the pose untouched"
        );

        let mut plan = bounded();
        assert_eq!(
            require_move_owner(&mut plan, owner, &pins, Facing::South, 2 * TRACK_PITCH),
            Err(SeedPlacementError::ImmovableRepairOwner { owner })
        );

        // One track still lands on the board, and still moves.
        let mut plan = bounded();
        require_move_owner(&mut plan, owner, &pins, Facing::South, TRACK_PITCH).unwrap();
        assert_eq!(
            plan.instances[&InstanceId(0)].preferred_origin,
            Anchor {
                z: origin.z + TRACK_PITCH,
                ..origin
            }
        );

        // A partial or unpinned set draws no board, and the same two-track
        // move is the legacy one.
        let mut plan = legacy.clone();
        assert_eq!(plan.io_footprint, None);
        require_move_owner(&mut plan, owner, &pins, Facing::South, 2 * TRACK_PITCH).unwrap();
        assert_eq!(
            plan.instances[&InstanceId(0)].preferred_origin,
            Anchor {
                z: origin.z + 2 * TRACK_PITCH,
                ..origin
            }
        );
    }

    #[test]
    fn repair_order_is_canonical_and_same_owner_separation_is_named() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
            block_facts: &facts,
        };
        let first = LayoutRepair::ExclusiveGuardedTrack {
            source: primitive_source(0),
        };
        let second = LayoutRepair::SeparateOwners {
            source_owner: LayoutOwner::Instance(InstanceId(0)),
            sink_owner: LayoutOwner::Instance(InstanceId(1)),
        };
        assert_eq!(
            TopologyAwareSeedPlacer
                .plan_with_repairs(request, &[first, second])
                .unwrap(),
            TopologyAwareSeedPlacer
                .plan_with_repairs(request, &[second, first])
                .unwrap()
        );
        assert_eq!(
            TopologyAwareSeedPlacer.plan_with_repairs(
                request,
                &[LayoutRepair::SeparateOwners {
                    source_owner: LayoutOwner::Instance(InstanceId(0)),
                    sink_owner: LayoutOwner::Instance(InstanceId(0)),
                }],
            ),
            Err(SeedPlacementError::SameRepairOwner {
                owner: LayoutOwner::Instance(InstanceId(0))
            })
        );
    }

    #[test]
    fn pinned_boundary_repair_keeps_the_pin_and_moves_the_sink_owner() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let input = PhysicalEndpointId::PrimaryInput(PortId(0));
        let pins = BTreeMap::from([(input, pin(Anchor { x: 20, y: 1, z: 40 }, Facing::North))]);
        let facts = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
            block_facts: &facts,
        };
        let baseline = TopologyAwareSeedPlacer.plan(request).unwrap();
        let repaired = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::EarlyTreeSinkAndEscape {
                    source: input,
                    sink: landing(0),
                }],
            )
            .unwrap();

        assert!(!baseline.automatic_inputs.contains_key(&PortId(0)));
        assert!(!repaired.automatic_inputs.contains_key(&PortId(0)));
        assert_eq!(
            pins[&input],
            pin(Anchor { x: 20, y: 1, z: 40 }, Facing::North)
        );
        // The pin line at z = 40 leaves no forward room to the north, so the
        // frame turns east; the repair moves the sink owner two tracks along
        // whatever lateral axis the plan settled on.
        assert_eq!(repaired.frame, baseline.frame);
        let lateral = |plan: &super::SeedPlacementPlan| {
            let origin = plan.instances[&InstanceId(0)].preferred_origin;
            super::project_horizontal(origin.x, origin.z, plan.frame.lateral)
        };
        assert_eq!(lateral(&repaired), lateral(&baseline) + 2 * TRACK_PITCH);
    }

    #[test]
    fn dependency_order_ignores_reversed_gate_declaration_order() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("y", &["produced_later"]), nor("produced_later", &["a"])],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();

        assert_eq!(facts.nodes[&InstanceId(1)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 1);
        assert_eq!(facts.order, [InstanceId(1), InstanceId(0)]);
    }

    #[test]
    fn concrete_instance_fanout_populates_ordered_successors() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                nor("shared", &["a"]),
                nor("right", &["shared"]),
                nor("left", &["shared"]),
            ],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();

        assert_eq!(
            facts.nodes[&InstanceId(0)].successors,
            [InstanceId(1), InstanceId(2)]
        );
        assert_eq!(facts.nodes[&InstanceId(1)].predecessors, [InstanceId(0)]);
        assert_eq!(facts.nodes[&InstanceId(2)].predecessors, [InstanceId(0)]);
    }

    #[test]
    fn fanout_levels_and_structural_slack_use_literal_longest_paths() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![
                nor("long_0", &["a"]),
                nor("long_1", &["long_0"]),
                nor("short", &["a"]),
                nor("y", &["long_1", "short"]),
            ],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();

        assert_eq!(
            facts.order,
            [InstanceId(0), InstanceId(1), InstanceId(2), InstanceId(3)]
        );
        assert_eq!(facts.nodes[&InstanceId(0)].predecessors, []);
        assert_eq!(facts.nodes[&InstanceId(0)].successors, [InstanceId(1)]);
        assert_eq!(
            facts.nodes[&InstanceId(3)].predecessors,
            [InstanceId(1), InstanceId(2)]
        );
        assert_eq!(facts.nodes[&InstanceId(3)].successors, []);
        assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(1)].forward_level, 1);
        assert_eq!(facts.nodes[&InstanceId(2)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(3)].forward_level, 2);
        assert_eq!(facts.nodes[&InstanceId(0)].reverse_level, 2);
        assert_eq!(facts.nodes[&InstanceId(1)].reverse_level, 1);
        assert_eq!(facts.nodes[&InstanceId(2)].reverse_level, 1);
        assert_eq!(facts.nodes[&InstanceId(3)].reverse_level, 0);
        assert_eq!(facts.nodes[&InstanceId(0)].head_ticks, 2);
        assert_eq!(facts.nodes[&InstanceId(1)].head_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(2)].head_ticks, 2);
        assert_eq!(facts.nodes[&InstanceId(3)].head_ticks, 6);
        assert_eq!(facts.nodes[&InstanceId(0)].tail_ticks, 6);
        assert_eq!(facts.nodes[&InstanceId(1)].tail_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(2)].tail_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(3)].tail_ticks, 2);
        assert_eq!(facts.critical_delay_ticks, 6);
        assert_eq!(
            facts.edges,
            [
                EdgeFacts {
                    source: InstanceId(0),
                    sink: InstanceId(1),
                    structural_slack_ticks: 0,
                },
                EdgeFacts {
                    source: InstanceId(1),
                    sink: InstanceId(3),
                    structural_slack_ticks: 0,
                },
                EdgeFacts {
                    source: InstanceId(2),
                    sink: InstanceId(3),
                    structural_slack_ticks: 2,
                },
            ]
        );
    }

    #[test]
    fn selected_topology_primitives_supply_cell_only_delay() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate {
                name: "y".into(),
                inputs: vec!["a".into()],
                output: "y".into(),
                kind: GateKind::Buf,
            }],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();

        assert_eq!(facts.nodes[&InstanceId(0)].head_ticks, 4);
        assert_eq!(facts.nodes[&InstanceId(0)].tail_ticks, 4);
        assert_eq!(facts.critical_delay_ticks, 4);
    }

    #[test]
    fn duplicate_instances_remain_independent_dependency_nodes() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                nor("shared", &["a"]),
                nor("left", &["shared"]),
                nor("right", &["shared"]),
            ],
        };
        let graph = InstanceGraph::with_variants(
            &netlist,
            &Library::default_library(),
            &BTreeMap::new(),
            &[DuplicateRequest {
                canonical: InstanceId(0),
                ordinal: 1,
                sinks: BTreeSet::from([PhysicalSink::InstanceInput {
                    instance: InstanceId(2),
                    input_index: 0,
                }]),
            }],
        )
        .unwrap();

        let facts = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();

        assert_eq!(
            facts.order,
            [InstanceId(0), InstanceId(1), InstanceId(3), InstanceId(2)]
        );
        assert_eq!(facts.nodes[&InstanceId(0)].successors, [InstanceId(1)]);
        assert_eq!(facts.nodes[&InstanceId(3)].successors, [InstanceId(2)]);
        assert_eq!(facts.nodes[&InstanceId(0)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(3)].forward_level, 0);
        assert_eq!(facts.nodes[&InstanceId(0)].reverse_level, 1);
        assert_eq!(facts.nodes[&InstanceId(3)].reverse_level, 1);
    }

    #[test]
    fn malformed_primary_input_sink_names_unknown_instance_is_rejected() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("y", &["a"])],
        };
        let mut graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let primary_input_assignment = graph
            .assignments
            .iter_mut()
            .find(|assignment| matches!(assignment.driver, PhysicalDriver::PrimaryInput(_)))
            .unwrap();
        primary_input_assignment.sink = PhysicalSink::InstanceInput {
            instance: InstanceId(99),
            input_index: 0,
        };

        assert_eq!(
            analyse_instance_dag(&graph, &BTreeMap::new()),
            Err(SeedPlacementError::UnknownInstance {
                instance: InstanceId(99),
            })
        );
    }

    #[test]
    fn malformed_instance_dependency_cycle_is_rejected() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("middle", &["a"]), nor("y", &["middle"])],
        };
        let mut cycle = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let back_edge_driver = cycle
            .assignments
            .iter()
            .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(PortId(0)))
            .unwrap()
            .driver
            .clone();
        let first_input = cycle
            .assignments
            .iter_mut()
            .find(|assignment| {
                assignment.sink
                    == PhysicalSink::InstanceInput {
                        instance: InstanceId(0),
                        input_index: 0,
                    }
            })
            .unwrap();
        first_input.signal = LogicalSignalId::GateOutput(GateIndex(1));
        first_input.driver = back_edge_driver;

        assert!(matches!(
            analyse_instance_dag(&cycle, &BTreeMap::new()),
            Err(SeedPlacementError::DependencyCycle { .. })
        ));
    }

    fn pin(at: Anchor, toward: Facing) -> PortPin {
        PortPin { at, toward }
    }

    #[test]
    fn placement_frame_covers_no_both_input_only_and_output_only_pins() {
        assert_eq!(derive_frame(&BTreeMap::new()).forward, Facing::East);

        let both = BTreeMap::from([
            (
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                pin(Anchor { x: 8, y: 1, z: 120 }, Facing::North),
            ),
            (
                PhysicalEndpointId::DeclaredOutput(PortId(0)),
                pin(Anchor { x: 8, y: 1, z: 24 }, Facing::North),
            ),
        ]);
        assert_eq!(derive_frame(&both).forward, Facing::North);
        assert_eq!(
            both[&PhysicalEndpointId::PrimaryInput(PortId(0))].net_cell(PortRole::Input),
            Anchor { x: 8, y: 1, z: 118 }
        );
        assert_eq!(
            both[&PhysicalEndpointId::DeclaredOutput(PortId(0))].net_cell(PortRole::Output),
            Anchor { x: 8, y: 1, z: 26 }
        );

        let inputs = BTreeMap::from([
            (
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                pin(Anchor { x: 0, y: 1, z: 0 }, Facing::North),
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(1)),
                pin(Anchor { x: 2, y: 1, z: 0 }, Facing::North),
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(2)),
                pin(Anchor { x: 4, y: 1, z: 0 }, Facing::East),
            ),
        ]);
        assert_eq!(derive_frame(&inputs).forward, Facing::North);

        let outputs = BTreeMap::from([
            (
                PhysicalEndpointId::DeclaredOutput(PortId(0)),
                pin(Anchor { x: 0, y: 1, z: 0 }, Facing::South),
            ),
            (
                PhysicalEndpointId::DeclaredOutput(PortId(1)),
                pin(Anchor { x: 2, y: 1, z: 0 }, Facing::South),
            ),
            (
                PhysicalEndpointId::DeclaredOutput(PortId(2)),
                pin(Anchor { x: 4, y: 1, z: 0 }, Facing::West),
            ),
        ]);
        assert_eq!(derive_frame(&outputs).forward, Facing::North);
    }

    #[test]
    fn interval_colouring_separates_overlaps_and_reuses_disjoint_tracks() {
        let intervals = [
            NetInterval {
                signal: LogicalSignalId::GateOutput(GateIndex(0)),
                start: 0,
                end: 3,
                fanout: 2,
                slack: 0,
            },
            NetInterval {
                signal: LogicalSignalId::GateOutput(GateIndex(1)),
                start: 1,
                end: 2,
                fanout: 1,
                slack: 2,
            },
            NetInterval {
                signal: LogicalSignalId::GateOutput(GateIndex(2)),
                start: 4,
                end: 5,
                fanout: 1,
                slack: 3,
            },
        ];

        let tracks = colour_intervals(&intervals);

        assert_eq!(tracks[&LogicalSignalId::GateOutput(GateIndex(0))], 0);
        assert_eq!(tracks[&LogicalSignalId::GateOutput(GateIndex(1))], 1);
        assert_eq!(tracks[&LogicalSignalId::GateOutput(GateIndex(2))], 0);
    }

    #[test]
    fn planner_torch_pose_uses_literal_actual_variant_ports() {
        let graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("y", &["a"])],
            },
            &Library::default_library(),
        )
        .unwrap();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &pins,
                block_facts: &facts,
            })
            .unwrap();
        let pose = plan.instances[&InstanceId(0)];
        let variant =
            &crate::compile::physical::variants(Primitive::Torch)[usize::from(pose.facing.index())];
        let input = variant.port(crate::compile::physical::PortKind::TorchInput);
        let output = variant.port(crate::compile::physical::PortKind::TorchOutput);

        assert_eq!(pose.facing, CellFacing::EAST);
        assert_eq!(pose.preferred_origin, Anchor { x: 0, y: 1, z: 8 });
        assert_eq!(input.position, Position::new(0, 0, 0));
        assert_eq!(output.position, Position::new(1, 0, 0));
        assert_eq!(
            input.position.offset(input.direction),
            Position::new(-1, 0, 0)
        );
        assert_eq!(
            output.position.offset(output.direction),
            Position::new(2, 0, 0)
        );
        assert_eq!(
            super::input_terminal(Primitive::Torch, pose.facing, pose.preferred_origin),
            Anchor { x: -1, y: 1, z: 8 }
        );
        assert_eq!(
            super::output_terminal(Primitive::Torch, pose.facing, pose.preferred_origin),
            Anchor { x: 2, y: 1, z: 8 }
        );
    }

    #[test]
    fn planner_repeater_pose_uses_literal_actual_rear_and_front_ports() {
        let library = Library::default_library();
        let graph = InstanceGraph::with_variants(
            &Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into()],
                gates: vec![Gate::merge("y", &["a", "b"])],
            },
            &library,
            &BTreeMap::from([(
                InstanceId(0),
                ImplementationKey::Merge {
                    isolation_mask: InputMask::new(0b01),
                },
            )]),
            &[],
        )
        .unwrap();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &pins,
                block_facts: &facts,
            })
            .unwrap();
        let pose = plan.instances[&InstanceId(0)];
        let variant = &crate::compile::physical::variants(Primitive::Repeater)
            [usize::from(pose.facing.index())];
        let rear = variant.port(crate::compile::physical::PortKind::RepeaterRear);
        let front = variant.port(crate::compile::physical::PortKind::RepeaterFront);

        // The merge's junction output leaves forward, so the macro takes the
        // east cell facing.  The literal repeater ports of that variant are
        // what the seed will orient its isolating repeaters against.
        assert_eq!(pose.facing, CellFacing::EAST);
        assert_eq!(pose.preferred_origin, Anchor { x: 0, y: 1, z: 8 });
        assert_eq!(rear.position, Position::new(0, 0, 0));
        assert_eq!(front.position, Position::new(0, 0, 0));
        assert_eq!(rear.position.offset(rear.direction), Position::new(1, 0, 0));
        assert_eq!(
            front.position.offset(front.direction),
            Position::new(-1, 0, 0)
        );
        assert_eq!(
            super::input_terminal(Primitive::Repeater, pose.facing, pose.preferred_origin),
            Anchor { x: 1, y: 1, z: 8 }
        );
        assert_eq!(
            super::output_terminal(Primitive::Repeater, pose.facing, pose.preferred_origin),
            Anchor { x: -1, y: 1, z: 8 }
        );
    }

    #[test]
    fn embedding_hint_penalty_changes_the_expected_pose() {
        let nodes = BTreeMap::from([
            (TemplateNode::Torch, Position::new(0, 0, 0)),
            (TemplateNode::SecondTorch, Position::new(4, 0, 0)),
        ]);
        let hint = EmbeddingHint::Coplanar(TemplateNode::Torch, TemplateNode::SecondTorch);

        assert_eq!(
            hint_penalty(&nodes, &[], CellFacing::NORTH, Facing::East),
            0
        );
        assert!(hint_penalty(&nodes, &[hint], CellFacing::NORTH, Facing::East) > 0);
        assert_eq!(
            hint_penalty(&nodes, &[hint], CellFacing::EAST, Facing::East),
            0
        );
        let origin = Anchor { x: 0, y: 1, z: 0 };
        let source = Anchor { x: -4, y: 1, z: 4 };
        let target = Anchor { x: 4, y: 1, z: -4 };
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate {
                name: "y".into(),
                inputs: vec!["a".into()],
                output: "y".into(),
                kind: GateKind::Buf,
            }],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        // Only facings along the frame's forward axis are candidates: the
        // channel plan needs every socket row and anchor on the row grid.
        // The pose still follows the actual port geometry: a source behind
        // and a target ahead face the cell forward, the mirrored situation
        // faces it backward.
        let instance = &graph.instances[0];
        assert_eq!(
            choose_instance_facing(instance, origin, source, target, Facing::East).unwrap(),
            CellFacing::EAST
        );
        assert_eq!(
            choose_instance_facing(instance, origin, target, source, Facing::East).unwrap(),
            CellFacing::EAST
        );
        let mut with_hint = instance.clone();
        with_hint.expanded.topology.embedding_hints = vec![hint];
        assert_eq!(
            choose_instance_facing(&with_hint, origin, source, target, Facing::East).unwrap(),
            CellFacing::EAST
        );
    }

    #[test]
    fn complete_plans_and_fingerprints_repeat_exactly() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("middle", &["a"]), nor("y", &["middle"])],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let placer = TopologyAwareSeedPlacer;
        let request = || SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
            block_facts: &facts,
        };

        let first = placer.plan(request()).unwrap();
        let second = placer.plan(request()).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.fingerprint, second.fingerprint);
        assert!(
            first.instances[&InstanceId(1)].preferred_origin.x
                > first.instances[&InstanceId(0)].preferred_origin.x
        );
    }

    #[test]
    fn planner_consumes_the_injected_analysis_without_recomputing_the_dag() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("middle", &["a"]), nor("y", &["middle"])],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
        let mut injected = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        injected.order = vec![InstanceId(1), InstanceId(0)];
        injected
            .nodes
            .get_mut(&InstanceId(0))
            .unwrap()
            .forward_level = 0;
        injected
            .nodes
            .get_mut(&InstanceId(1))
            .unwrap()
            .forward_level = 0;
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();

        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &injected,
                pins: &pins,
                block_facts: &facts,
            })
            .unwrap();

        assert_eq!(
            plan.instances[&InstanceId(0)].preferred_origin.x,
            plan.instances[&InstanceId(1)].preferred_origin.x
        );
    }

    #[test]
    fn oriented_negative_bounds_legalize_actual_different_facing_footprints() {
        let library = Library::default_library();
        let torch_graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("y", &["a"])],
            },
            &library,
        )
        .unwrap();
        let repeater_graph = InstanceGraph::with_variants(
            &Netlist {
                inputs: vec!["a".into(), "b".into(), "c".into()],
                outputs: vec!["y".into()],
                gates: vec![Gate::merge("y", &["a", "b", "c"])],
            },
            &library,
            &BTreeMap::from([(
                InstanceId(0),
                ImplementationKey::Merge {
                    isolation_mask: InputMask::new(0b111),
                },
            )]),
            &[],
        )
        .unwrap();
        let torch = super::macro_envelope(&torch_graph.instances[0]).unwrap();
        let repeaters = super::macro_envelope(&repeater_graph.instances[0]).unwrap();
        let torch_bounds = torch.oriented_bounds(CellFacing::NORTH, Facing::East);
        let repeater_bounds = repeaters.oriented_bounds(CellFacing::SOUTH, Facing::East);

        assert_eq!(
            torch_bounds,
            MacroBounds {
                min_forward: 0,
                max_forward: 0,
                min_lateral: -1,
                max_lateral: 0,
                min_y: 0,
                max_y: 0,
            }
        );
        assert_eq!(
            repeater_bounds,
            MacroBounds {
                min_forward: 0,
                max_forward: 0,
                min_lateral: -8,
                max_lateral: 0,
                min_y: -1,
                max_y: 0,
            }
        );

        let origins = legalize_laterals(&[
            (InstanceId(0), 0, torch_bounds),
            (InstanceId(1), 0, repeater_bounds),
        ])
        .unwrap();

        assert_eq!(origins[&InstanceId(0)], 0);
        // 8 cells of repeater footprint plus the ten-cell gap would put the
        // origin at 18; the row grid snaps it up to 20.
        assert_eq!(origins[&InstanceId(1)], 20);
        let torch_cells = actual_block_footprint(
            &torch_graph.instances[0],
            CellFacing::NORTH,
            origins[&InstanceId(0)],
        );
        let repeater_cells = actual_block_footprint(
            &repeater_graph.instances[0],
            CellFacing::SOUTH,
            origins[&InstanceId(1)],
        );
        assert!(torch_cells.is_disjoint(&repeater_cells));
    }

    /// A macro is a box, not a rectangle: the deck it stands on has to know
    /// how tall it is.  Both heights here are read off the literal variants
    /// -- a wall torch is its support and the torch beside it, all on the
    /// ground row, while a repeater stands on a support one row below its
    /// own cell -- and a block's box is its certified bounds' own span.
    #[test]
    fn macro_envelopes_keep_their_real_vertical_bounds() {
        use super::*;
        let library = Library::default_library();
        let torch_graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("y", &["a"])],
            },
            &library,
        )
        .unwrap();
        let repeater_graph = InstanceGraph::with_variants(
            &Netlist {
                inputs: vec!["a".into(), "b".into(), "c".into()],
                outputs: vec!["y".into()],
                gates: vec![Gate::merge("y", &["a", "b", "c"])],
            },
            &library,
            &BTreeMap::from([(
                InstanceId(0),
                ImplementationKey::Merge {
                    isolation_mask: InputMask::new(0b111),
                },
            )]),
            &[],
        )
        .unwrap();

        // `TORCH_*_BLOCKS` and `TORCH_*_PORTS` put every cell on y = 0.
        let torch = macro_envelope(&torch_graph.instances[0]).unwrap();
        assert_eq!((torch.min_y, torch.max_y), (0, 0));
        assert_eq!(
            torch.oriented_bounds(CellFacing::NORTH, Facing::East),
            MacroBounds {
                min_forward: 0,
                max_forward: 0,
                min_lateral: -1,
                max_lateral: 0,
                min_y: 0,
                max_y: 0,
            }
        );
        assert_eq!(
            torch
                .oriented_bounds(CellFacing::NORTH, Facing::East)
                .height(),
            1
        );

        // `REPEATER_*_BLOCKS` carry their solid support at `DOWN`, so the
        // macro reaches one row below the row its diode sits on.
        let repeaters = macro_envelope(&repeater_graph.instances[0]).unwrap();
        assert_eq!((repeaters.min_y, repeaters.max_y), (-1, 0));
        let repeater_bounds = repeaters.oriented_bounds(CellFacing::SOUTH, Facing::East);
        assert_eq!((repeater_bounds.min_y, repeater_bounds.max_y), (-1, 0));
        assert_eq!(repeater_bounds.height(), 2);

        // A block keeps its certified box: `height` cells up from its own
        // floor, exactly as `width`/`depth` run from its own west/north.
        let block = block_envelope(BlockFacts {
            width: 30,
            depth: 20,
            height: 5,
            delay_ticks: 37,
        });
        assert_eq!((block.min_y, block.max_y), (0, 4));
        assert_eq!(
            block
                .oriented_bounds(CellFacing::EAST, Facing::East)
                .height(),
            5
        );
    }

    /// The shelf pack on its own, against literal columns and a literal
    /// forward capacity: consecutive columns fill one deck until the next
    /// one plus that deck's turnaround would not fit, and then -- and only
    /// then -- a deck opens above.
    #[test]
    fn ordered_shelf_pack_uses_the_minimum_stable_decks() {
        use super::*;
        let col = |owner, lead, cost| DeckColumn {
            owner: InstanceId(owner),
            lead,
            cost,
            close: 0,
        };

        // 6 + 6 = 12 is past a capacity of 10, so the second column opens
        // deck 1; 6 + 4 = 10 is exactly the capacity, so the third joins it.
        let columns = [col(0, 0, 6), col(1, 0, 6), col(2, 0, 4)];
        assert_eq!(
            pack_decks(&columns, 10).unwrap(),
            vec![DeckId(0), DeckId(1), DeckId(1)]
        );
        assert_eq!(pack_decks(&columns, 10), pack_decks(&columns, 10));

        // Deck 0 has four cells to spare and the last column costs two, but
        // a column never moves back to a deck an earlier column left.
        let trailing = [col(0, 0, 6), col(1, 0, 6), col(2, 0, 2)];
        let packed = pack_decks(&trailing, 10).unwrap();
        assert_eq!(packed, vec![DeckId(0), DeckId(1), DeckId(1)]);
        assert!(packed.windows(2).all(|pair| pair[0] <= pair[1]));

        // A deck pays its closing turnaround once (3 here) and every upper
        // deck pays the channel it starts behind (2 here): 6 + 3 fills the
        // capacity of 9 exactly, and 2 + 4 + 3 fills the next deck exactly.
        let closing = [
            DeckColumn {
                close: 3,
                ..col(0, 0, 6)
            },
            DeckColumn {
                close: 3,
                ..col(1, 2, 4)
            },
        ];
        assert_eq!(pack_decks(&closing, 9).unwrap(), vec![DeckId(0), DeckId(1)]);

        // One column wider than the whole board is not a folding problem:
        // it is a macro the board cannot hold, and it is named.
        assert_eq!(
            pack_decks(&[col(7, 0, 11)], 10),
            Err(SeedPlacementError::LateralWindowTooNarrow {
                instance: InstanceId(7)
            })
        );
        // No forward span at all leaves no deck layout to find, while no
        // column at all needs none.
        assert_eq!(
            pack_decks(&columns, 0),
            Err(SeedPlacementError::NoDeckLayoutFits)
        );
        assert_eq!(pack_decks(&[], 0), Ok(Vec::new()));

        // Two decks whose macros reach one row below their ground and whose
        // routers reach three above it: the second ground is the first
        // integer that lifts its whole interval past `0..=4`.
        assert_eq!(
            deck_grounds(1, &[(-1, 3), (-1, 3)]).unwrap(),
            vec![
                DeckPlan {
                    ground: 1,
                    min_y: 0,
                    max_y: 4,
                },
                DeckPlan {
                    ground: 6,
                    min_y: 5,
                    max_y: 9,
                },
            ]
        );
    }

    /// The real placer on a board too short for its own chain: the levels
    /// that fit stay on the base deck and the one that does not opens the
    /// deck above, at a ground derived from the macros' own height.
    ///
    /// Every literal below is hand-derived from this fixture:
    ///
    /// * pins `(10, 20)` in and `(72, 60)` out, both travelling east, put
    ///   the frame's origin on the input's net cell `(12, 1, 20)`, forward
    ///   east and lateral south;
    /// * the board is `10..=72` by `20..=60`, so the forward limit is
    ///   `72 - 12 = 60` and the lateral window is `0..=40`;
    /// * one lane crosses every channel, so each is
    ///   `channel_width(1) + 2 = 11` cells and the first column starts at
    ///   `0 + 1 + 11 = 12`, leaving a capacity of `60 - 12 = 48`;
    /// * an east-facing torch spans two forward cells, so a column costs
    ///   `2 + 11 = 13` and a deck's turnaround costs `9 + 8 = 17`;
    /// * `13 + 13 + 17 = 43` fits, `13 + 13 + 13 + 17 = 56` does not, and
    ///   the third column pays its preceding channel as lead:
    ///   `11 + 13 + 17 = 41`.
    #[test]
    fn bounded_columns_fold_onto_ordered_decks() {
        use super::*;
        let graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("m0", &["a"]), nor("m1", &["m0"]), nor("y", &["m1"])],
            },
            &Library::default_library(),
        )
        .unwrap();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let facts = BTreeMap::new();
        let pins = BTreeMap::from([
            (
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                pin(Anchor { x: 10, y: 1, z: 20 }, Facing::East),
            ),
            (
                PhysicalEndpointId::DeclaredOutput(PortId(0)),
                pin(Anchor { x: 72, y: 1, z: 60 }, Facing::East),
            ),
        ]);
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &pins,
                block_facts: &facts,
            })
            .expect("a bounded board folds its columns onto decks");

        assert_eq!(plan.frame.forward, Facing::East);
        assert_eq!(plan.frame.origin, Anchor { x: 12, y: 1, z: 20 });

        // Two levels below, one above; the forward levels themselves are
        // untouched by the fold.
        let levels = [InstanceId(0), InstanceId(1), InstanceId(2)]
            .map(|id| plan.analysis.nodes[&id].forward_level);
        assert_eq!(levels, [0, 1, 2]);
        let decks = [InstanceId(0), InstanceId(1), InstanceId(2)]
            .map(|id| plan.analysis.nodes[&id].deck);
        assert_eq!(decks, [DeckId(0), DeckId(0), DeckId(1)]);

        // A torch stands on its own ground row, so every deck reserves one
        // row below it and the three-cell channel slab above it.
        assert_eq!(
            plan.decks,
            BTreeMap::from([
                (
                    DeckId(0),
                    DeckPlan {
                        ground: 1,
                        min_y: 0,
                        max_y: 4,
                    }
                ),
                (
                    DeckId(1),
                    DeckPlan {
                        ground: 6,
                        min_y: 5,
                        max_y: 9,
                    }
                ),
            ])
        );

        // Deck 0 steps `12 -> 24` by macro extent plus channel; deck 1
        // starts over at the same projected forward start.
        assert_eq!(
            plan.instances[&InstanceId(0)].preferred_origin,
            Anchor { x: 24, y: 1, z: 28 }
        );
        assert_eq!(
            plan.instances[&InstanceId(1)].preferred_origin,
            Anchor { x: 36, y: 1, z: 28 }
        );
        assert_eq!(
            plan.instances[&InstanceId(2)].preferred_origin,
            Anchor { x: 24, y: 1, z: 28 }
        );

        // Three 1x2x1 torch envelopes; their union spans forward 12..=25,
        // one lateral row, and grounds 1..=6: 14 * 1 * 6 = 84.
        assert_eq!(
            plan.floorplan,
            FloorplanMetrics {
                macro_volume: 6,
                union_volume: 84,
                deck_count: 2,
                cross_deck_nets: 0,
                vertical_trunk_lanes: 0,
            }
        );
    }

    fn actual_block_footprint(
        instance: &crate::compile::fragment_synth::instance_graph::Instance,
        facing: CellFacing,
        lateral_origin: i32,
    ) -> BTreeSet<(i32, i32)> {
        let positions = super::primitive_positions(instance);
        instance
            .expanded
            .topology
            .primitives
            .iter()
            .flat_map(|primitive| {
                let local = positions[&primitive.id];
                let (base_x, _, base_z) =
                    crate::compile::geometry::rotate((local.x, local.y, local.z), facing);
                crate::compile::physical::variants(primitive.primitive)[usize::from(facing.index())]
                    .blocks
                    .iter()
                    .map(move |block| {
                        (
                            base_x + block.position.x,
                            lateral_origin + base_z + block.position.z,
                        )
                    })
            })
            .collect()
    }

    /// Direct proof of the no-blocks invariant: the fingerprint for an
    /// existing multi-gate netlist, captured from the placer before Task 9's
    /// edits landed, must still match bit-for-bit now that `analyse_instance_dag`
    /// and `plan_in_frame` both read `graph.blocks` (empty here) alongside
    /// `graph.instances`.
    #[test]
    fn no_blocks_fingerprint_matches_the_pre_task_9_placer_exactly() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let pins = BTreeMap::new();
        let facts = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
            block_facts: &facts,
        };
        let plan = TopologyAwareSeedPlacer.plan(request).unwrap();
        assert_eq!(
            plan.fingerprint.as_str(),
            "24ef3d8e581982f52eeb7a40a6763ae2e8ad2493553aa4851fe309e55000bc93"
        );
    }
}
