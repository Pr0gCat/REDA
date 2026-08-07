//! Routing-cost breakdown: decomposes every routed netlist edge's physical
//! length into structural parts -- column, ramp, track, gate entry -- and
//! counts how many repeaters each part forces, since repeaters are the only
//! source of delay a route can add (redstone dust conducts within the same
//! game tick; only torches, repeaters and lamps are ever scheduled -- see
//! `redstone::simulator::schedule` and `component::repeater_delay_game_ticks`).
//!
//! This module never writes to a `World`. It recomputes the exact geometry
//! `compile` computes -- by calling `build_floorplan`, `build_nets`,
//! `assign_shafts`, `assign_tracks` and `layout_row_z` verbatim, the same
//! functions `compile` itself calls -- and then reads the *already compiled*
//! `World` along the coordinates that geometry implies, counting actual dust
//! vs. repeater blocks. Two consequences of that split follow directly:
//!
//! - A bug here cannot change what `compile` places. This module only reads
//!   `CompiledCircuit::world`; it holds no `&mut World` anywhere.
//! - The repeater counts it reports are exactly what is really in the world,
//!   not a re-derivation of `lay_dust_run`/`lay_track`'s placement rules that
//!   could quietly drift from them. Every `scan_*` helper below mirrors one
//!   emission function's *loop bounds* (so it visits exactly the cells that
//!   function wrote) but classifies each cell by reading it back, rather than
//!   re-implementing the decision of *which* cells become repeaters.
//!
//! # Part categories, after 3D placement
//!
//! Levels now stack along Y (see `compile`'s module doc comment), so the
//! four parts keep their names but two of them widen slightly:
//!
//! - **Column**: only the driving pin's own run up to its first climb --
//!   there is no longer a flat "feed-through column" at all, because a net
//!   that skips levels climbs straight through them instead (see below).
//! - **Ramp**: every vertical climb or descent. A single band's fixed
//!   `RAMP_LENGTH` hop never has a repeater (too short to ever need one --
//!   `MAX_DUST_RUN` is 14). A skip-level edge's shaft (`climb_levels`) can
//!   span many bands, though, and a climb cannot refresh itself the way a
//!   flat run can (a repeater does not participate in `dust_connections`'
//!   diagonal rule at all) -- so a long enough climb detours sideways onto a
//!   repeater instead (see `climb_levels`'s doc comment), and this category
//!   counts those. Reading the count back like this, rather than assuming
//!   zero, is exactly the point: getting a climb's strength budget wrong is
//!   the kind of mistake that produces a dead wire on some inputs and not
//!   others, not a compile error, so this module treats "how many repeaters
//!   did a climb actually need" as a real question with a real answer, not
//!   an architectural constant.
//! - **Track**: every east-west dust run at a channel's own track plane,
//!   *plus* a skip-level shaft's landing correction (`land_shaft`'s
//!   perpendicular run onto the destination channel's own track Z, when the
//!   two channels did not already happen to share a track index) -- folded
//!   in here rather than given a fifth category, since it is structurally
//!   the same kind of thing: a flat run ending in a mandatory repeater,
//!   preparing a tap for the next leg.
//! - **GateEntry**: unchanged -- the final approach into a consuming gate's
//!   input socket.
//!
//! See `docs/superpowers/specs/2026-08-07-routing-cost-breakdown.md` for the
//! measurement this exists to support, and why the two representations below
//! (per-edge, and "distinct") both matter: `analyze` answers "how long is the
//! path to net N's slowest consumer", counting shared trunk wiring once per
//! consumer that depends on it (deliberately -- that is genuinely how much
//! wire sits in series ahead of that specific consumer); `distinct_totals`
//! answers "how many blocks did the router place, in total", counting the
//! same shared trunk once. Only the second is comparable to a whole-world
//! block count, which is exactly how it is validated in this module's tests.

use std::collections::{BTreeMap, HashMap};

use super::{
    approach_column, assign_shafts, assign_tracks, build_floorplan, build_nets, direction_from, gate_y,
    layout_row_z, place_nor_gate, plan_climb, shaft_diagonal_z, shaft_rail_offset, side_directions,
    socket_approach_corners, track_y, CompileError, CompiledCircuit, Exit, Floorplan, Net, Netlist, NorCell, Source,
    DESCEND_LENGTH, GATE_HALF_WIDTH, GATE_Y_OFFSET, ORIGIN_X, RAMP_LENGTH,
};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

// ---------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------

/// One structural part of a routed edge's physical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RoutePart {
    /// North-south dust run at a band's own `gate_y`: a driver's pin to its
    /// first climb.
    Column,
    /// A dust-staircase climb or descent: `RAMP_LENGTH` blocks within one
    /// band for an ordinary next-row hop (never a repeater -- too short to
    /// need one), or a whole multiple of `BAND_HEIGHT` for a skip-level
    /// shaft, which can need one or more (see `scan_climb`'s doc comment).
    Ramp,
    /// East-west dust run at a channel's own track plane: the portion of an
    /// assigned track between a net's entry point and one specific exit,
    /// plus a skip-level shaft's landing correction onto its destination
    /// channel's own track Z, when one was needed. A track can carry
    /// several nets (left-edge assignment), so this is only the calling
    /// net's own share of it, not the whole physical track.
    Track,
    /// The final approach into a consuming gate's input socket: a run north
    /// into the row, an optional corner turn (west/east inputs only, since
    /// their approach column does not line up with their socket), and the
    /// mandatory terminating repeater every socket needs.
    GateEntry,
}

