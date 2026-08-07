//! Routing-cost breakdown: decomposes every routed netlist edge's physical
//! length into structural parts -- column, ramp, track, gate entry -- and
//! counts how many repeaters each part forces, since repeaters are the only
//! source of delay a route can add (redstone dust conducts within the same
//! game tick; only torches, repeaters and lamps are ever scheduled -- see
//! `redstone::simulator::schedule` and `component::repeater_delay_game_ticks`).
//!
//! This module never writes to a `World`. It recomputes the exact geometry
//! `compile` computes -- by calling `build_floorplan`, `build_nets`,
//! `reserve_columns`, `assign_tracks` and `layout_z` verbatim, the same
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
    approach_column, assign_tracks, build_floorplan, build_nets, direction_from, layout_z, place_nor_gate,
    reserve_columns, CompileError, CompiledCircuit, Exit, Floorplan, Net, Netlist, NorCell, Source, GATE_Y,
    OUTPUT_DIRECTION, RAMP_LENGTH, TRACK_Y,
};
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

// ---------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------

/// One structural part of a routed edge's physical path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RoutePart {
    /// North-south dust run at `GATE_Y`: a driver's pin to its first ramp,
    /// or (for a feed-through hop) one ramp's landing straight on to the
    /// next ramp's entry -- passing underneath any number of tracks it does
    /// not stop at.
    Column,
    /// A fixed four-block climb or descent between `GATE_Y` and `TRACK_Y`.
    /// Always exactly two repeaters and four blocks by construction of those
    /// two constants, regardless of net, slot or circuit -- not something
    /// the router's placement or track-assignment choices can affect.
    Ramp,
    /// East-west dust run at `TRACK_Y`: the portion of an assigned track
    /// between a net's entry point and one specific exit. A track can carry
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
    /// How many channels (routing rows) this edge's signal had to cross --
    /// 1 for a next-row consumer, more if a feed-through was needed to skip
    /// over intervening rows entirely.
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

/// `(floorplan, nets, row Z, per-channel track Z, per-channel track count)`.
type Geometry = (Floorplan, Vec<Net>, Vec<i32>, Vec<Vec<i32>>, Vec<usize>);

/// Recompute the floorplan, nets (with columns and tracks already assigned)
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

    reserve_columns(&plan, &mut nets, row_count, channel_count);
    let track_count = assign_tracks(&plan, &mut nets, channel_count);
    let (row_z, track_z) = layout_z(row_count, channel_count, &track_count);

    Ok((plan, nets, row_z, track_z, track_count))
}

