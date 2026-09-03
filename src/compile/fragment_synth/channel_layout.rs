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
    allocate_crossing_rows, lane_forward, lane_forward_from_end, plan_channel, ChannelNet,
    ChannelPlanError, LANE_MARGIN, LANE_PITCH,
};
use super::identity::PhysicalEndpointId;
use super::identity::{RouteId, RoutedSinkId};
use super::placement::{
    horizontal_unit, project_horizontal, LateralWindow, PlacementFrame, SeedPlacementAnalysis,
    ROW_GRID,
};
use super::seed::{reserve_route, step, step_many, SourceGeometry, TargetGeometry};
use crate::compile::geometry::Anchor;
use crate::compile::routing::{
    NonEmptyRouteSinks, PhysicalReservationKind, PhysicalReservationOwner, PhysicalReservations,
    PhysicalRouter, PlacedBlock, RealisedRouteTree, RouteEndpoint, RouteRequest, RouteSink,
    RouteTarget, RouteTerminalKind, RouterLimits, TerminalContract, TerminalRequirement,
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

/// One net as the seed routes it: a source and its ordered sinks.
#[derive(Debug, Clone)]
pub(crate) struct NetGeometry {
    pub source: PhysicalEndpointId,
    pub source_geometry: SourceGeometry,
    pub sinks: Vec<(PhysicalEndpointId, TargetGeometry)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ChannelLayoutError {
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
}

/// The cells the plan hands to the router.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChannelLayout {
    /// Channel cells closed for every route.
    pub closed: BTreeSet<Anchor>,
    /// Cells one net owns; every other route sees them as keep-outs.
    pub private: BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>>,
    /// Lane forward coordinate per net and channel, for tests and reports.
    pub lanes: Vec<BTreeMap<PhysicalEndpointId, i32>>,
    /// Staircase floors a net's box stub needs, reserved for the net's route
    /// before any route runs so no route conducts through them.
    pub floors: BTreeMap<PhysicalEndpointId, Vec<PlacedBlock>>,
}

/// How far beyond the placed macros the closed channel layers extend, inside
/// the lateral window.
const LATERAL_MARGIN: i32 = 32;
/// Free forward cells of the turnaround channel beyond the last column.
const TURNAROUND_CHANNEL: i32 = 40;
/// Closed forward cells before the first and after the last column.
const FORWARD_MARGIN: i32 = 8;

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

pub(crate) fn plan_channel_layout(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    placement_frame: PlacementFrame,
    window: LateralWindow,
    nets: &[NetGeometry],
    router: &dyn PhysicalRouter,
    reservations: &PhysicalReservations,
    limits: RouterLimits,
) -> Result<ChannelLayout, ChannelLayoutError> {
    let frame = Frame {
        forward: placement_frame.forward,
        lateral: placement_frame.lateral,
    };
    let ground = nets
        .iter()
        .map(|net| net.source_geometry.route_anchor.y)
        .min()
        .unwrap_or(1);

    // ---- columns -------------------------------------------------------
    let level_of_instance = |instance| {
        analysis
            .nodes
            .get(&instance)
            .map(|facts| facts.forward_level as i64)
    };
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
    for (primitive, placement) in &candidate.placements {
        if let Some(level) = level_of_instance(primitive.instance) {
            for block in &placement.blocks {
                occupy(&mut by_level, level, block.at);
            }
        }
    }
    for junction in candidate.junctions.values() {
        if let Some(level) = level_of_instance(junction.id) {
            for cell in &junction.cells {
                occupy(&mut by_level, level, cell.at);
            }
        }
    }
    let (min_level, max_level) = match (by_level.keys().next(), by_level.keys().last()) {
        (Some(&min), Some(&max)) => (min, max),
        _ => return Ok(ChannelLayout::default()),
    };
    for (endpoint, boundary) in &candidate.boundaries {
        let level = match endpoint {
            PhysicalEndpointId::PrimaryInput(_) => min_level - 1,
            PhysicalEndpointId::DeclaredOutput(_) => max_level + 1,
            _ => continue,
        };
        for block in &boundary.blocks {
            occupy(&mut by_level, level, block.at);
        }
    }
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
    // Endpoint cells belong to their columns as well.
    for net in nets {
        if let Some(level) = raw_level(net.source) {
            occupy(&mut by_level, level, net.source_geometry.route_anchor);
        }
        for (endpoint, geometry) in &net.sinks {
            if let Some(level) = raw_level(*endpoint) {
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
    }
    // Pinned inputs and pinned outputs whose forward extents overlap form
    // one pin column: the levels march away from both, and the output nets
    // come back to it through every column in between.
    let mut output_level = max_level + 1;
    if let (Some(inputs), Some(outputs)) = (
        by_level.get(&(min_level - 1)).cloned(),
        by_level.get(&(max_level + 1)).cloned(),
    ) {
        if inputs.min_forward <= outputs.max_forward && outputs.min_forward <= inputs.max_forward {
            by_level.remove(&(max_level + 1));
            let column = by_level
                .get_mut(&(min_level - 1))
                .expect("the input column was just read");
            column.min_forward = column.min_forward.min(outputs.min_forward);
            column.max_forward = column.max_forward.max(outputs.max_forward);
            column.blocked_laterals.extend(outputs.blocked_laterals);
            output_level = min_level - 1;
        }
    }
    let endpoint_level = |endpoint: PhysicalEndpointId| -> Option<i64> {
        if matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)) {
            Some(output_level)
        } else {
            raw_level(endpoint)
        }
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
    // A virtual empty column beyond the last one gives the last level's
    // sources a channel to leave into; a net whose sinks all lie behind its
    // source climbs onto a lane there, runs to a free crossing row, and
    // comes back through the last column at ground.
    if let Some(last) = columns.last().cloned() {
        let start = last.max_forward + 1;
        levels.push(levels.last().copied().unwrap_or(0) + 1);
        columns.push(Column {
            min_forward: start + TURNAROUND_CHANNEL,
            max_forward: start + TURNAROUND_CHANNEL,
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
    let source_line = |net: &NetGeometry| -> Result<Line, ChannelLayoutError> {
        let level = endpoint_level(net.source).ok_or(ChannelLayoutError::UnplacedEndpoint {
            endpoint: net.source,
        })?;
        let column = column_index(level).ok_or(ChannelLayoutError::UnplacedEndpoint {
            endpoint: net.source,
        })?;
        let geometry = net.source_geometry;
        let (channel, row) = if geometry.allowed_exit == frame.forward.opposite() {
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
            depth: frame.forward_of(geometry.route_anchor),
        })
    };
    let sink_line = |endpoint: PhysicalEndpointId,
                     geometry: &TargetGeometry,
                     source_column: usize|
     -> Result<Line, ChannelLayoutError> {
        let level =
            endpoint_level(endpoint).ok_or(ChannelLayoutError::UnplacedEndpoint { endpoint })?;
        let column =
            column_index(level).ok_or(ChannelLayoutError::UnplacedEndpoint { endpoint })?;
        let approach = step_many(
            geometry.terminal,
            geometry.allowed_entry,
            entry_depth(geometry),
        );
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
        for (endpoint, geometry) in &net.sinks {
            sinks.push(sink_line(*endpoint, geometry, source.column)?);
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
            net.source,
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
        let mut members = Vec::new();
        for net in nets {
            let source = &net.source_geometry;
            if matches!(net.source, PhysicalEndpointId::PrimaryInput(_))
                && pinned(net.source)
                && source.allowed_exit != frame.forward
            {
                members.push(StubPort {
                    net: net.source,
                    sink: None,
                    endpoint: net.source,
                    anchor: source.route_anchor,
                    exit: source.allowed_exit,
                    support: step(source.route_anchor, source.allowed_exit.opposite()),
                    entry_depth: 3,
                });
            }
            for (index, (endpoint, geometry)) in net.sinks.iter().enumerate() {
                if !matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)) || !pinned(*endpoint)
                {
                    continue;
                }
                let line = lines[&net.source].sinks[index];
                let entry = frame.forward_of(step(geometry.terminal, geometry.allowed_entry));
                let inside = if line.channel == line.column {
                    entry <= edge_of(&line)
                } else {
                    entry >= edge_of(&line)
                };
                if inside {
                    members.push(StubPort {
                        net: net.source,
                        sink: Some(index),
                        endpoint: *endpoint,
                        anchor: geometry.terminal,
                        exit: geometry.allowed_entry,
                        support: geometry.support,
                        entry_depth: entry_depth(geometry),
                    });
                }
            }
        }
        members.sort_by_key(|port| {
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
        let mut used_rows = BTreeMap::<usize, Vec<i32>>::new();
        for (stub_index, port) in members.iter().enumerate() {
            let line = line_of(&lines, port);
            let edge = edge_of(&line);
            let (entry_from, support_step) = if line.channel == line.column {
                (frame.forward.opposite(), frame.forward)
            } else {
                (frame.forward, frame.forward.opposite())
            };
            // The stub's end is only a label for the router; the real
            // terminal is the port's own.
            let label = match port.endpoint {
                PhysicalEndpointId::DeclaredOutput(id) | PhysicalEndpointId::PrimaryInput(id) => id,
                _ => continue,
            };
            let route = RouteId(u32::MAX - u32::try_from(stub_index).unwrap_or(0));
            let own_entry = step(port.anchor, port.exit);
            let mut candidates = (lateral_lo..=lateral_hi)
                .filter(|c| c.rem_euclid(2) == grid_phase)
                .filter(|c| {
                    used_rows
                        .get(&line.column)
                        .is_none_or(|rows| rows.iter().all(|row| (row - c).abs() >= 2))
                })
                .collect::<Vec<_>>();
            candidates.sort_by_key(|c| ((c - line.natural).abs(), *c));
            let mut found = None;
            for c in candidates {
                let end = frame.cell(edge, c, ground);
                let before = step(end, entry_from);
                let support = step(end, support_step);
                let mut scratch = reservations.clone();
                for tree in &committed {
                    reserve_route(&mut scratch, tree, &BTreeSet::new());
                }
                // Nothing may run next to another pinned port's anchor or
                // support, and every other port's entry cells stay free with
                // the same clearance a laid wire would get.
                for other in &members {
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
                        for direction in [Facing::North, Facing::East, Facing::South, Facing::West]
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
                            protect(
                                step_many(other.anchor, other.exit, distance),
                                PhysicalReservationKind::Conductor(crate::compile::dust()),
                            );
                        }
                    }
                }
                if scratch.get(&end).is_some() || scratch.get(&before).is_some() {
                    continue;
                }
                let sinks = vec![RouteSink {
                    id: RoutedSinkId { route, ordinal: 0 },
                    endpoint: port.endpoint,
                    anchor: end,
                    allowed_entry: entry_from,
                    terminal: TerminalContract::Sink {
                        target: RouteTarget::DeclaredOutput(label),
                        support,
                        requirement: TerminalRequirement::Exact(RouteTerminalKind::BareMergeDust),
                    },
                }];
                let Ok(sinks) = NonEmptyRouteSinks::new(sinks) else {
                    continue;
                };
                let request = RouteRequest {
                    id: route,
                    source: RouteEndpoint {
                        id: port.net,
                        anchor: port.anchor,
                        allowed_exit: port.exit,
                        terminal: TerminalContract::Source {
                            signal_strength: MAX_SIGNAL_STRENGTH,
                        },
                    },
                    sinks: &sinks,
                    reservations: &scratch,
                    limits,
                };
                if let Ok(tree) = router.route(request) {
                    found = Some((c, tree));
                    break;
                }
            }
            let Some((c, tree)) = found else {
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
            used_rows.entry(line.column).or_default().push(c);
            committed.push(tree);
            stubs.insert((port.net, port.sink), Stub { cells, floors });
            let net = lines.get_mut(&port.net).expect("net exists");
            let line = match port.sink {
                Some(index) => &mut net.sinks[index],
                None => &mut net.source,
            };
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
        for cell in &junction.cells {
            occupied_cells.insert((frame.forward_of(cell.at), frame.lateral_of(cell.at)));
        }
    }
    for boundary in candidate.boundaries.values() {
        for block in &boundary.blocks {
            occupied_cells.insert((frame.forward_of(block.at), frame.lateral_of(block.at)));
        }
    }
    for net in nets {
        occupied_cells.insert((
            frame.forward_of(net.source_geometry.route_anchor),
            frame.lateral_of(net.source_geometry.route_anchor),
        ));
        for (_, geometry) in &net.sinks {
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
    // Each sink's own terminal, support and line cells, which its corridor
    // may touch.
    let mut own_line_cells = BTreeMap::<(PhysicalEndpointId, usize), BTreeSet<(i32, i32)>>::new();
    for net in nets {
        for (index, (_, geometry)) in net.sinks.iter().enumerate() {
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
            own_line_cells.insert((net.source, index), own);
        }
    }
    struct Escape {
        corridor: i32,
        depth: i32,
        natural: i32,
        /// Forward coordinate of the column edge the corridor starts from.
        edge: i32,
    }
    let mut escapes = BTreeMap::<(PhysicalEndpointId, usize), Escape>::new();
    {
        // Sinks grouped by column, in the order (depth from the preferred
        // edge, natural lateral, net, index).
        let mut per_column = BTreeMap::<usize, Vec<(PhysicalEndpointId, usize)>>::new();
        for (&id, net) in &lines {
            for (index, sink) in net.sinks.iter().enumerate() {
                if stubs.contains_key(&(id, Some(index))) {
                    continue;
                }
                per_column.entry(sink.column).or_default().push((id, index));
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
                let line = &lines[&id].sinks[index];
                (
                    depth_from(line, preferred_side(line)),
                    line.natural,
                    id,
                    index,
                )
            });
            let mut pending = order;
            let mut retry = Vec::new();
            for pass in 0..2 {
                for &(id, index) in &pending {
                    let line = lines[&id].sinks[index];
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
                    candidates.sort_by_key(|c| ((c - line.natural).abs(), *c));
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
                            net.sinks[index].row = c;
                            net.sinks[index].channel = side;
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
                let endpoint = nets
                    .iter()
                    .find(|net| net.source == id)
                    .and_then(|net| net.sinks.get(index))
                    .map_or(id, |(endpoint, _)| *endpoint);
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

    // ---- crossings -----------------------------------------------------
    // A net alive in channels c..d crosses every column strictly between.
    let mut crossings = BTreeMap::<(PhysicalEndpointId, usize), i32>::new();
    for (column, geometry) in columns
        .iter()
        .enumerate()
        .take(columns.len().saturating_sub(1))
        .skip(1)
    {
        let demands = lines
            .iter()
            .filter(|(_, net)| net.first_channel < column && net.last_channel >= column)
            .map(|(&id, net)| (id, net.source.row))
            .collect::<Vec<_>>();
        if demands.is_empty() {
            continue;
        }
        // Crossing rows share the parity of every endpoint row (the grid is
        // four cells, so the rows between grid rows keep two cells from
        // them), and no crossing may sit next to another net's row.
        let free = (lateral_lo..=lateral_hi)
            .filter(|lateral| lateral.rem_euclid(2) == grid_phase)
            .filter(|lateral| !geometry.blocked_laterals.contains(lateral))
            .collect::<BTreeSet<_>>();
        let rows = allocate_crossing_rows(&free, &demands)
            .map_err(|error| ChannelLayoutError::Crossing { column, error })?;
        for (id, row) in rows {
            crossings.insert((id, column), row);
        }
    }

    // ---- lanes per channel ---------------------------------------------
    let mut layout = ChannelLayout::default();
    let mut private = |id: PhysicalEndpointId, cell: Anchor| {
        layout.private.entry(id).or_default().insert(cell);
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
                    private(id, frame.cell(forward, interval.0, ground));
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
        let free_jog_rows = (lateral_lo..=lateral_hi)
            .filter(|row| row.rem_euclid(ROW_GRID) == grid_phase)
            .filter(|row| straight_rows.iter().all(|used| (used - row).abs() > 1))
            .collect::<BTreeSet<_>>();
        let plan = plan_channel(&channel_nets, &free_jog_rows)
            .map_err(|error| ChannelLayoutError::Plan { channel, error })?;
        // Lanes counted from the start edge and lanes counted from the end
        // edge must still keep one lane pitch between the two pools.
        let segment_forward = |segment: &super::channel_plan::Segment| -> i32 {
            if segment.from_end {
                lane_forward_from_end(end, segment.lane)
            } else {
                lane_forward(start, segment.lane)
            }
        };
        let needed = LANE_PITCH * i32::try_from(plan.lane_count).unwrap_or(i32::MAX / 4)
            + 2 * LANE_MARGIN
            - 2;
        if plan.lane_count > 0 && end - start + 1 < needed {
            return Err(ChannelLayoutError::ChannelTooNarrow {
                channel,
                level,
                available: end - start + 1,
                lanes: plan.lane_count,
                needed,
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
            let lane_at = |row: i32| -> i32 {
                let segment = segments
                    .iter()
                    .find(|segment| segment.interval.0 <= row && row <= segment.interval.1)
                    .unwrap_or(&segments[0]);
                segment_forward(segment)
            };
            lane_cells.insert(net.id, segment_forward(&segments[0]));
            for segment in segments {
                let lane = segment_forward(segment);
                for lateral in segment.interval.0..=segment.interval.1 {
                    private(net.id, frame.cell(lane, lateral, ground + 2));
                }
            }
            for &row in &net.source_rows {
                let lane = lane_at(row);
                for forward in start..=(lane - 2) {
                    private(net.id, frame.cell(forward, row, ground));
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
                        private(net.id, cell);
                    }
                    riser(
                        &mut layout.floors,
                        net.id,
                        frame.cell(lane, departure, ground + 1),
                    );
                    private(net.id, frame.cell(lane - 2, departure, ground));
                }
            }
            // A descent may leave the lane on the row itself or one cell to
            // either side and then step onto the row at ground: the trunk's
            // own repeaters (never on two neighbouring cells) can then block
            // at most one of the three departures.
            for &row in &net.sink_rows {
                let lane = lane_at(row);
                for forward in (lane + 2)..=end {
                    private(net.id, frame.cell(forward, row, ground));
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
                        private(net.id, cell);
                    }
                    riser(
                        &mut layout.floors,
                        net.id,
                        frame.cell(lane, departure, ground + 1),
                    );
                    private(net.id, frame.cell(lane + 2, departure, ground));
                }
            }
            for pair in segments.windows(2) {
                let Some(jog) = pair[0].jog else {
                    continue;
                };
                let from = segment_forward(&pair[0]);
                let to = segment_forward(&pair[1]);
                if from < to {
                    for departure in [jog - 1, jog, jog + 1] {
                        for cell in descend_to_above(from, departure) {
                            private(net.id, cell);
                        }
                        riser(
                            &mut layout.floors,
                            net.id,
                            frame.cell(from, departure, ground + 1),
                        );
                        private(net.id, frame.cell(from + 2, departure, ground));
                    }
                    for forward in (from + 2)..=(to - 2) {
                        private(net.id, frame.cell(forward, jog, ground));
                    }
                    for cell in climb_from_below(to, jog) {
                        private(net.id, cell);
                    }
                    riser(&mut layout.floors, net.id, frame.cell(to, jog, ground + 1));
                } else {
                    for departure in [jog - 1, jog, jog + 1] {
                        for cell in descend_to_below(from, departure) {
                            private(net.id, cell);
                        }
                        riser(
                            &mut layout.floors,
                            net.id,
                            frame.cell(from, departure, ground + 1),
                        );
                        private(net.id, frame.cell(from - 2, departure, ground));
                    }
                    for forward in (to + 2)..=(from - 2) {
                        private(net.id, frame.cell(forward, jog, ground));
                    }
                    for cell in climb_from_above(to, jog) {
                        private(net.id, cell);
                    }
                    riser(&mut layout.floors, net.id, frame.cell(to, jog, ground + 1));
                }
            }
        }
        layout.lanes.push(lane_cells);
    }

    // ---- lines inside columns and column crossings ---------------------
    for net in nets {
        let Some(net_lines) = lines.get(&net.source) else {
            continue;
        };
        let source = &net.source_geometry;
        let column = &columns[net_lines.source.column];
        if let Some(stub) = stubs.get(&(net.source, None)) {
            for &cell in &stub.cells {
                private(net.source, cell);
            }
            layout
                .floors
                .entry(net.source)
                .or_default()
                .extend(stub.floors.iter().cloned());
        } else if frame.along_forward(source.allowed_exit) {
            let anchor = frame.forward_of(source.route_anchor);
            let (from, to) = if source.allowed_exit == frame.forward {
                (anchor + 1, column.max_forward)
            } else {
                (column.min_forward, anchor - 1)
            };
            for forward in from..=to {
                private(
                    net.source,
                    frame.cell(forward, net_lines.source.row, ground),
                );
            }
        } else {
            for distance in 1..=3 {
                private(
                    net.source,
                    step_many(source.route_anchor, source.allowed_exit, distance),
                );
            }
            let approach = step_many(source.route_anchor, source.allowed_exit, 3);
            let (from, to) = if net_lines.source.channel >= net_lines.source.column {
                (frame.forward_of(approach) + 1, column.max_forward)
            } else {
                (column.min_forward, frame.forward_of(approach) - 1)
            };
            for forward in from..=to {
                private(
                    net.source,
                    frame.cell(forward, net_lines.source.row, ground),
                );
            }
        }
        for (index, (_, geometry)) in net.sinks.iter().enumerate() {
            if let Some(stub) = stubs.get(&(net.source, Some(index))) {
                for &cell in &stub.cells {
                    private(net.source, cell);
                }
                layout
                    .floors
                    .entry(net.source)
                    .or_default()
                    .extend(stub.floors.iter().cloned());
                continue;
            }
            // The fixed entry line itself.
            for distance in 1..=entry_depth(geometry) {
                private(
                    net.source,
                    step_many(geometry.terminal, geometry.allowed_entry, distance),
                );
            }
            // The corridor from the column edge and the run to the approach.
            if let Some(escape) = escapes.get(&(net.source, index)) {
                let (lo, hi) = (escape.edge.min(escape.depth), escape.edge.max(escape.depth));
                for forward in lo..=hi {
                    private(net.source, frame.cell(forward, escape.corridor, ground));
                }
                let (lo, hi) = (
                    escape.corridor.min(escape.natural),
                    escape.corridor.max(escape.natural),
                );
                for lateral in lo..=hi {
                    private(net.source, frame.cell(escape.depth, lateral, ground));
                }
            }
        }
        for (column, geometry) in columns
            .iter()
            .enumerate()
            .take(columns.len().saturating_sub(1))
            .skip(1)
        {
            if let Some(&row) = crossings.get(&(net.source, column)) {
                for forward in geometry.min_forward..=geometry.max_forward {
                    private(net.source, frame.cell(forward, row, ground));
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
    let forward_min = columns.first().map_or(0, |column| column.min_forward) - FORWARD_MARGIN;
    let forward_max = columns.last().map_or(0, |column| column.max_forward) + FORWARD_MARGIN;
    // The closed layers cover the whole margin, window or not: outside the
    // window is the callers' side, and the router must never see it open.
    for forward in forward_min..=forward_max {
        for lateral in (lateral_min - LATERAL_MARGIN)..=(lateral_max + LATERAL_MARGIN) {
            for y in ground..=(ground + 3) {
                let cell = frame.cell(forward, lateral, y);
                if !all_private.contains(&cell) {
                    layout.closed.insert(cell);
                }
            }
        }
    }
    Ok(layout)
}