pub const ALL_PARTS: [RoutePart; 4] =
    [RoutePart::Column, RoutePart::Ramp, RoutePart::Track, RoutePart::GateEntry];

/// One part's contribution: how many blocks (dust + repeaters), and how many
/// of those blocks are repeaters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartTotals {
    pub length: i64,
    pub repeaters: usize,
}

impl std::ops::Add for PartTotals {
    type Output = PartTotals;
    fn add(self, other: PartTotals) -> PartTotals {
        PartTotals { length: self.length + other.length, repeaters: self.repeaters + other.repeaters }
    }
}

impl std::ops::AddAssign for PartTotals {
    fn add_assign(&mut self, other: PartTotals) {
        *self = *self + other;
    }
}

/// One real netlist edge -- a specific signal driving a specific gate's
/// input -- decomposed into the physical route that carries it.
#[derive(Debug, Clone)]
pub struct EdgeRoute {
    /// The driving signal: a primary input's name, or a gate's output name.
    pub source: String,
    /// The consuming gate's output name and which of its inputs this is,
    /// formatted as `"<gate output>.in[<index>]"`.
    pub sink: String,
    /// How many channels (routing bands) this edge's signal had to cross --
    /// 1 for a next-level consumer, more if a skip-level shaft was needed to
    /// pass over intervening bands entirely.
    pub hops: usize,
    pub parts: BTreeMap<RoutePart, PartTotals>,
}

impl EdgeRoute {
    pub fn part(&self, part: RoutePart) -> PartTotals {
        self.parts.get(&part).copied().unwrap_or_default()
    }

    pub fn total(&self) -> PartTotals {
        ALL_PARTS.iter().fold(PartTotals::default(), |acc, &p| acc + self.part(p))
    }
}

/// Every real netlist edge in a compiled circuit, plus the left-edge track
/// assignment's own headline numbers (tracks per channel) for context.
pub struct RoutingReport {
    pub edges: Vec<EdgeRoute>,
    pub channel_count: usize,
    pub track_count: Vec<usize>,
}

// ---------------------------------------------------------------------
// Reusable geometry: recomputes exactly what `compile` computes
// ---------------------------------------------------------------------

/// `(floorplan, nets, row Z (shared by every band), per-channel track Z,
/// per-channel track count)`.
type Geometry = (Floorplan, Vec<Net>, i32, Vec<Vec<i32>>, Vec<usize>);

/// Recompute the floorplan, nets (with shafts and tracks already assigned)
/// and Z layout for `netlist` -- the same pure, world-free stages `compile`
/// runs, called here verbatim so this module's geometry cannot drift from
/// what actually got built.
fn recompute_geometry(netlist: &Netlist) -> Result<Geometry, CompileError> {
    for gate in &netlist.gates {
        for input in &gate.inputs {
            if !netlist.is_driven(input) {
                return Err(CompileError::UndrivenSignal(input.clone()));
            }
        }
    }
    for output in &netlist.outputs {
        if !netlist.gates.iter().any(|gate| &gate.output == output) {
            return Err(CompileError::UndrivenSignal(output.clone()));
        }
    }
    let order = netlist.topological_order().ok_or(CompileError::CyclicNetlist)?;

    let mut producer_of: HashMap<&str, usize> = HashMap::new();
    for (index, gate) in netlist.gates.iter().enumerate() {
        producer_of.insert(gate.output.as_str(), index);
    }

    let plan = build_floorplan(netlist, &order, &producer_of);
    let row_count = plan.rows.len();
    let channel_count = row_count.saturating_sub(1);
    let mut nets = build_nets(netlist, &order, &plan, &producer_of);

    let content_max_x =
        plan.centre_x.iter().chain(plan.lever_x.iter()).copied().max().unwrap_or(ORIGIN_X) + GATE_HALF_WIDTH;
    assign_shafts(&mut nets, content_max_x);
    let rail_offset = shaft_rail_offset(&nets);
    let track_count = assign_tracks(&plan, &mut nets, channel_count, rail_offset);
    let (row_z, track_z) = layout_row_z(channel_count, &track_count);

    Ok((plan, nets, row_z, track_z, track_count))
}

/// One gate cell's socket geometry per distinct input count (1..=3) --
/// `NorCell`'s offsets are relative, so they do not depend on where a given
/// gate actually sits, only on how many inputs it has.
fn cell_geometry_by_input_count(netlist: &Netlist) -> HashMap<usize, NorCell> {
    let mut cells = HashMap::new();
    let mut scratch = World::new(20, 8, 20);
    for gate in &netlist.gates {
        cells
            .entry(gate.inputs.len())
            .or_insert_with(|| place_nor_gate(&mut scratch, (8, GATE_Y_OFFSET, 8), gate.inputs.len()));
    }
    cells
}