/// One gate cell's socket geometry per distinct input count (1..=3) --
/// `NorCell`'s offsets are relative, so they do not depend on where a given
/// gate actually sits, only on how many inputs it has.
fn cell_geometry_by_input_count(netlist: &Netlist) -> HashMap<usize, NorCell> {
    let mut cells = HashMap::new();
    let mut scratch = World::new(20, 5, 20);
    for gate in &netlist.gates {
        cells
            .entry(gate.inputs.len())
            .or_insert_with(|| place_nor_gate(&mut scratch, (8, GATE_Y, 8), gate.inputs.len()));
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

/// Mirrors `move_between_layers`'s loop exactly: one repeater and one
/// landing-dust cell per Y level crossed between `entry` and `target_y`.
/// Support blocks are stone, not wire, so they are not counted here.
fn scan_ramp(world: &World, entry: Position, direction: Facing, target_y: i32) -> PartTotals {
    let y_step = (target_y - entry.y).signum();
    let mut current = entry;
    let mut totals = PartTotals::default();
    while current.y != target_y {
        let repeater_pos = current.offset(direction);
        totals += classify(world, repeater_pos);
        let support_pos = repeater_pos.offset(direction);
        let landing = Position::new(support_pos.x, support_pos.y + y_step, support_pos.z);
        totals += classify(world, landing);
        current = landing;
    }
    totals
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
            Position::new(x, y, z).offset(OUTPUT_DIRECTION)
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

                // Column-in: once per net, always present, regardless of
                // which slot this particular edge ends at.
                let channel0 = net.channels[0];
                let entry0 = Position::new(
                    net.entry_column(0),
                    GATE_Y,
                    track_z[channel0][net.tracks[0]] + RAMP_LENGTH,
                );
                *parts.entry(RoutePart::Column).or_default() +=
                    scan_dust_run(world, pin, Facing::North, entry0.offset(Facing::North));

                for i in 0..=slot {
                    let channel = net.channels[i];
                    let z = track_z[channel][net.tracks[i]];
                    let entry = Position::new(net.entry_column(i), GATE_Y, z + RAMP_LENGTH);

                    // Ramp up into this slot's track: once per slot, shared
                    // by every exit (sinks and/or a feed-through) in it.
                    *parts.entry(RoutePart::Ramp).or_default() += scan_ramp(world, entry, Facing::North, TRACK_Y);

                    if i < slot {
                        // Intermediate hop: this net feeds a gate further
                        // away than the next row, so it feeds through here.
                        let hop_x = net.hops[i];
                        *parts.entry(RoutePart::Track).or_default() +=
                            scan_track_to(world, net.entry_column(i), hop_x, TRACK_Y, z);

                        let top = Position::new(hop_x, TRACK_Y, z);
                        *parts.entry(RoutePart::Ramp).or_default() += scan_ramp(world, top, Facing::North, GATE_Y);

                        let landing = Position::new(hop_x, GATE_Y, z - RAMP_LENGTH);
                        let next_channel = net.channels[i + 1];
                        let next_z = track_z[next_channel][net.tracks[i + 1]];
                        let next_entry = Position::new(hop_x, GATE_Y, next_z + RAMP_LENGTH);
                        *parts.entry(RoutePart::Column).or_default() +=
                            scan_dust_run(world, landing, Facing::North, next_entry.offset(Facing::North));
                    } else {
                        // Final slot: the real socket this edge ends at.
                        let exit_x = approach_column(plan.centre_x[gate], input_index);
                        *parts.entry(RoutePart::Track).or_default() +=
                            scan_track_to(world, net.entry_column(i), exit_x, TRACK_Y, z);

                        let top = Position::new(exit_x, TRACK_Y, z);
                        *parts.entry(RoutePart::Ramp).or_default() += scan_ramp(world, top, Facing::North, GATE_Y);

                        let landing = Position::new(exit_x, GATE_Y, z - RAMP_LENGTH);
                        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
                        let (dx, dy, dz) = cell.input_offsets[input_index];
                        let socket = Position::new(
                            plan.centre_x[gate] + dx,
                            GATE_Y + dy,
                            row_z[plan.row_of[gate]] + dz,
                        );
                        let mut entry_total = PartTotals::default();
                        if socket.x == landing.x {
                            entry_total += scan_to_socket(world, landing, socket);
                        } else {
                            let corner = Position::new(landing.x, GATE_Y, row_z[plan.row_of[gate]]);
                            entry_total += scan_to_corner(world, landing, corner);
                            entry_total += scan_to_socket(world, corner, socket);
                        }
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
    let cell_of_count = cell_geometry_by_input_count(netlist);
    let world = &compiled.world;

    let mut total: BTreeMap<RoutePart, PartTotals> = BTreeMap::new();
    for net in &nets {
        let pin = source_pin(netlist, compiled, net.source);

        for (slot, &channel) in net.channels.iter().enumerate() {
            let z = track_z[channel][net.tracks[slot]];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + RAMP_LENGTH);

            if slot == 0 {
                *total.entry(RoutePart::Column).or_default() +=
                    scan_dust_run(world, pin, Facing::North, entry.offset(Facing::North));
            }
            *total.entry(RoutePart::Ramp).or_default() += scan_ramp(world, entry, Facing::North, TRACK_Y);

            let (lo, hi) = net.span(slot, &plan.centre_x);
            *total.entry(RoutePart::Track).or_default() +=
                scan_full_track(world, net.entry_column(slot), lo, hi, TRACK_Y, z);

            for exit in net.exits(slot, &plan.centre_x) {
                let top = Position::new(exit.x(), TRACK_Y, z);
                *total.entry(RoutePart::Ramp).or_default() += scan_ramp(world, top, Facing::North, GATE_Y);
                let landing = Position::new(exit.x(), GATE_Y, z - RAMP_LENGTH);

                match exit {
                    Exit::Socket { gate, input_index, .. } => {
                        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
                        let (dx, dy, dz) = cell.input_offsets[input_index];
                        let socket = Position::new(
                            plan.centre_x[gate] + dx,
                            GATE_Y + dy,
                            row_z[plan.row_of[gate]] + dz,
                        );
                        let entry_total = if socket.x == landing.x {
                            scan_to_socket(world, landing, socket)
                        } else {
                            let corner = Position::new(landing.x, GATE_Y, row_z[plan.row_of[gate]]);
                            scan_to_corner(world, landing, corner) + scan_to_socket(world, corner, socket)
                        };
                        *total.entry(RoutePart::GateEntry).or_default() += entry_total;
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let next_channel = net.channels[next_slot];
                        let next_z = track_z[next_channel][net.tracks[next_slot]];
                        let next_entry = Position::new(x, GATE_Y, next_z + RAMP_LENGTH);
                        *total.entry(RoutePart::Column).or_default() +=
                            scan_dust_run(world, landing, Facing::North, next_entry.offset(Facing::North));
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
    fn ramp_length_and_repeater_count_are_fixed_by_gate_y_and_track_y() {
        let (and4, _) = build_and4_netlist();
        let compiled = compile(&and4).expect("and4 must compile");
        let report = analyze(&and4, &compiled).expect("and4 must analyze");
        for edge in &report.edges {
            let ramp = edge.part(RoutePart::Ramp);
            // Every edge crosses at least one channel, so it has at least
            // one ramp up and one ramp down: 2 * hops ramp instances, each
            // exactly `RAMP_LENGTH` long with `RAMP_LENGTH / 2` repeaters.
            let expected_instances = 2 * edge.hops as i64;
            assert_eq!(ramp.length, expected_instances * RAMP_LENGTH as i64);
            assert_eq!(ramp.repeaters, expected_instances as usize * (RAMP_LENGTH as usize / 2));
        }
    }
}
