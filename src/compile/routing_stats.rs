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
    approach_column, band_ramp_length, band_y, bent_path_cells, build_floorplan, build_nets,
    cell_geometry_by_input_count, effective_band, reserve_columns, resolve_bypass_and_geometry, CompileError,
    CompiledCircuit, Exit, Floorplan, Net, Netlist, Source, BYPASS_QUERY_MAX_DISTANCE, GATE_Y, OUTPUT_DIRECTION,
    RAMP_REST_INTERVAL,
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
    /// A dust-staircase climb or descent between `GATE_Y` and one track
    /// band's own `band_y`: `band_ramp_length(band)` blocks, zero repeaters,
    /// by construction of that function -- not something the router's
    /// placement or track-assignment choices can affect, though which band
    /// (and so how long) does depend on which slot climbed it.
    Ramp,
    /// East-west dust run at one track band's `band_y`: the portion of an
    /// assigned track between a net's entry point and one specific exit. A
    /// track can carry several nets (left-edge assignment), so this is only
    /// the calling net's own share of it, not the whole physical track.
    Track,
    /// The final approach into a consuming gate's input socket: a run north
    /// into the row and the mandatory terminating repeater every socket
    /// needs -- nothing else, now that a gate has no interior. Before cells
    /// dissolved (see `docs/superpowers/specs/2026-08-08-3d-codesign.md`),
    /// this also carried an unconditional corner-plus-repeater jog for every
    /// west/east input, because the column the router landed a signal on
    /// was `SOCKET_REACH` away from centre for *row-spacing* reasons while
    /// the cell's own socket sat only 2 away -- two different numbers that
    /// happened to both be called "the gate's width". `approach_column` is
    /// now defined to equal the gate's actual socket X, so that jog is gone
    /// structurally, not merely rarer -- this category still measures the
    /// same thing (the last-mile approach into a socket), just without the
    /// cost that never had to exist.
    GateEntry,
    /// A whole edge routed directly at `GATE_Y`, with no ramp and no track at
    /// all -- `compute_bypass`'s "close enough" case. Covers everything from
    /// the source pin to the consuming socket: an optional sideways jog onto
    /// the sink's approach column (skipped if the pin is already on it), and
    /// the final approach into the socket. Mutually exclusive with
    /// `Column`/`Ramp`/`Track`/`GateEntry` for the same edge -- a bypassed
    /// edge has none of those parts at all.
    Bypass,
}

pub const ALL_PARTS: [RoutePart; 5] =
    [RoutePart::Column, RoutePart::Ramp, RoutePart::Track, RoutePart::GateEntry, RoutePart::Bypass];

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

/// `(floorplan, nets, row Z, per-channel track Z, per-channel track count,
/// per-net bypass flag)`.
type Geometry = (Floorplan, Vec<Net>, Vec<i32>, Vec<Vec<i32>>, Vec<usize>, Vec<bool>);

/// Recompute the floorplan, nets (with columns and tracks already assigned),
/// Z layout and bypass decisions for `netlist` -- the same stages `compile`
/// runs, called here verbatim so this module's geometry cannot drift from
/// what actually got built. Every stage except the last is pure and
/// world-free; `resolve_bypass_and_geometry` itself builds one throwaway
/// probe `World` internally (see its own doc comment) purely to decide the
/// widened bypass set, and never hands that world back out.
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
    let (bypass, row_z, track_z) = resolve_bypass_and_geometry(
        netlist,
        &plan,
        &mut nets,
        row_count,
        channel_count,
        BYPASS_QUERY_MAX_DISTANCE,
    );
    let track_count: Vec<usize> = track_z.iter().map(Vec::len).collect();

    Ok((plan, nets, row_z, track_z, track_count, bypass))
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

/// Mirrors `lay_bent_path`'s own cell list (`bent_path_cells`) exactly, so
/// this reads back precisely the cells that call wrote -- one classification
/// per cell, with no assumption about which of them (if any) turned out to
/// need a repeater.
fn scan_bent_path(world: &World, start: Position, waypoints: &[Position]) -> PartTotals {
    bent_path_cells(start, waypoints).into_iter().fold(PartTotals::default(), |acc, pos| acc + classify(world, pos))
}