// ---------------------------------------------------------------------
// World scanning: reads back what `compile` actually placed
// ---------------------------------------------------------------------

fn classify(world: &World, pos: Position) -> PartTotals {
    match world.get(pos.x, pos.y, pos.z).kind {
        BlockKind::Repeater => PartTotals { length: 1, repeaters: 1 },
        BlockKind::RedstoneWire => PartTotals { length: 1, repeaters: 0 },
        _ => PartTotals::default(),
    }
}

/// Mirrors `lay_dust_run`'s loop bounds exactly (same `start`, `direction`,
/// `stop_before`), so it visits precisely the cells that call wrote.
fn scan_dust_run(world: &World, start: Position, direction: Facing, stop_before: Position) -> PartTotals {
    let mut totals = PartTotals::default();
    let mut pos = start.offset(direction);
    while pos != stop_before {
        totals += classify(world, pos);
        pos = pos.offset(direction);
    }
    totals
}

/// Mirrors `lay_segment_to_corner`'s structure: a dust run up to the
/// mandatory refresh repeater, then the refresh repeater and the corner cell
/// themselves, both read back rather than assumed.
fn scan_to_corner(world: &World, start: Position, corner: Position) -> PartTotals {
    let direction = direction_from(start, corner);
    let refresh_point = corner.offset(direction.opposite());
    let mut totals = scan_dust_run(world, start, direction, refresh_point);
    totals += classify(world, refresh_point);
    totals += classify(world, corner);
    totals
}

/// Mirrors `lay_segment_to_socket`'s structure: a dust run up to the socket,
/// then the mandatory terminating repeater at the socket cell itself.
fn scan_to_socket(world: &World, start: Position, socket: Position) -> PartTotals {
    let direction = direction_from(start, socket);
    let mut totals = scan_dust_run(world, start, direction, socket);
    totals += classify(world, socket);
    totals
}

/// Mirrors `move_between_layers`'s loop exactly: one landing-dust cell per Y
/// level crossed between `entry` and `target_y`. The riser each step also
/// places is solid stone, not wire, so it is never counted here (`classify`
/// only recognises `Repeater`/`RedstoneWire`) -- a single-band ramp has no
/// repeaters at all, only the dust cells this counts.
fn scan_ramp(world: &World, entry: Position, direction: Facing, target_y: i32) -> PartTotals {
    let mut current = entry;
    let mut totals = PartTotals::default();
    if target_y >= current.y {
        while current.y != target_y {
            let riser = current.offset(direction);
            let landing = riser.up();
            totals += classify(world, landing);
            current = landing;
        }
    } else {
        while current.y != target_y {
            let stepped = current.offset(direction);
            let landing = Position::new(stepped.x, stepped.y - 1, stepped.z);
            totals += classify(world, landing);
            current = landing;
        }
    }
    totals
}

/// Mirrors `climb_levels`'s loop exactly, `needs_detour` (from `plan_climb`,
/// replayed on the same `incoming_strength` the caller passed -- always
/// `MAX_SIGNAL_STRENGTH` for a skip-level shaft's climb, since `move_
/// through_shaft` always lands it on a mandatory-repeater correction first)
/// telling it precisely which levels detoured onto a repeater and which
/// climbed straight through -- without this, a detour partway up (see
/// `plan_climb`'s doc comment) shifts every cell after it sideways, and a
/// scan that assumed a straight line would silently visit the wrong cells
/// for the rest of the climb. The solid risers are still never counted
/// (`classify` only recognises `Repeater`/`RedstoneWire`).
///
/// Returns the totals *and* the actual landing position, since a detour
/// also changes where the climb ends up -- callers need that real position,
/// not the naive `entry` position offset by `levels`, to know where the
/// climb actually left off.
fn scan_climb(world: &World, entry: Position, direction: Facing, levels: i32, incoming_strength: u8) -> (PartTotals, Position) {
    let needs_detour = plan_climb(levels, incoming_strength);
    let detour_direction = side_directions(direction)[0];
    let mut current = entry;
    let mut totals = PartTotals::default();
    for &detour in &needs_detour {
        if detour {
            let repeater_pos = current.offset(detour_direction);
            totals += classify(world, repeater_pos);
            let fresh = repeater_pos.offset(detour_direction);
            totals += classify(world, fresh);
            current = fresh;
        }
        let riser = current.offset(direction);
        let landing = riser.up();
        totals += classify(world, landing);
        current = landing;
    }
    (totals, current)
}

