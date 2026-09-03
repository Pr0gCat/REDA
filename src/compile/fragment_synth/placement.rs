//! Pure topology analysis for topology-aware seed placement.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::channel_plan::{channel_width, lane_count};
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
use crate::compile::planner::{PortPin, PortRole};
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
    #[error("layout repair cannot move pinned or missing owner {owner:?}")]
    ImmovableRepairOwner { owner: LayoutOwner },
    #[error("layout repair cannot separate the same owner {owner:?}")]
    SameRepairOwner { owner: LayoutOwner },
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
}

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
}

impl MacroBounds {
    fn forward_span(self) -> i32 {
        self.max_forward - self.min_forward + 1
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
/// Lateral cells the channel routing plan may use beyond the outermost
/// macro: the closed layers and their escape corridors.
pub(crate) const CHANNEL_LATERAL_MARGIN: i32 = 32;

impl TopologyAwareSeedPlacer {
    /// The plan with explicit minimum channel widths (keyed like
    /// `derive_channel_widths`) layered over the estimated ones.
    fn plan_with_widths(
        &self,
        request: SeedPlacementRequest<'_>,
        minimum_widths: &BTreeMap<i64, i32>,
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        // The frame points from the inputs toward the outputs.  When both
        // are pinned and the levels with their channels do not fit between
        // the two pin lines, the frame is turned around: the levels are then
        // placed behind the input line, and both pin lines face one
        // turnaround channel in front of it.
        let frame = derive_frame(request.pins);
        match self.plan_in_frame(request, minimum_widths, frame)? {
            Some(plan) => Ok(plan),
            None => {
                let flipped = PlacementFrame {
                    forward: frame.forward.opposite(),
                    lateral: clockwise(frame.forward.opposite()),
                    origin: frame.origin,
                };
                self.plan_in_frame(request, minimum_widths, flipped)?
                    .ok_or(SeedPlacementError::CoordinateOverflow)
            }
        }
    }

    /// The plan in one frame, or `None` when pinned outputs lie inside the
    /// forward span the levels need.
    fn plan_in_frame(
        &self,
        request: SeedPlacementRequest<'_>,
        minimum_widths: &BTreeMap<i64, i32>,
        frame: PlacementFrame,
    ) -> Result<Option<SeedPlacementPlan>, SeedPlacementError> {
        let analysis = request.analysis;
        let intervals = net_intervals(request.graph, analysis);
        let tracks = colour_intervals(&intervals);
        let track_laterals = track_laterals(request.graph, request.pins, frame, &tracks);
        let envelopes = request
            .graph
            .instances
            .iter()
            .map(|instance| macro_envelope(instance).map(|size| (instance.id, size)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;

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

        let bounds = request
            .graph
            .instances
            .iter()
            .map(|instance| {
                (
                    instance.id,
                    envelopes[&instance.id].oriented_bounds(facings[&instance.id], frame.forward),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut level_bounds = BTreeMap::<u64, MacroBounds>::new();
        for instance in &request.graph.instances {
            let level = analysis.nodes[&instance.id].forward_level;
            let instance_bounds = bounds[&instance.id];
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
        // Automatic ports share the shift: they sit on the free tracks the
        // macros were placed around.
        let automatic_input_lateral =
            |port: PortId| -> i32 { track_laterals[&LogicalSignalId::PrimaryInput(port)] };
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
            let port_laterals = automatic_input_ports
                .iter()
                .map(|&port| automatic_input_lateral(port))
                .chain(
                    automatic_output_ports
                        .iter()
                        .map(|&port| automatic_output_lateral(port)),
                )
                .collect::<Vec<_>>();
            confine_laterals_to_world(frame, &bounds, &port_laterals, &mut laterals)?
        };

        let mut channels =
            derive_channel_widths(request, analysis, &level_bounds, &laterals, &track_laterals);
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

        // Pinned inputs sit at the frame origin, so the first level starts
        // one input channel further forward; automatic inputs are placed one
        // input channel behind the origin instead.
        let pinned_inputs = request
            .pins
            .keys()
            .any(|endpoint| matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)));
        // Pinned outputs ahead of the levels must leave room for every
        // column and channel; otherwise the caller turns the frame around.
        let mut cursor = if pinned_inputs {
            input_channel + 1
        } else {
            0i32
        };
        if let Some((output_min, _)) = pinned_output_forward_extent(request, frame) {
            if output_min > 0 {
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
                if cursor + total > output_min {
                    return Ok(None);
                }
            }
        }
        let mut columns = BTreeMap::new();
        for (&level, level_bounds) in &level_bounds {
            let column = cursor
                .checked_sub(level_bounds.min_forward)
                .ok_or(SeedPlacementError::CoordinateOverflow)?;
            columns.insert(level, column);
            let width = channels
                .get(&(level as i64))
                .copied()
                .unwrap_or_else(|| channel_width(1));
            cursor = column
                .checked_add(level_bounds.max_forward)
                .and_then(|value| value.checked_add(width))
                .ok_or(SeedPlacementError::CoordinateOverflow)?;
        }

        let mut frame_origins = BTreeMap::<InstanceId, (i32, i32)>::new();
        for (&level, &column) in &columns {
            for (&id, &lateral) in &laterals {
                if analysis.nodes[&id].forward_level == level {
                    frame_origins.insert(id, (column, lateral));
                }
            }
        }

        let mut instances = BTreeMap::new();
        for instance in &request.graph.instances {
            let (forward, lateral) = frame_origins[&instance.id];
            let origin = frame_to_world(frame, forward, lateral);
            instances.insert(
                instance.id,
                PreferredInstancePose {
                    preferred_origin: origin,
                    facing: facings[&instance.id],
                },
            );
        }

        let input_forward = -input_channel;
        let output_forward = cursor;
        let automatic_inputs = automatic_input_ports
            .iter()
            .map(|&port| {
                let lateral = automatic_input_lateral(port)
                    .checked_add(lateral_shift)
                    .ok_or(SeedPlacementError::CoordinateOverflow)?;
                Ok((port, frame_to_world(frame, input_forward, lateral)))
            })
            .collect::<Result<BTreeMap<_, _>, SeedPlacementError>>()?;
        let automatic_outputs = automatic_output_ports
            .iter()
            .map(|&port| {
                let lateral = automatic_output_lateral(port)
                    .checked_add(lateral_shift)
                    .ok_or(SeedPlacementError::CoordinateOverflow)?;
                Ok((port, frame_to_world(frame, output_forward, lateral)))
            })
            .collect::<Result<BTreeMap<_, _>, SeedPlacementError>>()?;

        let fingerprint = plan_fingerprint(&instances, &automatic_inputs, &automatic_outputs, &[]);
        Ok(Some(SeedPlacementPlan {
            instances,
            automatic_inputs,
            automatic_outputs,
            fingerprint,
            frame,
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
    *anchor = checked_step_many(*anchor, lateral, distance)?;
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

/// Free forward cells after every level (keyed by that level; the input
/// channel is keyed by `min_level - 1`).
///
/// A channel needs one lane per trunk that crosses it at the same lateral
/// range, so the width comes from the left-edge lane count over the lateral
/// intervals of the nets alive in that channel.  When both inputs and
/// outputs are pinned the columns must still fit between the pin lines, so
/// the widths are scaled down proportionally when their sum does not fit;
/// the seed reports a typed refusal if a channel then cannot hold its lanes.
fn derive_channel_widths(
    request: SeedPlacementRequest<'_>,
    analysis: &SeedPlacementAnalysis,
    level_bounds: &BTreeMap<u64, MacroBounds>,
    laterals: &BTreeMap<InstanceId, i32>,
    track_laterals: &BTreeMap<LogicalSignalId, i32>,
) -> BTreeMap<i64, i32> {
    let intervals = net_intervals(request.graph, analysis);
    let min_level = level_bounds.keys().next().copied().unwrap_or(0) as i64;
    let max_level = level_bounds.keys().last().copied().unwrap_or(0) as i64;
    let mut widths = BTreeMap::new();
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
        widths.insert(
            channel_level,
            channel_width(lane_count(&crossing)) + ENDPOINT_CELLS_PER_CHANNEL,
        );
    }

    widths
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

/// Shifts every macro's lateral so the layout, with the channel layers the
/// routing plan closes around it, stays inside the origin-based world along
/// the frame's lateral axis.  Pinned ports fix the frame origin, so only the
/// macros and the automatic ports (at `port_laterals`) can move; they all
/// move together, by whole row-grid steps, and only when the world edge is
/// on their side.  Returns the shift the caller applies to the ports.
fn confine_laterals_to_world(
    frame: PlacementFrame,
    bounds: &BTreeMap<InstanceId, MacroBounds>,
    port_laterals: &[i32],
    laterals: &mut BTreeMap<InstanceId, i32>,
) -> Result<i32, SeedPlacementError> {
    let (lx, lz) = horizontal_unit(frame.lateral);
    let (sign, origin) = if lx != 0 {
        (lx, frame.origin.x)
    } else {
        (lz, frame.origin.z)
    };
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
    // World coordinate along the lateral axis: `origin + sign * lateral`.
    let shortfall = if sign > 0 {
        (CHANNEL_LATERAL_MARGIN - origin - extent_min).max(0)
    } else {
        (extent_max - (origin - CHANNEL_LATERAL_MARGIN)).max(0)
    };
    if shortfall == 0 {
        return Ok(0);
    }
    let steps = (shortfall + ROW_GRID - 1) / ROW_GRID;
    let shift = steps
        .checked_mul(ROW_GRID)
        .map(|shift| if sign > 0 { shift } else { -shift })
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
    graph.instances.iter().map(|instance| {
        let mut values = Vec::new();
        for assignment in graph.assignments.iter().filter(|assignment| {
            matches!(assignment.sink, PhysicalSink::InstanceInput { instance: sink, .. } if sink == instance.id)
                || matches!(&assignment.driver, PhysicalDriver::Instance(driver) if instance_driver_owner(driver) == instance.id)
        }) {
            let critical = match (&assignment.driver, assignment.sink) {
                (PhysicalDriver::Instance(driver), PhysicalSink::InstanceInput { instance: sink, .. }) => analysis.edges.iter().any(|edge| edge.source == instance_driver_owner(driver) && edge.sink == sink && edge.structural_slack_ticks == 0),
                _ => false,
            };
            let weight = 1 + fanout[&assignment.signal] + usize::from(critical) * 4;
            values.extend(std::iter::repeat_n(track_laterals[&assignment.signal], weight));
        }
        values.sort();
        (instance.id, values.get(values.len() / 2).copied().unwrap_or(0))
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
            let (base_x, _, base_z) = geometry::rotate((local.x, local.y, local.z), facing);
            let variant = &variants[usize::from(facing.index())];
            for point in variant
                .blocks
                .iter()
                .map(|block| block.position)
                .chain(variant.ports.iter().map(|port| port.position))
            {
                let x = base_x + point.x;
                let z = base_z + point.z;
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
    Ok(MacroEnvelope { by_facing })
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
) -> Result<SeedPlacementAnalysis, SeedPlacementError> {
    let ids = graph
        .instances
        .iter()
        .map(|instance| instance.id)
        .collect::<BTreeSet<_>>();
    if ids.len() != graph.instances.len() {
        let mut seen = BTreeSet::new();
        let instance = graph
            .instances
            .iter()
            .map(|instance| instance.id)
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
    fn pinned_layouts_shift_away_from_the_world_edge_in_grid_steps() {
        use super::*;
        // Lateral runs west from a pin at x = 21: the macros at laterals
        // 0..=35 would land at negative x, so the whole free layout moves to
        // higher laterals until the channel margin fits, by whole grid steps.
        let frame = PlacementFrame {
            forward: Facing::South,
            lateral: Facing::West,
            origin: Anchor { x: 21, y: 1, z: 62 },
        };
        let bounds = MacroBounds {
            min_forward: 0,
            max_forward: 3,
            min_lateral: -1,
            max_lateral: 3,
        };
        let ids = [InstanceId(0), InstanceId(1)];
        let bounds = ids
            .iter()
            .map(|&id| (id, bounds))
            .collect::<BTreeMap<_, _>>();
        let mut laterals = BTreeMap::from([(ids[0], 0), (ids[1], 32)]);

        let shift = confine_laterals_to_world(frame, &bounds, &[16], &mut laterals).unwrap();

        // extent_max = 32 + 3 = 35 must come down to 21 - 32 = -11: a shortfall
        // of 46 rounds up to twelve grid steps of four.
        assert_eq!(shift, -48);
        assert_eq!(laterals[&ids[0]], -48);
        assert_eq!(laterals[&ids[1]], -16);

        // Lateral running east from the same pin: extent_min = -1 lands at
        // x = 20, eleven cells short of the margin, so the layout moves up by
        // three grid steps; far from the edge nothing moves.
        let frame = PlacementFrame {
            lateral: Facing::East,
            ..frame
        };
        let mut laterals = BTreeMap::from([(ids[0], 0), (ids[1], 32)]);
        assert_eq!(
            confine_laterals_to_world(frame, &bounds, &[16], &mut laterals).unwrap(),
            12
        );
        assert_eq!(laterals[&ids[1]], 44);
        let mut laterals = BTreeMap::from([(ids[0], 40), (ids[1], 72)]);
        assert_eq!(
            confine_laterals_to_world(frame, &bounds, &[56], &mut laterals).unwrap(),
            0
        );
        assert_eq!(laterals[&ids[1]], 72);
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
        legalize_laterals, EdgeFacts, LayoutOwner, LayoutRepair, MacroBounds, NetInterval,
        SeedPlacementError, SeedPlacementRequest, SeedPlacer, TopologyAwareSeedPlacer, TRACK_PITCH,
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
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
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

    #[test]
    fn repair_order_is_canonical_and_same_owner_separation_is_named() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
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
        let analysis = analyse_instance_dag(&graph).unwrap();
        let input = PhysicalEndpointId::PrimaryInput(PortId(0));
        let pins = BTreeMap::from([(input, pin(Anchor { x: 20, y: 1, z: 40 }, Facing::North))]);
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
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
        assert_eq!(
            repaired.instances[&InstanceId(0)].preferred_origin.x,
            baseline.instances[&InstanceId(0)].preferred_origin.x + 2 * TRACK_PITCH
        );
    }

    #[test]
    fn dependency_order_ignores_reversed_gate_declaration_order() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![nor("y", &["produced_later"]), nor("produced_later", &["a"])],
        };
        let graph = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();

        let facts = analyse_instance_dag(&graph).unwrap();

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

        let facts = analyse_instance_dag(&graph).unwrap();

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

        let facts = analyse_instance_dag(&graph).unwrap();

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

        let facts = analyse_instance_dag(&graph).unwrap();

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

        let facts = analyse_instance_dag(&graph).unwrap();

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
            analyse_instance_dag(&graph),
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
            analyse_instance_dag(&cycle),
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
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &pins,
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
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &analysis,
                pins: &pins,
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
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let placer = TopologyAwareSeedPlacer;
        let request = || SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
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
        let mut injected = analyse_instance_dag(&graph).unwrap();
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

        let plan = TopologyAwareSeedPlacer
            .plan(SeedPlacementRequest {
                graph: &graph,
                analysis: &injected,
                pins: &pins,
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
            }
        );
        assert_eq!(
            repeater_bounds,
            MacroBounds {
                min_forward: 0,
                max_forward: 0,
                min_lateral: -8,
                max_lateral: 0,
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
}
