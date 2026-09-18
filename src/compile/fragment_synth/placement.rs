//! Pure topology analysis for topology-aware seed placement.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{InstanceId, PrimitiveId};
use crate::compile::fragment_synth::identity::{PhysicalEndpointId, PortId};
use crate::compile::fragment_synth::instance_graph::{
    Instance, InstanceDriver, InstanceGraph, LogicalSignalId, PhysicalDriver, PhysicalSink,
};
use crate::compile::fragment_synth::terminal_geometry::{
    primitive_input_terminal, primitive_output_terminal, PrimitiveTerminalError,
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
    #[error("layout repair cannot separate the same owner {owner:?}")]
    SameRepairOwner { owner: LayoutOwner },
    #[error("physical input terminal is invalid: {0}")]
    PrimitiveTerminal(#[from] PrimitiveTerminalError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    Primitive(PrimitiveId),
    Junction(InstanceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum RunwayDirection {
    North,
    South,
    East,
    West,
}

impl RunwayDirection {
    fn facing(self) -> Facing {
        match self {
            Self::North => Facing::North,
            Self::South => Facing::South,
            Self::East => Facing::East,
            Self::West => Facing::West,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum SeparationAxis {
    Lateral,
    Runway(RunwayDirection),
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
    ReserveSourceEscape {
        source: PhysicalEndpointId,
    },
    RouteBefore {
        source: PhysicalEndpointId,
        blocker: PhysicalEndpointId,
    },
    SeparateOwners {
        source_owner: LayoutOwner,
        sink_owner: LayoutOwner,
        axis: SeparationAxis,
        ordinal: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreferredInstancePose {
    pub preferred_origin: Anchor,
    pub facing: CellFacing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeedPlacementPlan {
    pub frame: PlacementFrame,
    pub signal_tracks: BTreeMap<LogicalSignalId, i32>,
    pub instances: BTreeMap<InstanceId, PreferredInstancePose>,
    pub automatic_inputs: BTreeMap<PortId, Anchor>,
    pub automatic_outputs: BTreeMap<PortId, Anchor>,
    pub owner_offsets: BTreeMap<LayoutOwner, Anchor>,
    pub fingerprint: Fingerprint,
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

const ROUTING_CHANNEL: i32 = 6;
const TRACK_PITCH: i32 = 6;

impl SeedPlacer for TopologyAwareSeedPlacer {
    fn plan(
        &self,
        request: SeedPlacementRequest<'_>,
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        let analysis = request.analysis;
        let frame = derive_frame(request.pins);
        let intervals = net_intervals(request.graph, analysis);
        let tracks = colour_intervals(&intervals);
        let track_laterals = track_laterals(request.graph, request.pins, frame, &tracks);
        let envelopes = request
            .graph
            .instances
            .iter()
            .map(|instance| macro_access_envelope(instance).map(|size| (instance.id, size)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;

        let mut lanes = initial_lanes(request.graph, &track_laterals, analysis);
        barycentric_sweep(analysis, &mut lanes, true);
        barycentric_sweep(analysis, &mut lanes, false);

        let mut facings = BTreeMap::new();
        for instance in &request.graph.instances {
            let lateral = lanes[&instance.id];
            let input_tracks = request
                .graph
                .assignments
                .iter()
                .filter_map(|assignment| match assignment.sink {
                    PhysicalSink::InstanceInput {
                        instance: sink,
                        input_index,
                    } if sink == instance.id => {
                        Some((input_index, track_laterals[&assignment.signal]))
                    }
                    _ => None,
                })
                .collect::<BTreeMap<_, _>>();
            let output_track = track_laterals
                .get(&LogicalSignalId::GateOutput(instance.logical_gate))
                .copied();
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
            let source = frame_to_world(frame, -ROUTING_CHANNEL, lateral);
            let target = frame_to_world(frame, max_span + ROUTING_CHANNEL, lateral);
            facings.insert(
                instance.id,
                choose_instance_facing_with_tracks(
                    instance,
                    origin,
                    source,
                    target,
                    frame,
                    &input_tracks,
                    output_track,
                )?,
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
        let mut columns = BTreeMap::new();
        let mut cursor = 0i32;
        for (&level, level_bounds) in &level_bounds {
            let column = cursor
                .checked_sub(level_bounds.min_forward)
                .ok_or(SeedPlacementError::CoordinateOverflow)?;
            columns.insert(level, column);
            cursor = column
                .checked_add(level_bounds.max_forward)
                .and_then(|value| value.checked_add(ROUTING_CHANNEL))
                .ok_or(SeedPlacementError::CoordinateOverflow)?;
        }

        let mut frame_origins = BTreeMap::<InstanceId, (i32, i32)>::new();
        for (&level, &column) in &columns {
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
                let lateral = legalized[&id];
                frame_origins.insert(id, (column, lateral));
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

        let input_forward = -ROUTING_CHANNEL;
        let output_forward = cursor;
        let automatic_inputs = request
            .graph
            .primary_inputs
            .iter()
            .copied()
            .filter(|port| {
                !request
                    .pins
                    .contains_key(&PhysicalEndpointId::PrimaryInput(*port))
            })
            .map(|port| {
                let signal = LogicalSignalId::PrimaryInput(port);
                (
                    port,
                    frame_to_world(frame, input_forward, track_laterals[&signal]),
                )
            })
            .collect();
        let automatic_outputs = request
            .graph
            .declared_outputs
            .iter()
            .copied()
            .filter(|port| {
                !request
                    .pins
                    .contains_key(&PhysicalEndpointId::DeclaredOutput(*port))
            })
            .map(|port| {
                let signal = request
                    .graph
                    .assignments
                    .iter()
                    .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                    .map(|assignment| assignment.signal);
                let lateral = signal
                    .and_then(|signal| track_laterals.get(&signal).copied())
                    .unwrap_or(0);
                (port, frame_to_world(frame, output_forward, lateral))
            })
            .collect();

        let owner_offsets = BTreeMap::new();
        let fingerprint = plan_fingerprint(
            frame,
            &track_laterals,
            &instances,
            &automatic_inputs,
            &automatic_outputs,
            &owner_offsets,
            &[],
        );
        Ok(SeedPlacementPlan {
            frame,
            signal_tracks: track_laterals,
            instances,
            automatic_inputs,
            automatic_outputs,
            owner_offsets,
            fingerprint,
        })
    }

    fn plan_with_repairs(
        &self,
        request: SeedPlacementRequest<'_>,
        repairs: &[LayoutRepair],
    ) -> Result<SeedPlacementPlan, SeedPlacementError> {
        let mut plan = self.plan(request)?;
        let repairs = repairs.iter().copied().collect::<BTreeSet<_>>();
        if repairs.is_empty() {
            return Ok(plan);
        }
        let baseline = plan.clone();
        let frame = derive_frame(request.pins);
        for repair in &repairs {
            match *repair {
                LayoutRepair::ExclusiveGuardedTrack { .. } => {}
                LayoutRepair::EarlyTreeSinkAndEscape { .. } => {}
                LayoutRepair::ReserveSourceEscape { .. } => {}
                LayoutRepair::RouteBefore { .. } => {}
                LayoutRepair::SeparateOwners {
                    source_owner,
                    sink_owner,
                    axis,
                    ordinal,
                } => {
                    if source_owner == sink_owner {
                        return Err(SeedPlacementError::SameRepairOwner {
                            owner: source_owner,
                        });
                    }
                    let direction = match axis {
                        SeparationAxis::Lateral => frame.lateral,
                        SeparationAxis::Runway(direction) => direction.facing(),
                    };
                    let source_coordinate = owner_anchor(&baseline, source_owner, request.pins)
                        .map(|anchor| project_horizontal(anchor.x, anchor.z, direction));
                    let sink_coordinate = owner_anchor(&baseline, sink_owner, request.pins)
                        .map(|anchor| project_horizontal(anchor.x, anchor.z, direction));
                    let preferred_sign = match axis {
                        SeparationAxis::Runway(_) => 1,
                        SeparationAxis::Lateral => match (source_coordinate, sink_coordinate) {
                            (Some(source), Some(sink)) if sink < source => -1,
                            _ => 1,
                        },
                    };
                    let _ = separate_owners_legalized(
                        &mut plan,
                        source_owner,
                        sink_owner,
                        request,
                        direction,
                        preferred_sign,
                        i32::from(ordinal).saturating_add(1),
                    )?;
                }
            }
        }
        let repairs = repairs.into_iter().collect::<Vec<_>>();
        plan.fingerprint = plan_fingerprint(
            plan.frame,
            &plan.signal_tracks,
            &plan.instances,
            &plan.automatic_inputs,
            &plan.automatic_outputs,
            &plan.owner_offsets,
            &repairs,
        );
        Ok(plan)
    }
}

fn owner_anchor(
    plan: &SeedPlacementPlan,
    owner: LayoutOwner,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
) -> Option<Anchor> {
    match owner {
        LayoutOwner::Instance(instance) => plan
            .instances
            .get(&instance)
            .map(|pose| pose.preferred_origin),
        LayoutOwner::Primitive(primitive) => plan.instances.get(&primitive.instance).map(|pose| {
            add_offset(
                pose.preferred_origin,
                plan.owner_offsets
                    .get(&owner)
                    .copied()
                    .unwrap_or(Anchor { x: 0, y: 0, z: 0 }),
            )
        }),
        LayoutOwner::Junction(instance) => plan.instances.get(&instance).map(|pose| {
            add_offset(
                pose.preferred_origin,
                plan.owner_offsets
                    .get(&owner)
                    .copied()
                    .unwrap_or(Anchor { x: 0, y: 0, z: 0 }),
            )
        }),
        LayoutOwner::Boundary(endpoint) => {
            pins.get(&endpoint)
                .map(|pin| pin.at)
                .or_else(|| match endpoint {
                    PhysicalEndpointId::PrimaryInput(port) => {
                        plan.automatic_inputs.get(&port).copied()
                    }
                    PhysicalEndpointId::DeclaredOutput(port) => {
                        plan.automatic_outputs.get(&port).copied()
                    }
                    _ => None,
                })
        }
    }
}

fn move_owner_legalized(
    plan: &mut SeedPlacementPlan,
    owner: LayoutOwner,
    request: SeedPlacementRequest<'_>,
    lateral: Facing,
    distance: i32,
) -> Result<bool, SeedPlacementError> {
    let mut trial = plan.clone();
    if !move_owner(&mut trial, owner, request.pins, lateral, distance)? {
        return Ok(false);
    }
    let collides = match owner {
        LayoutOwner::Instance(instance) | LayoutOwner::Junction(instance) => {
            instance_owner_collides(&trial, instance, request)
        }
        LayoutOwner::Primitive(primitive) => {
            instance_owner_collides(&trial, primitive.instance, request)
        }
        LayoutOwner::Boundary(endpoint) => boundary_owner_collides(&trial, endpoint, request),
    };
    if collides {
        return Ok(false);
    }
    *plan = trial;
    Ok(true)
}

fn separate_owners_legalized(
    plan: &mut SeedPlacementPlan,
    source_owner: LayoutOwner,
    sink_owner: LayoutOwner,
    request: SeedPlacementRequest<'_>,
    direction: Facing,
    preferred_sign: i32,
    first_shell: i32,
) -> Result<bool, SeedPlacementError> {
    let shell_count = i32::try_from(request.graph.instances.len())
        .unwrap_or(i32::MAX)
        .saturating_mul(2)
        .max(8);
    for shell in first_shell..=first_shell.saturating_add(shell_count) {
        let magnitude = TRACK_PITCH.saturating_mul(shell);
        for (owner, sign) in [
            (sink_owner, preferred_sign),
            (sink_owner, -preferred_sign),
            (source_owner, -preferred_sign),
            (source_owner, preferred_sign),
        ] {
            if move_owner_legalized(
                plan,
                owner,
                request,
                direction,
                magnitude.saturating_mul(sign),
            )? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn instance_owner_collides(
    plan: &SeedPlacementPlan,
    instance: InstanceId,
    request: SeedPlacementRequest<'_>,
) -> bool {
    let Some(candidate) = placed_instance_bounds(plan, request.graph, instance) else {
        return true;
    };
    if plan
        .instances
        .keys()
        .copied()
        .filter(|other| *other != instance)
        .filter_map(|other| placed_instance_bounds(plan, request.graph, other))
        .any(|other| horizontal_bounds_overlap(candidate, other))
    {
        return true;
    }
    boundary_endpoints(plan, request.pins)
        .filter_map(|endpoint| placed_boundary_bounds(plan, endpoint, request.pins))
        .any(|boundary| horizontal_bounds_overlap(candidate, boundary))
}

fn boundary_owner_collides(
    plan: &SeedPlacementPlan,
    endpoint: PhysicalEndpointId,
    request: SeedPlacementRequest<'_>,
) -> bool {
    let Some(candidate) = placed_boundary_bounds(plan, endpoint, request.pins) else {
        return true;
    };
    if plan
        .instances
        .keys()
        .copied()
        .filter_map(|instance| placed_instance_bounds(plan, request.graph, instance))
        .any(|instance| horizontal_bounds_overlap(candidate, instance))
    {
        return true;
    }
    boundary_endpoints(plan, request.pins)
        .filter(|other| *other != endpoint)
        .filter_map(|other| placed_boundary_bounds(plan, other, request.pins))
        .any(|other| horizontal_bounds_overlap(candidate, other))
}

fn boundary_endpoints<'a>(
    plan: &'a SeedPlacementPlan,
    pins: &'a BTreeMap<PhysicalEndpointId, PortPin>,
) -> impl Iterator<Item = PhysicalEndpointId> + 'a {
    pins.keys()
        .copied()
        .chain(
            plan.automatic_inputs
                .keys()
                .copied()
                .map(PhysicalEndpointId::PrimaryInput),
        )
        .chain(
            plan.automatic_outputs
                .keys()
                .copied()
                .map(PhysicalEndpointId::DeclaredOutput),
        )
}

fn placed_boundary_bounds(
    plan: &SeedPlacementPlan,
    endpoint: PhysicalEndpointId,
    pins: &BTreeMap<PhysicalEndpointId, PortPin>,
) -> Option<HorizontalBounds> {
    if let Some(pin) = pins.get(&endpoint).copied() {
        let (route_endpoint, direction) = match endpoint {
            PhysicalEndpointId::PrimaryInput(_) => (pin.net_cell(PortRole::Input), pin.toward),
            PhysicalEndpointId::DeclaredOutput(_) => {
                (pin.handover(PortRole::Output), pin.toward.opposite())
            }
            _ => return None,
        };
        return Some(port_access_bounds([pin.at], route_endpoint, direction));
    }

    let forward = derive_frame(pins).forward;
    match endpoint {
        PhysicalEndpointId::PrimaryInput(port) => {
            let body = *plan.automatic_inputs.get(&port)?;
            Some(port_access_bounds(
                [body],
                step_anchor(body, forward),
                forward,
            ))
        }
        PhysicalEndpointId::DeclaredOutput(port) => {
            let body = *plan.automatic_outputs.get(&port)?;
            let direction = forward.opposite();
            Some(port_access_bounds(
                [body],
                step_anchor(body, direction),
                direction,
            ))
        }
        _ => None,
    }
}

fn port_access_bounds(
    bodies: impl IntoIterator<Item = Anchor>,
    route_endpoint: Anchor,
    direction: Facing,
) -> HorizontalBounds {
    let approach = step_anchor(route_endpoint, direction);
    let mut points = bodies.into_iter().chain([route_endpoint, approach]).chain(
        [Facing::North, Facing::South, Facing::East, Facing::West]
            .into_iter()
            .map(|facing| step_anchor(approach, facing)),
    );
    let first = points.next().unwrap_or(route_endpoint);
    points.fold(
        HorizontalBounds {
            min_x: first.x,
            max_x: first.x,
            min_z: first.z,
            max_z: first.z,
        },
        |bounds, point| HorizontalBounds {
            min_x: bounds.min_x.min(point.x),
            max_x: bounds.max_x.max(point.x),
            min_z: bounds.min_z.min(point.z),
            max_z: bounds.max_z.max(point.z),
        },
    )
}

fn placed_instance_bounds(
    plan: &SeedPlacementPlan,
    graph: &InstanceGraph,
    instance: InstanceId,
) -> Option<HorizontalBounds> {
    let pose = plan.instances.get(&instance)?;
    let instance = graph
        .instances
        .iter()
        .find(|candidate| candidate.id == instance)?;
    let local = macro_access_envelope(instance).ok()?.by_facing[usize::from(pose.facing.index())];
    Some(HorizontalBounds {
        min_x: pose.preferred_origin.x.saturating_add(local.min_x),
        max_x: pose.preferred_origin.x.saturating_add(local.max_x),
        min_z: pose.preferred_origin.z.saturating_add(local.min_z),
        max_z: pose.preferred_origin.z.saturating_add(local.max_z),
    })
}

fn horizontal_bounds_overlap(left: HorizontalBounds, right: HorizontalBounds) -> bool {
    left.min_x <= right.max_x
        && right.min_x <= left.max_x
        && left.min_z <= right.max_z
        && right.min_z <= left.max_z
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
        LayoutOwner::Primitive(_) | LayoutOwner::Junction(_) => {
            let offset = plan
                .owner_offsets
                .entry(owner)
                .or_insert(Anchor { x: 0, y: 0, z: 0 });
            *offset = checked_step_many(*offset, lateral, distance)?;
            return Ok(true);
        }
    };
    let Some(anchor) = anchor else {
        return Ok(false);
    };
    *anchor = checked_step_many(*anchor, lateral, distance)?;
    Ok(true)
}

fn add_offset(anchor: Anchor, offset: Anchor) -> Anchor {
    Anchor {
        x: anchor.x.saturating_add(offset.x),
        y: anchor.y.saturating_add(offset.y),
        z: anchor.z.saturating_add(offset.z),
    }
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

fn derive_frame(pins: &BTreeMap<PhysicalEndpointId, PortPin>) -> PlacementFrame {
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

const fn horizontal_unit(direction: Facing) -> (i32, i32) {
    match direction {
        Facing::North => (0, -1),
        Facing::South => (0, 1),
        Facing::East => (1, 0),
        Facing::West => (-1, 0),
        Facing::Up | Facing::Down => unreachable!(),
    }
}

const fn project_horizontal(x: i32, z: i32, direction: Facing) -> i32 {
    match direction {
        Facing::North => -z,
        Facing::South => z,
        Facing::East => x,
        Facing::West => -x,
        Facing::Up | Facing::Down => unreachable!(),
    }
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
        origins.insert(instance, origin);
        next_min_lateral = Some(
            origin
                .checked_add(bounds.max_lateral)
                .and_then(|maximum| maximum.checked_add(ROUTING_CHANNEL))
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
                .filter_map(|assignment| match &assignment.driver {
                    PhysicalDriver::PrimaryInput(_) => Some(0),
                    PhysicalDriver::Instance(driver) => {
                        Some(analysis.nodes[&instance_driver_owner(driver)].forward_level)
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
    macro_envelope_with_access(instance, false)
}

fn macro_access_envelope(instance: &Instance) -> Result<MacroEnvelope, SeedPlacementError> {
    macro_envelope_with_access(instance, true)
}

fn macro_envelope_with_access(
    instance: &Instance,
    include_access: bool,
) -> Result<MacroEnvelope, SeedPlacementError> {
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
            let mut include = |point: Position| {
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
            };
            for block in variant.blocks {
                include(block.position);
            }
            for port in variant.ports {
                include(port.position);
                if !include_access {
                    continue;
                }
                let terminal = port.position.offset(port.direction);
                let approach = terminal.offset(port.direction);
                include(terminal);
                include(approach);
                for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                    include(approach.offset(direction));
                }
            }
            if include_access {
                let primitive_anchor = Anchor {
                    x: base_x,
                    y: 0,
                    z: base_z,
                };
                for ordinal in 0..instance
                    .expanded
                    .topology
                    .connections
                    .iter()
                    .filter(|connection| {
                        connection.target == ConnectionTarget::Primitive(primitive.id)
                    })
                    .count()
                {
                    let input = primitive_input_terminal(
                        primitive.primitive,
                        facing,
                        primitive_anchor,
                        ordinal,
                    )?;
                    let terminal =
                        Position::new(input.terminal.x, input.terminal.y, input.terminal.z);
                    let approach = terminal.offset(input.allowed_entry);
                    include(terminal);
                    include(approach);
                    for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                        include(approach.offset(direction));
                    }
                }
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
    choose_instance_facing_with_tracks(
        instance,
        origin,
        source,
        target,
        PlacementFrame {
            forward,
            lateral: clockwise(forward),
            origin,
        },
        &BTreeMap::new(),
        None,
    )
}

fn choose_instance_facing_with_tracks(
    instance: &Instance,
    origin: Anchor,
    source: Anchor,
    target: Anchor,
    frame: PlacementFrame,
    input_tracks: &BTreeMap<u16, i32>,
    output_track: Option<i32>,
) -> Result<CellFacing, SeedPlacementError> {
    const TRACK_ALIGNMENT_WEIGHT: i64 = 8;

    let positions = primitive_positions(instance);
    let roles = instance
        .expanded
        .topology
        .primitives
        .iter()
        .map(|primitive| (primitive.role, positions[&primitive.id]))
        .collect::<BTreeMap<_, _>>();
    let mut best = None;
    for (rank, facing) in [
        CellFacing::NORTH,
        CellFacing::EAST,
        CellFacing::SOUTH,
        CellFacing::WEST,
    ]
    .into_iter()
    .enumerate()
    {
        let mut score = hint_penalty(
            &roles,
            &instance.expanded.topology.embedding_hints,
            facing,
            frame.forward,
        ) * 8;
        for primitive in &instance.expanded.topology.primitives {
            let local = positions[&primitive.id];
            let at = primitive_world(origin, local, facing);
            score += manhattan(source, input_terminal(primitive.primitive, facing, at));
            score += manhattan(output_terminal(primitive.primitive, facing, at), target);
        }
        let mut ordinal_by_primitive = BTreeMap::<PrimitiveId, usize>::new();
        for connection in &instance.expanded.topology.connections {
            let ConnectionTarget::Primitive(target_id) = connection.target else {
                continue;
            };
            let ordinal = ordinal_by_primitive.entry(target_id).or_default();
            let target_spec = instance
                .expanded
                .topology
                .primitives
                .iter()
                .find(|primitive| primitive.id == target_id)
                .expect("validated target");
            let target_at = primitive_world(origin, positions[&target_id], facing);
            let target_port =
                primitive_input_terminal(target_spec.primitive, facing, target_at, *ordinal)?
                    .terminal;
            *ordinal += 1;
            match connection.source {
                ConnectionSource::ExternalInput { input_index } => {
                    if let Some(track) = input_tracks.get(&input_index) {
                        score += i64::from((lateral_projection(frame, target_port) - track).abs())
                            * TRACK_ALIGNMENT_WEIGHT;
                    }
                }
                ConnectionSource::Primitive(source_id) => {
                    let source_spec = instance
                        .expanded
                        .topology
                        .primitives
                        .iter()
                        .find(|primitive| primitive.id == source_id)
                        .expect("validated source");
                    let source_at = primitive_world(origin, positions[&source_id], facing);
                    let source_port = output_terminal(source_spec.primitive, facing, source_at);
                    score += manhattan(source_port, target_port);
                    if forward_projection(target_port, frame.forward)
                        <= forward_projection(source_port, frame.forward)
                    {
                        score += 4;
                    }
                }
            }
        }
        let mut output_direction_rank = 0usize;
        if let OutputSpec::Primitive(output_id) = &instance.expanded.topology.output {
            let output_spec = instance
                .expanded
                .topology
                .primitives
                .iter()
                .find(|primitive| primitive.id == *output_id)
                .expect("validated output");
            let output_at = primitive_world(origin, positions[output_id], facing);
            let output = primitive_output_terminal(output_spec.primitive, facing, output_at)?;
            output_direction_rank = usize::from(output.allowed_exit != frame.forward);
            if let Some(track) = output_track {
                score += i64::from((lateral_projection(frame, output.support) - track).abs())
                    * TRACK_ALIGNMENT_WEIGHT;
            }
        }
        let key = (output_direction_rank, score, rank);
        if best.map(|(old, _)| key < old).unwrap_or(true) {
            best = Some((key, facing));
        }
    }
    Ok(best
        .map(|(_, facing)| facing)
        .unwrap_or_else(|| facing_for_direction(frame.forward)))
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
    frame: PlacementFrame,
    signal_tracks: &BTreeMap<LogicalSignalId, i32>,
    instances: &BTreeMap<InstanceId, PreferredInstancePose>,
    automatic_inputs: &BTreeMap<PortId, Anchor>,
    automatic_outputs: &BTreeMap<PortId, Anchor>,
    owner_offsets: &BTreeMap<LayoutOwner, Anchor>,
    repairs: &[LayoutRepair],
) -> Fingerprint {
    let poses = instances
        .iter()
        .map(|(id, pose)| (*id, pose.preferred_origin, pose.facing.index()))
        .collect::<Vec<_>>();
    let offsets = owner_offsets
        .iter()
        .map(|(owner, offset)| (*owner, *offset))
        .collect::<Vec<_>>();
    let tracks = signal_tracks
        .iter()
        .map(|(signal, lateral)| (*signal, *lateral))
        .collect::<Vec<_>>();
    let bytes = if repairs.is_empty() && owner_offsets.is_empty() {
        serde_json::to_vec(&(
            "topology-aware-seed-v2",
            frame,
            &tracks,
            poses,
            automatic_inputs,
            automatic_outputs,
        ))
    } else {
        serde_json::to_vec(&(
            "topology-aware-seed-repairs-v2",
            frame,
            &tracks,
            poses,
            automatic_inputs,
            automatic_outputs,
            offsets,
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
            Ok::<_, SeedPlacementError>(EdgeFacts {
                source,
                sink,
                structural_slack_ticks: critical_delay_ticks.saturating_sub(path_ticks),
            })
        })
        .collect::<Result<Vec<_>, SeedPlacementError>>()?;

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
    use std::collections::{BTreeMap, BTreeSet};

    use crate::compile::fragment_synth::benchmark::legacy_benchmark_evaluator;
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
        analyse_instance_dag, choose_instance_facing, choose_instance_facing_with_tracks,
        colour_intervals, derive_frame, hint_penalty, instance_owner_collides, legalize_laterals,
        macro_access_envelope, EdgeFacts, LayoutOwner, LayoutRepair, MacroBounds, NetInterval,
        PlacementFrame, RunwayDirection, SeedPlacementError, SeedPlacementRequest, SeedPlacer,
        SeparationAxis, TopologyAwareSeedPlacer, TRACK_PITCH,
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
        assert_eq!(exclusive.instances, baseline.instances);
        assert_eq!(exclusive.owner_offsets, baseline.owner_offsets);
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
        assert_eq!(early.instances, baseline.instances);
        assert_eq!(early.owner_offsets, baseline.owner_offsets);
        assert_ne!(early.fingerprint, baseline.fingerprint);
        assert_ne!(early.fingerprint, exclusive.fingerprint);

        let separated = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::SeparateOwners {
                    source_owner: LayoutOwner::Instance(InstanceId(0)),
                    sink_owner: LayoutOwner::Instance(InstanceId(1)),
                    axis: SeparationAxis::Lateral,
                    ordinal: 0,
                }],
            )
            .unwrap();
        assert_eq!(
            (separated.instances[&InstanceId(1)].preferred_origin.z
                - baseline.instances[&InstanceId(1)].preferred_origin.z)
                .abs(),
            TRACK_PITCH
        );
        assert_eq!(
            separated.instances[&InstanceId(0)],
            baseline.instances[&InstanceId(0)]
        );

        let runway = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::SeparateOwners {
                    source_owner: LayoutOwner::Instance(InstanceId(0)),
                    sink_owner: LayoutOwner::Instance(InstanceId(1)),
                    axis: SeparationAxis::Runway(RunwayDirection::East),
                    ordinal: 0,
                }],
            )
            .unwrap();
        assert_eq!(
            runway.instances[&InstanceId(1)].preferred_origin.x,
            baseline.instances[&InstanceId(1)].preferred_origin.x + TRACK_PITCH
        );
        assert_eq!(
            runway.instances[&InstanceId(1)].preferred_origin.z,
            baseline.instances[&InstanceId(1)].preferred_origin.z
        );
        assert_eq!(
            runway.instances[&InstanceId(0)],
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
            axis: SeparationAxis::Lateral,
            ordinal: 0,
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
                    axis: SeparationAxis::Lateral,
                    ordinal: 0,
                }],
            ),
            Err(SeedPlacementError::SameRepairOwner {
                owner: LayoutOwner::Instance(InstanceId(0))
            })
        );
    }

    #[test]
    fn repaired_cell_owner_collision_is_detected_before_materialisation() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
        };
        let mut plan = TopologyAwareSeedPlacer.plan(request).unwrap();
        plan.instances
            .get_mut(&InstanceId(1))
            .unwrap()
            .preferred_origin = plan.instances[&InstanceId(0)].preferred_origin;

        assert!(instance_owner_collides(&plan, InstanceId(1), request));

        plan.instances
            .get_mut(&InstanceId(1))
            .unwrap()
            .preferred_origin
            .x += 100;
        assert!(!instance_owner_collides(&plan, InstanceId(1), request));
    }

    #[test]
    fn adjacent_parallel_port_access_envelopes_are_a_cell_collision() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
        };
        let mut plan = TopologyAwareSeedPlacer.plan(request).unwrap();
        let first = plan.instances[&InstanceId(0)].preferred_origin;
        plan.instances
            .get_mut(&InstanceId(1))
            .unwrap()
            .preferred_origin = Anchor {
            z: first.z + 1,
            ..first
        };

        assert!(instance_owner_collides(&plan, InstanceId(1), request));
    }

    #[test]
    fn macro_access_envelope_covers_every_topology_input_ordinal() {
        let graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into(), "b".into(), "c".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("y", &["a", "b", "c"])],
            },
            &Library::default_library(),
        )
        .unwrap();

        let bounds = macro_access_envelope(&graph.instances[0])
            .unwrap()
            .by_facing[usize::from(CellFacing::NORTH.index())];

        assert!(bounds.min_x <= -2);
        assert!(bounds.max_x >= 2);
        assert!(bounds.max_z >= 2);
    }

    #[test]
    fn initial_segment_a_placement_keeps_all_cell_access_envelopes_disjoint() {
        let evaluator = legacy_benchmark_evaluator().unwrap();
        let fixture = evaluator.fixture("segment_a").unwrap();
        let graph =
            InstanceGraph::one_to_one(fixture.lowered_netlist(), &Library::default_library())
                .unwrap();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
        };
        let plan = TopologyAwareSeedPlacer.plan(request).unwrap();

        for instance in plan.instances.keys().copied() {
            assert!(
                !instance_owner_collides(&plan, instance, request),
                "instance {instance:?} overlaps another cell's routing access envelope"
            );
        }
    }

    #[test]
    fn segment_a_owner_separation_finds_the_nearest_legal_shell() {
        let evaluator = legacy_benchmark_evaluator().unwrap();
        let fixture = evaluator.fixture("segment_a").unwrap();
        let graph =
            InstanceGraph::one_to_one(fixture.lowered_netlist(), &Library::default_library())
                .unwrap();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
        };
        let baseline = TopologyAwareSeedPlacer.plan(request).unwrap();
        let repaired = TopologyAwareSeedPlacer
            .plan_with_repairs(
                request,
                &[LayoutRepair::SeparateOwners {
                    source_owner: LayoutOwner::Instance(InstanceId(1)),
                    sink_owner: LayoutOwner::Instance(InstanceId(23)),
                    axis: SeparationAxis::Lateral,
                    ordinal: 0,
                }],
            )
            .unwrap();

        assert!(
            repaired.instances[&InstanceId(1)] != baseline.instances[&InstanceId(1)]
                || repaired.instances[&InstanceId(23)] != baseline.instances[&InstanceId(23)],
            "a recorded separation repair must change one of its named owners"
        );
    }

    #[test]
    fn overlapping_automatic_boundary_bodies_are_rejected_before_materialisation() {
        let graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("y", &["a", "b"])],
            },
            &Library::default_library(),
        )
        .unwrap();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
        };
        let mut plan = TopologyAwareSeedPlacer.plan(request).unwrap();
        plan.automatic_inputs
            .insert(PortId(1), plan.automatic_inputs[&PortId(0)]);

        assert!(super::boundary_owner_collides(
            &plan,
            PhysicalEndpointId::PrimaryInput(PortId(1)),
            request,
        ));
    }

    #[test]
    fn cell_port_access_cannot_overlap_an_automatic_boundary_runway() {
        let graph = two_stage_graph();
        let analysis = analyse_instance_dag(&graph).unwrap();
        let pins = BTreeMap::new();
        let request = SeedPlacementRequest {
            graph: &graph,
            analysis: &analysis,
            pins: &pins,
        };
        let mut plan = TopologyAwareSeedPlacer.plan(request).unwrap();
        let input = plan.automatic_inputs[&PortId(0)];
        plan.instances
            .get_mut(&InstanceId(0))
            .unwrap()
            .preferred_origin = Anchor {
            x: input.x + 4,
            z: input.z,
            ..input
        };
        plan.instances
            .get_mut(&InstanceId(1))
            .unwrap()
            .preferred_origin
            .x += 100;

        assert!(instance_owner_collides(&plan, InstanceId(0), request));
    }

    #[test]
    fn pinned_boundary_promotion_keeps_pin_and_placement_geometry() {
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
        assert_eq!(repaired.instances, baseline.instances);
        assert_eq!(repaired.owner_offsets, baseline.owner_offsets);
        assert_ne!(repaired.fingerprint, baseline.fingerprint);
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

        assert_eq!(pose.facing, CellFacing::NORTH);
        assert_eq!(pose.preferred_origin, Anchor { x: 0, y: 1, z: 6 });
        assert_eq!(input.position, Position::new(0, 0, 0));
        assert_eq!(output.position, Position::new(0, 0, -1));
        assert_eq!(
            input.position.offset(input.direction),
            Position::new(0, 0, 1)
        );
        assert_eq!(
            output.position.offset(output.direction),
            Position::new(0, 0, -2)
        );
        assert_eq!(
            super::input_terminal(Primitive::Torch, pose.facing, pose.preferred_origin),
            Anchor { x: 0, y: 1, z: 7 }
        );
        assert_eq!(
            crate::compile::fragment_synth::terminal_geometry::primitive_input_terminal(
                Primitive::Torch,
                pose.facing,
                pose.preferred_origin,
                0,
            )
            .unwrap()
            .terminal,
            Anchor { x: -1, y: 1, z: 6 }
        );
        assert_eq!(
            super::output_terminal(Primitive::Torch, pose.facing, pose.preferred_origin),
            Anchor { x: 0, y: 1, z: 4 }
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

        assert_eq!(pose.facing, CellFacing::WEST);
        assert_eq!(pose.preferred_origin, Anchor { x: 0, y: 1, z: 6 });
        assert_eq!(rear.position, Position::new(0, 0, 0));
        assert_eq!(front.position, Position::new(0, 0, 0));
        assert_eq!(
            rear.position.offset(rear.direction),
            Position::new(-1, 0, 0)
        );
        assert_eq!(
            front.position.offset(front.direction),
            Position::new(1, 0, 0)
        );
        assert_eq!(
            super::input_terminal(Primitive::Repeater, pose.facing, pose.preferred_origin),
            Anchor { x: -1, y: 1, z: 6 }
        );
        assert_eq!(
            super::output_terminal(Primitive::Repeater, pose.facing, pose.preferred_origin),
            Anchor { x: 1, y: 1, z: 6 }
        );
    }

    #[test]
    fn facing_keeps_the_cell_output_on_the_dag_forward_axis() {
        let graph = InstanceGraph::one_to_one(
            &Netlist {
                inputs: vec!["a".into(), "b".into(), "c".into()],
                outputs: vec!["y".into()],
                gates: vec![nor("y", &["a", "b", "c"])],
            },
            &Library::default_library(),
        )
        .unwrap();
        let instance = &graph.instances[0];
        let origin = Anchor { x: 0, y: 1, z: 0 };
        let frame = PlacementFrame {
            forward: Facing::East,
            lateral: Facing::South,
            origin,
        };
        let facing = choose_instance_facing_with_tracks(
            instance,
            origin,
            Anchor { x: -6, y: 1, z: 0 },
            Anchor { x: 6, y: 1, z: 0 },
            frame,
            &BTreeMap::from([(0, 0), (1, 6), (2, -6)]),
            None,
        )
        .unwrap();
        let crate::compile::fragment_synth::topology::OutputSpec::Primitive(output_id) =
            instance.expanded.topology.output
        else {
            panic!("NOR topology must expose a primitive output");
        };
        let positions = super::primitive_positions(instance);
        let spec = instance
            .expanded
            .topology
            .primitives
            .iter()
            .find(|primitive| primitive.id == output_id)
            .unwrap();
        let output = crate::compile::fragment_synth::terminal_geometry::primitive_output_terminal(
            spec.primitive,
            facing,
            super::primitive_world(origin, positions[&output_id], facing),
        )
        .unwrap();

        assert_eq!(output.allowed_exit, frame.forward);
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
        let without_hint = &graph.instances[0];
        assert_eq!(
            choose_instance_facing(without_hint, origin, source, target, Facing::East).unwrap(),
            CellFacing::NORTH
        );
        let mut with_hint = without_hint.clone();
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
        assert_eq!(
            first.frame,
            PlacementFrame {
                forward: Facing::East,
                lateral: Facing::South,
                origin: Anchor { x: 0, y: 1, z: 0 },
            }
        );
        assert_eq!(first.signal_tracks.len(), 3);
        assert!(first
            .signal_tracks
            .contains_key(&LogicalSignalId::PrimaryInput(PortId(0))));
        assert!(first
            .signal_tracks
            .contains_key(&LogicalSignalId::GateOutput(GateIndex(1))));
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
        assert_eq!(origins[&InstanceId(1)], 14);
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