/// One skip-level shaft hop's worth of `Track`/`Ramp` cells -- mirrors
/// `move_through_shaft`'s structure exactly (rail jog east, sweep to
/// `shaft_diagonal_z`, the climb, sweep back onto `target_z`, rail jog back
/// west onto the real tap), rather than the plain single fixed-X sweep an
/// earlier version of this module (and of `move_through_shaft` itself) used
/// -- see that function's doc comment for why the rail jogs exist at all.
/// Callers never need the landing position back: every caller here
/// recomputes the next slot's own tap straight from `Net::entry_column`,
/// same as `move_through_shaft`'s own real callers do.
struct ShaftScan {
    track: PartTotals,
    ramp: PartTotals,
}

fn scan_shaft(
    world: &World,
    origin: Position,
    row_z: i32,
    climb: i32,
    target_z: i32,
    rail_offset: i32,
) -> ShaftScan {
    let diagonal_z = shaft_diagonal_z(row_z);
    let mut track = PartTotals::default();

    let rail_entry = Position::new(origin.x + rail_offset, origin.y, origin.z);
    track += scan_to_corner(world, origin, rail_entry);

    let pre_corner = Position::new(rail_entry.x, rail_entry.y, diagonal_z);
    track += scan_to_corner(world, rail_entry, pre_corner);

    let (ramp, landing) = scan_climb(world, pre_corner, Facing::East, climb, MAX_SIGNAL_STRENGTH);

    let post_corner = Position::new(landing.x, landing.y, target_z);
    track += scan_to_corner(world, landing, post_corner);

    let destination = Position::new(post_corner.x - rail_offset, post_corner.y, post_corner.z);
    track += scan_to_corner(world, post_corner, destination);

    ShaftScan { track, ramp }
}

/// The portion of an east-west track between `source_x` (exclusive -- it is
/// the ramp's own landing cell, already counted by `scan_ramp`) and
/// `target_x` (inclusive). Because `lay_track`'s repeater placement is
/// strictly causal -- source-outward, never looking past the cell it is
/// deciding -- this prefix is exactly what a signal travelling from
/// `source_x` to `target_x` actually passes through, whatever else the same
/// physical track was also asked to reach. Used for one edge's own path (it
/// is correct, and expected, for two edges on the same side of `source_x` to
/// each count the nearer edge's repeaters -- see `scan_full_track` for the
/// non-overlapping version used to total a whole track exactly once.
fn scan_track_to(world: &World, source_x: i32, target_x: i32, y: i32, z: i32) -> PartTotals {
    if target_x == source_x {
        return PartTotals::default();
    }
    let step = if target_x > source_x { 1 } else { -1 };
    let mut totals = PartTotals::default();
    let mut x = source_x + step;
    loop {
        totals += classify(world, Position::new(x, y, z));
        if x == target_x {
            break;
        }
        x += step;
    }
    totals
}

/// A whole track's cells, counted exactly once each -- west run and east run
/// from `source_x`, matching `lay_track`'s own two-direction loop. Unlike
/// `scan_track_to` called once per exit, this must be called once per
/// `(net, slot)`, or two exits on the same side of `source_x` would have the
/// nearer one's cells counted twice (once for itself, once again as a prefix
/// of the farther one's scan).
fn scan_full_track(world: &World, source_x: i32, lo: i32, hi: i32, y: i32, z: i32) -> PartTotals {
    let mut totals = PartTotals::default();
    if lo < source_x {
        totals += scan_track_to(world, source_x, lo, y, z);
    }
    if hi > source_x {
        totals += scan_track_to(world, source_x, hi, y, z);
    }
    totals
}

fn source_pin(netlist: &Netlist, compiled: &CompiledCircuit, source: Source) -> Position {
    match source {
        Source::Lever(i) => {
            let (x, y, z) = compiled.input_positions[&netlist.inputs[i]];
            Position::new(x, y, z).offset(Facing::North)
        }
        Source::Gate(g) => {
            let (x, y, z) = compiled.gate_output_positions[&netlist.gates[g].output];
            Position::new(x, y, z).offset(super::OUTPUT_DIRECTION)
        }
    }
}

// ---------------------------------------------------------------------
// Per-edge report
// ---------------------------------------------------------------------