/// Mirrors the last leg of `emit`'s ordinary (non-bypass) Columns pass: from
/// a track/ramp `landing`, an optional sideways bend onto the socket's own X
/// (skipped for a south input, whose socket already sits on `landing`'s own
/// column -- see `approach_column`'s doc comment), then the socket.
fn scan_socket_entry(world: &World, landing: Position, socket: Position, row_z_gate: i32) -> PartTotals {
    let mut waypoints: Vec<Position> = Vec::new();
    if socket.x != landing.x {
        waypoints.push(Position::new(landing.x, landing.y, row_z_gate));
    }
    waypoints.push(socket);
    scan_bent_path(world, landing, &waypoints)
}

/// Mirrors the bypass branch of `emit`'s Columns pass exactly: from `pin`
/// (via `bypass_source_start`'s own one-extra-hop shift, whenever the net's
/// source is a merge that needs to jog off its own column -- see that
/// function's doc comment for why that hop exists and cannot be folded into
/// `waypoints` the way an ordinary bend can), an optional sideways bend onto
/// the sink's approach column (`exit_x`), an optional second bend at the
/// destination row, then the socket.
fn scan_bypass(
    world: &World,
    netlist: &Netlist,
    net: &Net,
    pin: Position,
    exit_x: i32,
    socket: Position,
    row_z_gate: i32,
) -> PartTotals {
    let start = super::bypass_source_start(netlist, net, pin, exit_x);
    let extra_hop = if start != pin { classify(world, start) } else { PartTotals::default() };

    let mut waypoints: Vec<Position> = Vec::new();
    if start.x != exit_x {
        waypoints.push(Position::new(exit_x, start.y, start.z));
    }
    if socket.x != exit_x {
        waypoints.push(Position::new(exit_x, start.y, row_z_gate));
    }
    waypoints.push(socket);
    extra_hop + scan_bent_path(world, start, &waypoints)
}