/// Decompose every real netlist edge of a compiled circuit into its physical
/// route. `compiled` must be the result of calling `compile(netlist)` --
/// this only reads its `World`, it never rebuilds one.
pub fn analyze(netlist: &Netlist, compiled: &CompiledCircuit) -> Result<RoutingReport, CompileError> {
    let (plan, nets, row_z, track_z, track_count) = recompute_geometry(netlist)?;
    let rail_offset = shaft_rail_offset(&nets);
    let cell_of_count = cell_geometry_by_input_count(netlist);
    let world = &compiled.world;

    let mut edges = Vec::new();
    for net in &nets {
        let source_label = match net.source {
            Source::Lever(i) => netlist.inputs[i].clone(),
            Source::Gate(g) => netlist.gates[g].output.clone(),
        };
        let pin = source_pin(netlist, compiled, net.source);

        for (slot, sinks) in net.sinks.iter().enumerate() {
            for &(gate, input_index) in sinks {
                let mut parts: BTreeMap<RoutePart, PartTotals> = BTreeMap::new();

                // Column-in and the first climb: once per net, always
                // present, regardless of which slot this particular edge
                // ends at.
                let channel0 = net.channels[0];
                let z0 = track_z[channel0][net.tracks[0]];
                let entry0 = Position::new(net.entry_column(0), gate_y(channel0), z0 + RAMP_LENGTH);
                *parts.entry(RoutePart::Column).or_default() +=
                    scan_dust_run(world, pin, Facing::North, entry0.offset(Facing::North));
                *parts.entry(RoutePart::Ramp).or_default() +=
                    scan_ramp(world, entry0, Facing::North, track_y(channel0));

                for i in 0..=slot {
                    let channel = net.channels[i];
                    let z = track_z[channel][net.tracks[i]];
                    let source_x = net.entry_column(i);

                    if i < slot {
                        // Intermediate hop: this net feeds a gate further
                        // away than the next row, so it climbs straight
                        // past this channel's own level via a shaft --
                        // always via `shaft_diagonal_z`, and always with a
                        // mandatory-repeater correction on both ends (see
                        // `shaft_diagonal_z`'s doc comment for why neither
                        // is ever skipped), so the climb itself always
                        // starts at `MAX_SIGNAL_STRENGTH`.
                        let hop_x = net.hop_exit[i];
                        *parts.entry(RoutePart::Track).or_default() +=
                            scan_track_to(world, source_x, hop_x, track_y(channel), z);

                        let origin = Position::new(hop_x, track_y(channel), z);
                        let next_channel = net.channels[i + 1];
                        let climb = track_y(next_channel) - track_y(channel);
                        let target_z = track_z[next_channel][net.tracks[i + 1]];
                        let shaft = scan_shaft(world, origin, row_z, climb, target_z, rail_offset);
                        *parts.entry(RoutePart::Track).or_default() += shaft.track;
                        *parts.entry(RoutePart::Ramp).or_default() += shaft.ramp;
                    } else {
                        // Final slot: the real socket this edge ends at.
                        let exit_x = approach_column(plan.centre_x[gate], input_index);
                        *parts.entry(RoutePart::Track).or_default() +=
                            scan_track_to(world, source_x, exit_x, track_y(channel), z);

                        *parts.entry(RoutePart::Ramp).or_default() += scan_ramp(
                            world,
                            Position::new(exit_x, track_y(channel), z),
                            Facing::South,
                            gate_y(channel + 1),
                        );

                        let landing = Position::new(exit_x, gate_y(channel + 1), z + DESCEND_LENGTH);
                        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
                        let (dx, dy, dz) = cell.input_offsets[input_index];
                        let socket =
                            Position::new(plan.centre_x[gate] + dx, gate_y(channel + 1) + dy, row_z + dz);
                        let mut entry_total = PartTotals::default();
                        let mut current = landing;
                        for corner in socket_approach_corners(landing, socket, row_z) {
                            entry_total += scan_to_corner(world, current, corner);
                            current = corner;
                        }
                        entry_total += scan_to_socket(world, current, socket);
                        *parts.entry(RoutePart::GateEntry).or_default() += entry_total;
                    }
                }

                let sink_label = format!("{}.in[{}]", netlist.gates[gate].output, input_index);
                edges.push(EdgeRoute { source: source_label.clone(), sink: sink_label, hops: slot + 1, parts });
            }
        }
    }

    Ok(RoutingReport { edges, channel_count: track_count.len(), track_count })
}

// ---------------------------------------------------------------------
// Whole-circuit validation
// ---------------------------------------------------------------------

/// The same geometry as `analyze`, but summed once per physical segment
/// instead of once per edge that depends on it -- so unlike
/// `RoutingReport::edges`, which deliberately counts a shared trunk once for
/// every consumer downstream of it, this total is directly comparable to a
/// whole-world block count. Broken down by part, so it doubles as "where did
/// this circuit's repeaters actually go" at the whole-circuit level.
pub fn distinct_totals_by_part(
    netlist: &Netlist,
    compiled: &CompiledCircuit,
) -> Result<BTreeMap<RoutePart, PartTotals>, CompileError> {
    let (plan, nets, row_z, track_z, _track_count) = recompute_geometry(netlist)?;
    let rail_offset = shaft_rail_offset(&nets);
    let cell_of_count = cell_geometry_by_input_count(netlist);
    let world = &compiled.world;

    let mut total: BTreeMap<RoutePart, PartTotals> = BTreeMap::new();
    for net in &nets {
        let pin = source_pin(netlist, compiled, net.source);

        for (slot, &channel) in net.channels.iter().enumerate() {
            let z = track_z[channel][net.tracks[slot]];
            let source_x = net.entry_column(slot);
            let entry = Position::new(source_x, gate_y(channel), z + RAMP_LENGTH);

            if slot == 0 {
                *total.entry(RoutePart::Column).or_default() +=
                    scan_dust_run(world, pin, Facing::North, entry.offset(Facing::North));
                *total.entry(RoutePart::Ramp).or_default() += scan_ramp(world, entry, Facing::North, track_y(channel));
            }

            let (lo, hi) = net.span(slot, &plan.centre_x);
            *total.entry(RoutePart::Track).or_default() += scan_full_track(world, source_x, lo, hi, track_y(channel), z);

            for exit in net.exits(slot, &plan.centre_x) {
                match exit {
                    Exit::Socket { gate, input_index, .. } => {
                        *total.entry(RoutePart::Ramp).or_default() += scan_ramp(
                            world,
                            Position::new(exit.x(), track_y(channel), z),
                            Facing::South,
                            gate_y(channel + 1),
                        );
                        let landing = Position::new(exit.x(), gate_y(channel + 1), z + DESCEND_LENGTH);
                        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
                        let (dx, dy, dz) = cell.input_offsets[input_index];
                        let socket =
                            Position::new(plan.centre_x[gate] + dx, gate_y(channel + 1) + dy, row_z + dz);
                        let mut entry_total = PartTotals::default();
                        let mut current = landing;
                        for corner in socket_approach_corners(landing, socket, row_z) {
                            entry_total += scan_to_corner(world, current, corner);
                            current = corner;
                        }
                        entry_total += scan_to_socket(world, current, socket);
                        *total.entry(RoutePart::GateEntry).or_default() += entry_total;
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let origin = Position::new(x, track_y(channel), z);
                        let next_channel = net.channels[next_slot];
                        let climb = track_y(next_channel) - track_y(channel);
                        let target_z = track_z[next_channel][net.tracks[next_slot]];
                        let shaft = scan_shaft(world, origin, row_z, climb, target_z, rail_offset);
                        *total.entry(RoutePart::Track).or_default() += shaft.track;
                        *total.entry(RoutePart::Ramp).or_default() += shaft.ramp;
                    }
                }
            }
        }
    }

    Ok(total)
}

/// The grand total across every part -- see `distinct_totals_by_part`.
pub fn distinct_totals(netlist: &Netlist, compiled: &CompiledCircuit) -> Result<PartTotals, CompileError> {
    let by_part = distinct_totals_by_part(netlist, compiled)?;
    Ok(ALL_PARTS.iter().fold(PartTotals::default(), |acc, p| acc + by_part.get(p).copied().unwrap_or_default()))
}

#[cfg(test)]
mod dbg_collision {
    use super::*;
    use crate::circuits::seven_segment::{build_single_segment_netlist, INPUT_NAMES};
    use crate::compile::compile;
    use crate::redstone::simulator::Simulator;

    #[test]
    fn live_trace_segment_a_at_0000() {
        let (netlist, output_signal) = build_single_segment_netlist(0);
        let compiled = compile(&netlist).unwrap();
        let mut sim = Simulator::new(compiled.world.clone());
        sim.run_until_stable(2000).unwrap();

        for &name in INPUT_NAMES.iter() {
            let (x, y, z) = compiled.input_positions[name];
            let mut state = sim.world().get(x, y, z).clone();
            state.lit = false;
            sim.world_mut().set(x, y, z, state);
            sim.run_until_stable(2000).unwrap();
        }

        let read = |sim: &Simulator, (x, y, z): (i32, i32, i32)| sim.world().get(x, y, z).lit;
        let read_power = |sim: &Simulator, (x, y, z): (i32, i32, i32)| sim.world().get(x, y, z).power;

        for g in ["g8", "g9", "g10"] {
            let pos = compiled.gate_output_positions[g];
            println!("{g} output torch lit={} pos={:?}", read(&sim, pos), pos);
        }
        let out_pos = compiled.output_positions[&output_signal];
        println!("output lamp lit={}", read(&sim, out_pos));

        // Channel4 track0 line: y=23, z=28 (from earlier geometry dump).
        for x in [10, 50, 88, 150, 182, 185, 186] {
            println!("track4/0 x={x} y=23 z=28 power={:?}", read_power(&sim, (x, 23, 28)));
        }

        println!("--- net22 destination jog region, y=23 z=28, x=180..210 ---");
        for x in 180..=210 {
            let b = sim.world().get(x, 23, 28);
            println!("x={x} kind={:?} lit={} power={} facing={:?}", b.kind, b.lit, b.power, b.facing);
        }
        println!("--- net22 post_corner climb landing region, y=23, z=40..50, x=200..210 ---");
        for x in 200..=210 {
            for z in 40..=50 {
                let b = sim.world().get(x, 23, z);
                if b.kind != crate::redstone::world::block::BlockKind::Air {
                    println!("x={x} z={z} kind={:?} lit={} power={} facing={:?}", b.kind, b.lit, b.power, b.facing);
                }
            }
        }
    }