/// Mirrors `move_between_layers`'s loop exactly: one landing-dust cell per Y
/// level crossed between `entry` and `target_y`, plus one mandatory rest-stop
/// repeater every `RAMP_REST_INTERVAL` levels (see that constant's doc
/// comment -- a climbing step's landing cannot double as its own repeater,
/// so a rest stop is a genuine extra flat cell, counted here just like any
/// other). The riser each climbing step also places is solid stone, not
/// wire, so it is never counted here (`classify` only recognises
/// `Repeater`/`RedstoneWire`).
fn scan_ramp(world: &World, entry: Position, direction: Facing, target_y: i32) -> PartTotals {
    let levels = (target_y - entry.y).abs();
    let climbing = target_y >= entry.y;
    let mut current = entry;
    let mut totals = PartTotals::default();
    for level in 0..levels {
        if level > 0 && level % RAMP_REST_INTERVAL == 0 {
            let rest = current.offset(direction);
            totals += classify(world, rest);
            let rest_output = rest.offset(direction);
            totals += classify(world, rest_output);
            current = rest_output;
        }
        if climbing {
            let riser = current.offset(direction);
            let landing = riser.up();
            totals += classify(world, landing);
            current = landing;
        } else {
            let stepped = current.offset(direction);
            let landing = Position::new(stepped.x, stepped.y - 1, stepped.z);
            totals += classify(world, landing);
            current = landing;
        }
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
    let (plan, nets, row_z, track_z, track_count, bypass) = recompute_geometry(netlist)?;
    let cell_of_count = cell_geometry_by_input_count(netlist);
    let world = &compiled.world;

    let mut edges = Vec::new();
    for (n, net) in nets.iter().enumerate() {
        let source_label = match net.source {
            Source::Lever(i) => netlist.inputs[i].clone(),
            Source::Gate(g) => netlist.gates[g].output.clone(),
        };
        let pin = source_pin(netlist, compiled, net.source);

        if bypass[n] {
            // `compute_bypass` only admits nets with exactly one channel and
            // exactly one sink, so this net has exactly one edge, routed
            // entirely by the bypass branch of `emit`'s Columns pass.
            let (gate, input_index) = net.sinks[0][0];
            let exit_x = approach_column(plan.centre_x[gate], input_index);
            let row_z_gate = row_z[plan.row_of[gate]];
            let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
            let (dx, dy, dz) = cell.input_offsets[input_index];
            let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);

            let mut parts: BTreeMap<RoutePart, PartTotals> = BTreeMap::new();
            parts.insert(RoutePart::Bypass, scan_bypass(world, netlist, net, pin, exit_x, socket, row_z_gate));

            let sink_label = format!("{}.in[{}]", netlist.gates[gate].output, input_index);
            edges.push(EdgeRoute { source: source_label, sink: sink_label, hops: 1, parts });
            continue;
        }

        for (slot, sinks) in net.sinks.iter().enumerate() {
            for &(gate, input_index) in sinks {
                let mut parts: BTreeMap<RoutePart, PartTotals> = BTreeMap::new();

                // Column-in: once per net, always present, regardless of
                // which slot this particular edge ends at.
                let channel0 = net.channels[0];
                let band0 = net.tracks[0];
                let eff_band0 = effective_band(&track_count, channel0, band0);
                let entry0 = Position::new(
                    net.entry_column(0),
                    GATE_Y,
                    track_z[channel0][band0] + band_ramp_length(eff_band0),
                );
                *parts.entry(RoutePart::Column).or_default() +=
                    scan_dust_run(world, pin, Facing::North, entry0.offset(Facing::North));

                for i in 0..=slot {
                    let channel = net.channels[i];
                    let band = net.tracks[i];
                    let eff_band = effective_band(&track_count, channel, band);
                    let ramp_length = band_ramp_length(eff_band);
                    let z = track_z[channel][band];
                    let entry = Position::new(net.entry_column(i), GATE_Y, z + ramp_length);

                    // Ramp up into this slot's track: once per slot, shared
                    // by every exit (sinks and/or a feed-through) in it.
                    *parts.entry(RoutePart::Ramp).or_default() +=
                        scan_ramp(world, entry, Facing::North, band_y(eff_band));

                    if i < slot {
                        // Intermediate hop: this net feeds a gate further
                        // away than the next row, so it feeds through here.
                        let hop_x = net.hops[i];
                        *parts.entry(RoutePart::Track).or_default() +=
                            scan_track_to(world, net.entry_column(i), hop_x, band_y(eff_band), z);

                        let top = Position::new(hop_x, band_y(eff_band), z);
                        *parts.entry(RoutePart::Ramp).or_default() += scan_ramp(world, top, Facing::North, GATE_Y);

                        let landing = Position::new(hop_x, GATE_Y, z - ramp_length);
                        let next_channel = net.channels[i + 1];
                        let next_band = net.tracks[i + 1];
                        let eff_next_band = effective_band(&track_count, next_channel, next_band);
                        let next_z = track_z[next_channel][next_band];
                        let next_entry = Position::new(hop_x, GATE_Y, next_z + band_ramp_length(eff_next_band));
                        *parts.entry(RoutePart::Column).or_default() +=
                            scan_dust_run(world, landing, Facing::North, next_entry.offset(Facing::North));
                    } else {
                        // Final slot: the real socket this edge ends at.
                        let exit_x = approach_column(plan.centre_x[gate], input_index);
                        *parts.entry(RoutePart::Track).or_default() +=
                            scan_track_to(world, net.entry_column(i), exit_x, band_y(eff_band), z);

                        let top = Position::new(exit_x, band_y(eff_band), z);
                        *parts.entry(RoutePart::Ramp).or_default() += scan_ramp(world, top, Facing::North, GATE_Y);

                        let landing = Position::new(exit_x, GATE_Y, z - ramp_length);
                        let row_z_gate = row_z[plan.row_of[gate]];
                        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
                        let (dx, dy, dz) = cell.input_offsets[input_index];
                        let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);
                        *parts.entry(RoutePart::GateEntry).or_default() +=
                            scan_socket_entry(world, landing, socket, row_z_gate);
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
    let (plan, nets, row_z, track_z, track_count, bypass) = recompute_geometry(netlist)?;
    let cell_of_count = cell_geometry_by_input_count(netlist);
    let world = &compiled.world;

    let mut total: BTreeMap<RoutePart, PartTotals> = BTreeMap::new();
    for (n, net) in nets.iter().enumerate() {
        let pin = source_pin(netlist, compiled, net.source);

        if bypass[n] {
            let (gate, input_index) = net.sinks[0][0];
            let exit_x = approach_column(plan.centre_x[gate], input_index);
            let row_z_gate = row_z[plan.row_of[gate]];
            let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
            let (dx, dy, dz) = cell.input_offsets[input_index];
            let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);
            *total.entry(RoutePart::Bypass).or_default() += scan_bypass(world, netlist, net, pin, exit_x, socket, row_z_gate);
            continue;
        }

        for (slot, &channel) in net.channels.iter().enumerate() {
            let band = net.tracks[slot];
            let eff_band = effective_band(&track_count, channel, band);
            let ramp_length = band_ramp_length(eff_band);
            let z = track_z[channel][band];
            let entry = Position::new(net.entry_column(slot), GATE_Y, z + ramp_length);

            if slot == 0 {
                *total.entry(RoutePart::Column).or_default() +=
                    scan_dust_run(world, pin, Facing::North, entry.offset(Facing::North));
            }
            *total.entry(RoutePart::Ramp).or_default() += scan_ramp(world, entry, Facing::North, band_y(eff_band));

            let (lo, hi) = net.span(slot, &plan.centre_x);
            *total.entry(RoutePart::Track).or_default() +=
                scan_full_track(world, net.entry_column(slot), lo, hi, band_y(eff_band), z);

            for exit in net.exits(slot, &plan.centre_x) {
                let top = Position::new(exit.x(), band_y(eff_band), z);
                *total.entry(RoutePart::Ramp).or_default() += scan_ramp(world, top, Facing::North, GATE_Y);
                let landing = Position::new(exit.x(), GATE_Y, z - ramp_length);

                match exit {
                    Exit::Socket { gate, input_index, .. } => {
                        let row_z_gate = row_z[plan.row_of[gate]];
                        let cell = &cell_of_count[&netlist.gates[gate].inputs.len()];
                        let (dx, dy, dz) = cell.input_offsets[input_index];
                        let socket = Position::new(plan.centre_x[gate] + dx, GATE_Y + dy, row_z_gate + dz);
                        *total.entry(RoutePart::GateEntry).or_default() +=
                            scan_socket_entry(world, landing, socket, row_z_gate);
                    }
                    Exit::Feedthrough { x, next_slot } => {
                        let next_channel = net.channels[next_slot];
                        let next_band = net.tracks[next_slot];
                        let eff_next_band = effective_band(&track_count, next_channel, next_band);
                        let next_z = track_z[next_channel][next_band];
                        let next_entry = Position::new(x, GATE_Y, next_z + band_ramp_length(eff_next_band));
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
    fn every_edge_has_a_positive_total_length_even_when_dust_terminates_it() {
        // A legal straight dust endpoint can now weakly power a gate's
        // support directly, so a short routed edge can contain zero
        // repeaters. Length remains the topology-independent sanity check:
        // every declared edge must still occupy a real route.
        let (and4, _) = build_and4_netlist();
        let compiled = compile(&and4).expect("and4 must compile");
        let report = analyze(&and4, &compiled).expect("and4 must analyze");
        assert!(!report.edges.is_empty());
        for edge in &report.edges {
            let total = edge.total();
            assert!(total.length > 0, "{} -> {}: length must be positive", edge.source, edge.sink);
        }
    }

    /// Every non-bypassed edge crosses at least one channel, so it has at
    /// least one ramp up and one ramp down per slot -- but, now that a
    /// channel can spread its tracks across several Y bands
    /// (`docs/superpowers/specs/2026-08-08-3d-codesign.md`, "Layers, not two
    /// planes"), each slot's ramp instances are `band_ramp_length` of
    /// *that slot's own band* long, not a single flat constant shared by
    /// every slot of every edge. This test recomputes each net's actual
    /// per-slot band assignment (the same `recompute_geometry` `analyze`
    /// itself calls) and checks the reported ramp length against exactly the
    /// bands the edge's own slots used -- zero repeaters throughout, since a
    /// dust staircase, unlike the old repeater ramp, never places one.
    #[test]
    fn ramp_length_matches_the_bands_each_edge_actually_climbed() {
        let (and4, _) = build_and4_netlist();
        let compiled = compile(&and4).expect("and4 must compile");
        let report = analyze(&and4, &compiled).expect("and4 must analyze");
        let (_plan, nets, _row_z, _track_z, track_count, bypass) =
            recompute_geometry(&and4).expect("and4 must recompute geometry");

        // Effective, not raw, bands -- `and4` never gets dense enough to
        // revert any channel (`BAND_CAP`'s doc comment), but resolving
        // through `effective_band` here anyway is what keeps this
        // assertion honest about what `analyze` itself actually predicts.
        let mut bands_by_source: HashMap<String, Vec<usize>> = HashMap::new();
        for (n, net) in nets.iter().enumerate() {
            if bypass[n] {
                continue;
            }
            let source = match net.source {
                Source::Lever(i) => and4.inputs[i].clone(),
                Source::Gate(g) => and4.gates[g].output.clone(),
            };
            let eff_bands: Vec<usize> = net
                .channels
                .iter()
                .zip(net.tracks.iter())
                .map(|(&channel, &band)| effective_band(&track_count, channel, band))
                .collect();
            bands_by_source.insert(source, eff_bands);
        }

        for edge in &report.edges {
            if edge.part(RoutePart::Bypass).length > 0 {
                // A bypassed edge never ramps at all -- it connects directly
                // at `GATE_Y` (see `compute_bypass`) -- so it is exempt from
                // the "every edge climbs" assumption this test otherwise
                // checks.
                assert_eq!(edge.part(RoutePart::Ramp), PartTotals::default());
                continue;
            }
            let ramp = edge.part(RoutePart::Ramp);
            let tracks = bands_by_source
                .get(&edge.source)
                .unwrap_or_else(|| panic!("{}: net must have been recorded by recompute_geometry", edge.source));
            let expected_length: i64 =
                tracks[..edge.hops].iter().map(|&band| 2 * band_ramp_length(band) as i64).sum();
            assert_eq!(
                ramp.length, expected_length,
                "{} -> {}: ramp length must match the bands it actually climbed", edge.source, edge.sink
            );
            assert_eq!(ramp.repeaters, 0, "a dust staircase ramp must never place a repeater");
        }
    }

    /// The one behavioural check this whole feature exists for: on `and4`,
    /// at least one edge actually takes the direct `GATE_Y` bypass instead of
    /// climbing to a track and back down. If this ever goes back to zero, the
    /// bypass has silently stopped firing on the reference circuit the
    /// routing-cost spec measured it against.
    #[test]
    fn and4_actually_uses_the_bypass_on_at_least_one_edge() {
        let (and4, _) = build_and4_netlist();
        let compiled = compile(&and4).expect("and4 must compile");
        let report = analyze(&and4, &compiled).expect("and4 must analyze");
        let bypassed = report.edges.iter().filter(|e| e.part(RoutePart::Bypass).length > 0).count();
        assert!(bypassed > 0, "expected at least one bypassed edge on and4, got 0 of {}", report.edges.len());
    }
}