    #[test]
    fn find_pre_post_correction_collisions() {
        let (netlist, _label) = build_single_segment_netlist(0);
        let (plan, nets, row_z, track_z, _track_count) = recompute_geometry(&netlist).unwrap();
        let diagonal_z = shaft_diagonal_z(row_z);
        let rail_offset = shaft_rail_offset(&nets);
        println!("rail_offset={rail_offset}");

        // For every net's shaft hop, print the pre-correction sweep (fixed X,
        // fixed Y = track_y(origin channel), Z from origin track Z to
        // diagonal_z) and post-correction sweep (fixed X, fixed Y =
        // track_y(dest channel), Z from diagonal_z to dest track Z).
        struct Sweep {
            net: usize,
            slot: usize,
            kind: &'static str,
            x: i32,
            y: i32,
            z_lo: i32,
            z_hi: i32,
        }
        let mut sweeps = Vec::new();
        for (n, net) in nets.iter().enumerate() {
            for slot in 0..net.channels.len() {
                if slot + 1 >= net.channels.len() {
                    continue;
                }
                let channel = net.channels[slot];
                let next_channel = net.channels[slot + 1];
                let z = track_z[channel][net.tracks[slot]];
                let x = net.hop_exit[slot] + rail_offset;
                let (lo, hi) = if z < diagonal_z { (z, diagonal_z) } else { (diagonal_z, z) };
                sweeps.push(Sweep {
                    net: n,
                    slot,
                    kind: "pre",
                    x,
                    y: track_y(channel),
                    z_lo: lo,
                    z_hi: hi,
                });
                let target_z = track_z[next_channel][net.tracks[slot + 1]];
                let landing_x = net.hop_entry[slot] + rail_offset;
                let (lo2, hi2) = if target_z < diagonal_z { (target_z, diagonal_z) } else { (diagonal_z, target_z) };
                sweeps.push(Sweep {
                    net: n,
                    slot,
                    kind: "post",
                    x: landing_x,
                    y: track_y(next_channel),
                    z_lo: lo2,
                    z_hi: hi2,
                });
            }
        }

        // For every channel, every track index's actual physical span (union
        // over every net/slot using that track index in that channel).
        let channel_count = track_z.len();
        let mut track_span: Vec<Vec<Option<(i32, i32)>>> =
            track_z.iter().map(|zs| vec![None; zs.len()]).collect();
        for net in &nets {
            for slot in 0..net.channels.len() {
                let channel = net.channels[slot];
                let track = net.tracks[slot];
                let (lo, hi) = net.span(slot, &plan.centre_x);
                let entry = &mut track_span[channel][track];
                *entry = Some(match entry {
                    Some((elo, ehi)) => ((*elo).min(lo), (*ehi).max(hi)),
                    None => (lo, hi),
                });
            }
        }

        for (n, net) in nets.iter().enumerate() {
            println!(
                "net {n}: source={:?} channels={:?} tracks={:?} hop_exit={:?} hop_entry={:?} sinks={:?}",
                match net.source {
                    Source::Lever(i) => format!("lever {} ({})", i, netlist.inputs[i]),
                    Source::Gate(g) => format!("gate {} ({})", g, netlist.gates[g].output),
                },
                net.channels,
                net.tracks,
                net.hop_exit,
                net.hop_entry,
                net.sinks
                    .iter()
                    .flatten()
                    .map(|&(g, i)| format!("{}.in[{}]", netlist.gates[g].output, i))
                    .collect::<Vec<_>>()
            );
        }

        println!("row_z={row_z} diagonal_z={diagonal_z}");
        for c in 0..channel_count {
            for (k, z) in track_z[c].iter().enumerate() {
                println!("channel {c} track {k}: z={z} span={:?} y={}", track_span[c][k], track_y(c));
            }
        }

        for s in &sweeps {
            // Does this sweep's fixed Y correspond to a channel, and does its
            // Z range cross any *other* track index's Z within that channel?
            let channel = (0..channel_count).find(|&c| track_y(c) == s.y);
            let Some(channel) = channel else { continue };
            for (k, &tz) in track_z[channel].iter().enumerate() {
                if tz > s.z_lo && tz < s.z_hi {
                    if let Some((lo, hi)) = track_span[channel][k] {
                        let hits = s.x >= lo && s.x <= hi;
                        println!(
                            "net {} slot {} {} sweep x={} y={} z=[{},{}] CROSSES channel {} track {} (z={}) span=[{},{}] -> HITS_SPAN={}",
                            s.net, s.slot, s.kind, s.x, s.y, s.z_lo, s.z_hi, channel, k, tz, lo, hi, hits
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
    use crate::compile::compile;

    /// The one load-bearing check for this whole module: `distinct_totals`'s
    /// repeater count must equal the number of repeater blocks `compile`
    /// actually placed in the world, for every reference circuit. If the
    /// scanning helpers above visited the wrong cells, or missed a segment
    /// kind, or double-counted one, this is where it would show up.
    fn assert_repeater_count_matches_world(netlist: &Netlist, label: &str) {
        let compiled = compile(netlist).expect("reference circuits must compile");
        let (sx, sy, sz) = compiled.world.size();
        let mut world_repeaters = 0usize;
        for x in 0..sx {
            for y in 0..sy {
                for z in 0..sz {
                    if compiled.world.get(x, y, z).kind == BlockKind::Repeater {
                        world_repeaters += 1;
                    }
                }
            }
        }

        let totals = distinct_totals(netlist, &compiled).expect("reference circuits must analyze");
        assert_eq!(
            totals.repeaters, world_repeaters,
            "{label}: distinct_totals's repeater count must match the world's actual repeater count"
        );
    }

    #[test]
    fn distinct_totals_matches_the_world_for_every_reference_circuit() {
        let (and4, _) = build_and4_netlist();
        assert_repeater_count_matches_world(&and4, "and4");

        let (full_adder, _) = build_full_adder_netlist();
        assert_repeater_count_matches_world(&full_adder, "full_adder");

        let (segment_a, _) = build_single_segment_netlist(0);
        assert_repeater_count_matches_world(&segment_a, "segment_a");

        let (seven_segment, _) = build_seven_segment_netlist();
        assert_repeater_count_matches_world(&seven_segment, "seven_segment");
    }

    #[test]
    fn every_edge_has_a_positive_total_length_and_at_least_one_repeater() {
        // Every socket termination is a mandatory repeater (redstone dust
        // cannot charge a block sideways), so no real edge can ever have
        // zero repeaters, whatever else its route looks like.
        let (and4, _) = build_and4_netlist();
        let compiled = compile(&and4).expect("and4 must compile");
        let report = analyze(&and4, &compiled).expect("and4 must analyze");
        assert!(!report.edges.is_empty());
        for edge in &report.edges {
            let total = edge.total();
            assert!(total.length > 0, "{} -> {}: length must be positive", edge.source, edge.sink);
            assert!(total.repeaters >= 1, "{} -> {}: every socket needs at least one repeater", edge.source, edge.sink);
        }
    }

    #[test]
    fn single_band_ramps_never_place_a_repeater() {
        // A single-band ramp (`RAMP_LENGTH` = 2) is always far too short to
        // ever need a repeater (`MAX_DUST_RUN` is 14). A skip-level shaft
        // *can* need one -- see `climb_levels`'s doc comment -- so this only
        // checks edges with `hops == 1`, which never touch a shaft at all
        // (see `Net::exits`: a shaft only exists between two channels this
        // net has to cross, and a `hops == 1` edge never crosses one).
        let (and4, _) = build_and4_netlist();
        let compiled = compile(&and4).expect("and4 must compile");
        let report = analyze(&and4, &compiled).expect("and4 must analyze");
        for edge in report.edges.iter().filter(|e| e.hops == 1) {
            let ramp = edge.part(RoutePart::Ramp);
            assert_eq!(
                ramp.repeaters, 0,
                "{} -> {}: a single-band ramp must never place a repeater",
                edge.source, edge.sink
            );
        }
    }

    #[test]
    fn a_deep_skip_level_shaft_can_need_a_repeater() {
        // None of the four reference circuits' own skip-level edges climb
        // far enough to need this any more: `move_through_shaft` now always
        // lands the climb on a mandatory-repeater correction first (see
        // `shaft_diagonal_z`'s doc comment), so a climb starts fresh
        // (`MAX_SIGNAL_STRENGTH`) every time, and none of the reference
        // circuits' own skips are deep enough to run that out before
        // reaching the top. A synthetic deep chain still can: six levels of
        // `NOT` between the primary input and a seventh gate that also
        // reads that same primary input directly skips from row 0 to row 6
        // (`BAND_HEIGHT` * 6 = 30 levels, well past `MAX_DUST_RUN`'s 14).
        // This is the one load-bearing check that `scan_climb` actually
        // replays `plan_climb`'s decision instead of assuming a straight
        // run -- without it, `distinct_totals_matches_the_world_for_every_
        // reference_circuit` would still catch a wrong *count* on whatever
        // circuit first needed this, but only this test pins down that the
        // count is allowed to be nonzero for the right, specific reason.
        use crate::compile::Gate;

        let mut gates = Vec::new();
        let mut prev = "a".to_string();
        for i in 1..=6 {
            let out = format!("g{i}");
            gates.push(Gate { name: out.clone(), inputs: vec![prev.clone()], output: out.clone() });
            prev = out;
        }
        gates.push(Gate { name: "g7".to_string(), inputs: vec!["a".to_string(), prev], output: "g7".to_string() });
        let netlist =
            Netlist { inputs: vec!["a".to_string()], outputs: vec!["g7".to_string()], gates };

        let compiled = compile(&netlist).expect("this deep chain must compile");
        let report = analyze(&netlist, &compiled).expect("this deep chain must analyze");
        let skip_edge = report
            .edges
            .iter()
            .find(|e| e.source == "a" && e.sink == "g7.in[0]")
            .expect("a -> g7.in[0] must be a routed edge");
        assert!(skip_edge.hops > 1, "a -> g7 should skip several levels, got hops={}", skip_edge.hops);
        assert!(
            skip_edge.part(RoutePart::Ramp).repeaters > 0,
            "a climb this deep should have needed at least one shaft repeater"
        );
    }
}
