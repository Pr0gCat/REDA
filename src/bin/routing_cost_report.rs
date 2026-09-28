//! Routing-cost breakdown report: for each reference circuit, decomposes
//! every routed netlist edge into its structural parts (column, ramp, track,
//! gate entry) and prints length/repeater distributions -- for all edges,
//! and separately for the critical path found by `reda::timing`.
//!
//! See `docs/superpowers/specs/2026-08-07-routing-cost-breakdown.md`, which
//! this binary's output feeds directly into. This is a measurement tool, not
//! part of the library's public surface -- it is fine for it to panic on
//! anything unexpected in a reference circuit.
//!
//! **Reports the emitter's layout, and says so by calling `compile_legacy`.**
//! Column, ramp, track and gate entry are the row/channel/track emitter's own
//! structural parts; a relaxation-placed circuit has none of them, and
//! `routing_stats` refuses it rather than inventing them. Since the hybrid
//! `compile` landed, `compile` is no longer guaranteed to return a layout this
//! report can read, so naming the path is what keeps the report meaning what
//! it has always meant.

use std::collections::BTreeMap;

use reda::circuits::and4::{build_and4_netlist, INPUT_NAMES as AND4_INPUTS};
use reda::circuits::full_adder::{build_full_adder_netlist, INPUT_NAMES as ADDER_INPUTS};
use reda::circuits::seven_segment::{
    build_seven_segment_netlist, build_single_segment_netlist, INPUT_NAMES as DECODER_INPUTS,
};
use reda::compile::fragment_synth::attribution::{
    arrivals_of, attribute_path_with, attribute_transition, last_change_path, leaf_owner,
    HopKind,
};
use reda::compile::fragment_synth::benchmark::legacy_benchmark_evaluator;
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
use reda::compile::routing_stats::{
    analyze, distinct_totals_by_part, EdgeRoute, PartTotals, RoutePart, ALL_PARTS,
};
use reda::compile::{compile_legacy, Netlist};
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;
use reda::redstone::world::storage::World;
use reda::timing::{
    observations_to_result, summarize_worst_case, watch_all_nets, TransitionResult,
};

const MAX_TICKS: u64 = 4000;

// ---------------------------------------------------------------------
// Sweep: flip every input through every combination, recording timing.
// ---------------------------------------------------------------------

fn sweep(simulator: &mut Simulator, lever_positions: &[(i32, i32, i32)]) -> Vec<TransitionResult> {
    let bit_count = lever_positions.len();
    let mut transitions = Vec::new();
    for combination in 0u32..(1 << bit_count) {
        for (i, &(x, y, z)) in lever_positions.iter().enumerate() {
            let bit = (combination >> (bit_count - 1 - i)) & 1 == 1;
            simulator.reset_observer();
            let start_tick = simulator.current_tick();
            let mut state = simulator.world().get(x, y, z).clone();
            state.lit = bit;
            simulator.world_mut().set(x, y, z, state);
            let settle = simulator
                .run_until_stable(MAX_TICKS)
                .expect("reference circuits must settle");
            transitions.push(observations_to_result(
                simulator.observations(),
                start_tick,
                settle,
            ));
        }
    }
    transitions
}

// ---------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct Stats {
    min: i64,
    median: f64,
    mean: f64,
    max: i64,
}

fn stats(values: &[i64]) -> Stats {
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    let mean = v.iter().sum::<i64>() as f64 / n as f64;
    let median = if n % 2 == 1 {
        v[n / 2] as f64
    } else {
        (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0
    };
    Stats {
        min: v[0],
        median,
        mean,
        max: v[n - 1],
    }
}

fn print_stats_row(label: &str, length_values: &[i64], repeater_values: &[i64]) {
    let l = stats(length_values);
    let r = stats(repeater_values);
    println!(
        "  {label:<12} length: min={:>4} median={:>7.1} mean={:>7.2} max={:>4}  |  repeaters: min={:>3} median={:>5.1} mean={:>6.2} max={:>3}",
        l.min, l.median, l.mean, l.max, r.min, r.median, r.mean, r.max
    );
}

fn part_name(part: RoutePart) -> &'static str {
    match part {
        RoutePart::Column => "column",
        RoutePart::Ramp => "ramp",
        RoutePart::Track => "track",
        RoutePart::GateEntry => "gate-entry",
        RoutePart::Bypass => "bypass",
    }
}

fn print_distribution(title: &str, edges: &[&EdgeRoute]) {
    println!("{title} (n={}):", edges.len());
    for &part in &ALL_PARTS {
        let lengths: Vec<i64> = edges.iter().map(|e| e.part(part).length).collect();
        let repeaters: Vec<i64> = edges
            .iter()
            .map(|e| e.part(part).repeaters as i64)
            .collect();
        print_stats_row(part_name(part), &lengths, &repeaters);
    }
    let total_lengths: Vec<i64> = edges.iter().map(|e| e.total().length).collect();
    let total_repeaters: Vec<i64> = edges.iter().map(|e| e.total().repeaters as i64).collect();
    print_stats_row("TOTAL", &total_lengths, &total_repeaters);
}

// ---------------------------------------------------------------------
// Critical path -> edges
// ---------------------------------------------------------------------

/// Map a critical path (a chain of signal names, source to output) onto the
/// specific `EdgeRoute`s that carry each consecutive hop.
fn critical_edges<'a>(
    netlist: &Netlist,
    edges: &'a [EdgeRoute],
    critical_path: &[String],
) -> Vec<&'a EdgeRoute> {
    let mut result = Vec::new();
    for window in critical_path.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        let gate = netlist
            .gates
            .iter()
            .find(|g| &g.output == b)
            .unwrap_or_else(|| panic!("critical path signal `{b}` must be some gate's output"));
        let input_index = gate
            .inputs
            .iter()
            .position(|input| input == a)
            .unwrap_or_else(|| panic!("critical path signal `{a}` must feed `{b}`"));
        let sink_label = format!("{b}.in[{input_index}]");
        let edge = edges
            .iter()
            .find(|e| e.source == *a && e.sink == sink_label)
            .unwrap_or_else(|| panic!("no routed edge found for {a} -> {sink_label}"));
        result.push(edge);
    }
    result
}

// ---------------------------------------------------------------------
// Per-circuit report
// ---------------------------------------------------------------------

fn run_and_report(label: &str, netlist: &Netlist, input_names: &[&str], outputs: &[String]) {
    println!("\n================ {label} ================");

    let compiled = compile_legacy(netlist).expect("reference circuits must compile");
    let report = analyze(netlist, &compiled).expect("reference circuits must analyze");
    let by_part =
        distinct_totals_by_part(netlist, &compiled).expect("reference circuits must analyze");

    println!(
        "channels={}, tracks-per-channel={:?}, total tracks={}",
        report.channel_count,
        report.track_count,
        report.track_count.iter().sum::<usize>()
    );

    println!("\nWhole-circuit repeater count by part (each physical segment counted once):");
    let mut grand = PartTotals::default();
    for &part in &ALL_PARTS {
        let t = by_part.get(&part).copied().unwrap_or_default();
        grand += t;
        println!(
            "  {:<12} length={:<6} repeaters={}",
            part_name(part),
            t.length,
            t.repeaters
        );
    }
    println!(
        "  {:<12} length={:<6} repeaters={}",
        "TOTAL", grand.length, grand.repeaters
    );

    let all_edges: Vec<&EdgeRoute> = report.edges.iter().collect();
    println!();
    print_distribution("All edges", &all_edges);

    let mut hop_histogram: BTreeMap<usize, usize> = BTreeMap::new();
    for edge in &all_edges {
        *hop_histogram.entry(edge.hops).or_default() += 1;
    }
    println!("Hop-count histogram (edges by channels crossed): {hop_histogram:?}");

    let bypassed = all_edges
        .iter()
        .filter(|e| e.part(RoutePart::Bypass).length > 0)
        .count();
    println!(
        "Bypass vs track: {bypassed}/{} edges routed directly at GATE_Y (no ramp, no track)",
        all_edges.len()
    );

    let lever_positions: Vec<(i32, i32, i32)> = input_names
        .iter()
        .map(|&n| *compiled.input_positions.get(n).unwrap())
        .collect();
    let watched = watch_all_nets(&compiled);
    // Simulate on a clone of the world -- `compiled` is kept intact (its
    // block kinds/positions never change during simulation) so it can still
    // be handed to `summarize_worst_case` below, which needs it to count the
    // actual measured critical path's repeaters.
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("must settle before the first reading");
    simulator.attach_observer(watched);
    let transitions = sweep(&mut simulator, &lever_positions);
    let summary = summarize_worst_case(netlist, &compiled, outputs, &transitions);

    println!(
        "\nWorst-case settle: {} game ticks; logic-depth bound: {} game ticks; ratio: {:.2}x",
        summary.worst_settle_game_ticks, summary.logic_depth_bound_game_ticks, summary.ratio
    );
    // Both terms are `Some` here by construction -- this compiles through the
    // emitter, which is the layout `routing_stats` can read -- but they are
    // printed through the `Option` rather than unwrapped, so that pointing
    // this binary at another path degrades to a printed "unavailable" instead
    // of a panic in a measurement tool.
    match (summary.critical_path_repeater_count, summary.critical_path_model_game_ticks) {
        (Some(repeaters), Some(model)) => println!(
            "Critical-path settle model: {} gates + {repeaters} repeaters -> {model} game ticks predicted ({} measured)",
            summary.critical_path_gate_count, summary.worst_settle_game_ticks,
        ),
        _ => println!(
            "Critical-path settle model: {} gates, repeaters unavailable (this layout has no              row/channel/track geometry to read) -- {} game ticks measured",
            summary.critical_path_gate_count, summary.worst_settle_game_ticks,
        ),
    }
    println!("Critical path: {}", summary.critical_path.join(" -> "));

    let crit_edges = critical_edges(netlist, &report.edges, &summary.critical_path);
    assert_eq!(crit_edges.len(), summary.critical_path.len() - 1);
    let worst_transition = transitions
        .iter()
        .find(|transition| transition.settle_game_ticks == summary.worst_settle_game_ticks)
        .expect("the timing summary's worst transition must be in the sweep");
    let attribution = attribute_transition(netlist, &summary.critical_path, worst_transition)
        .expect("the critical path must map onto the production partition and observations");
    assert!(attribution.reconciles());
    let mut child_repeaters = 0usize;
    let mut trunk_repeaters = 0usize;
    for (hop, edge) in attribution.hops.iter().zip(&crit_edges) {
        let repeaters = edge.total().repeaters;
        match &hop.kind {
            HopKind::Trunk { .. } => trunk_repeaters += repeaters,
            HopKind::Intra(_) | HopKind::FromInput(_) => child_repeaters += repeaters,
        }
    }
    assert_eq!(
        child_repeaters + trunk_repeaters,
        crit_edges
            .iter()
            .map(|edge| edge.total().repeaters)
            .sum::<usize>()
    );
    println!(
        "\nMeasured critical-path attribution (production leaf partition): child={} ticks/{} repeaters; trunk={} ticks/{} repeaters; input lead={} ticks; output/settle tail={} ticks; total={} ticks",
        attribution.intra_ticks,
        child_repeaters,
        attribution.trunk_ticks,
        trunk_repeaters,
        attribution.lead_ticks,
        attribution.tail_ticks,
        attribution.settle_game_ticks,
    );
    println!("Critical-path hops by production chunk:");
    for (hop, edge) in attribution.hops.iter().zip(&crit_edges) {
        let kind = match &hop.kind {
            HopKind::Intra(chunk) => format!("child {chunk:?}"),
            HopKind::Trunk { from, to } => format!("trunk {from:?} -> {to:?}"),
            HopKind::FromInput(chunk) => format!("input boundary -> child {chunk:?}"),
        };
        println!(
            "  {} -> {}  {kind}: measured_delta={} ticks, route_repeaters={}",
            hop.from,
            hop.to,
            hop.ticks,
            edge.total().repeaters,
        );
    }
    println!();
    print_distribution("Critical-path edges", &crit_edges);

    println!("\nCritical-path edges, individually:");
    for edge in &crit_edges {
        let t = edge.total();
        println!(
            "  {} -> {}  (hops={})  total: length={} repeaters={}",
            edge.source, edge.sink, edge.hops, t.length, t.repeaters
        );
        for &part in &ALL_PARTS {
            let p = edge.part(part);
            if p.length > 0 {
                println!(
                    "      {:<12} length={:<6} repeaters={}",
                    part_name(part),
                    p.length,
                    p.repeaters
                );
            }
        }
    }

    let crit_total_repeaters: usize = crit_edges.iter().map(|e| e.total().repeaters).sum();
    println!(
        "\nSum of critical-path edges' own repeater counts (each edge's full path, trunk included where shared): {crit_total_repeaters}"
    );
}

/// Measure the shipped fragment_synth product for one acceptance fixture over
/// the acceptance evaluator's exact drivers and manifest, then attribute its
/// worst transition hop by hop against the leaves the product actually built.
///
/// The critical path is read backwards off the measured arrivals
/// ([`last_change_path`]), and the attribution must reconcile: lead + inside
/// leaves + across leaves + tail equals the acceptance ticks, or this panics.
fn run_recursive(name: &str) {
    let evaluator = legacy_benchmark_evaluator().expect("acceptance fixtures must load");
    let fixture = evaluator
        .fixture(name)
        .unwrap_or_else(|| panic!("unknown acceptance fixture `{name}`"));
    let netlist = fixture.lowered_netlist();
    let result = compile_fragment_synth(
        SynthesisInput {
            lowered: netlist,
            source_provenance: None,
            pins: Some(fixture.placements()),
        },
        SynthesisBudget::Evaluations(0),
    )
    .unwrap_or_else(|error| panic!("{name}: production compile failed: {error}"));
    let compiled = &result.compiled;
    let acceptance_ticks = evaluator
        .evaluate_world(name, compiled)
        .expect("acceptance manifest must measure the shipped world")
        .max_observed_settle_game_ticks_on_manifest
        .expect("acceptance report has a measured settle");
    let conductors = conductor_watch(&compiled.world);
    let worst = evaluator
        .worst_transition_timing(name, compiled, &conductors)
        .expect("the worst transition must measure");
    let cell_arrivals = conductors
        .iter()
        .filter_map(|(at, label)| {
            let tick = worst.nets.get(label)?.arrival_tick()?;
            Some(((at.x, at.y, at.z), tick))
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        worst.settle_game_ticks, acceptance_ticks,
        "the report must reproduce the acceptance ticks"
    );
    let mut arrivals = arrivals_of(&worst);
    arrivals.retain(|label, _| !label.starts_with('@'));
    let path = last_change_path(netlist, &arrivals).expect("a critical path must be readable");
    let diagnostics = result.recursive_diagnostics.as_ref();
    let owner = leaf_owner(netlist, diagnostics).expect("every gate must belong to one leaf");
    let attribution =
        attribute_path_with(netlist, &owner, &path, &arrivals, worst.settle_game_ticks)
            .expect("the critical path must attribute");
    assert!(attribution.reconciles(), "lead + intra + trunk + tail must equal the settle");
    let crossings = attribution
        .hops
        .iter()
        .filter(|hop| matches!(hop.kind, HopKind::Trunk { .. }))
        .count();

    println!("================ {name}: fragment_synth, budget zero ================");
    println!(
        "acceptance ticks={acceptance_ticks}; blocks={}; leaves={}; path gates={}; crossings={crossings}",
        reda::compile::metrics::physical_metrics(&compiled.world, netlist.gates.len() as u64)
            .non_air_blocks,
        diagnostics.map_or(1, |diagnostics| diagnostics.leaves.len().max(1)),
        path.len().saturating_sub(1),
    );
    println!(
        "lead={} intra={} trunk={} tail={} (sum {})",
        attribution.lead_ticks,
        attribution.intra_ticks,
        attribution.trunk_ticks,
        attribution.tail_ticks,
        attribution.settle_game_ticks,
    );
    if let Some(diagnostics) = diagnostics {
        if let Some(chosen) = diagnostics.chosen {
            println!("shipped candidate: {}", diagnostics.candidates[chosen].label);
        }
        for candidate in &diagnostics.candidates {
            match &candidate.quality {
                Ok(key) => println!(
                    "  candidate {}: {} ticks / {} blocks",
                    candidate.label, key.observed_settle, key.non_air_blocks
                ),
                Err(_) => println!("  candidate {}: refused", candidate.label),
            }
        }
        for leaf in &diagnostics.leaves {
            println!("  leaf {:?}: {} gates", leaf.chunk, leaf.gates.len());
        }
    }
    println!("critical path: {}", path.join(" -> "));
    for hop in &attribution.hops {
        let kind = match &hop.kind {
            HopKind::Intra(_) => "inside a leaf".to_owned(),
            HopKind::Trunk { .. } => {
                let trunk = diagnostics.and_then(|diagnostics| {
                    diagnostics.root_trunks.iter().find(|trunk| trunk.signal == hop.from)
                });
                match trunk {
                    Some(trunk) => format!(
                        "across leaves on trunk `{}`: {} cells, {} repeaters, terminal repeaters {:?} (x2 = {} ticks), lane {:?}",
                        trunk.signal,
                        trunk.cells,
                        trunk.repeaters,
                        trunk.branch_terminal_repeaters,
                        2 * trunk.branch_terminal_repeaters.iter().max().copied().unwrap_or(0),
                        trunk.lane,
                    ),
                    None => "across leaves (no root trunk carries it)".to_owned(),
                }
            }
            HopKind::FromInput(_) => "from a primary input".to_owned(),
        };
        let at = |signal: &str| {
            compiled
                .gate_output_positions
                .get(signal)
                .or_else(|| compiled.input_positions.get(signal))
                .copied()
        };
        let span = match (at(&hop.from), at(&hop.to)) {
            (Some(from), Some(to)) => format!(
                " [{from:?} -> {to:?}, manhattan {}]",
                (from.0 - to.0).abs() + (from.1 - to.1).abs() + (from.2 - to.2).abs()
            ),
            _ => String::new(),
        };
        println!("  {} -> {}: {} ticks, {kind}{span}", hop.from, hop.to, hop.ticks);
        if let (Some(from), Some(to)) = (at(&hop.from), at(&hop.to)) {
            match trail(&compiled.world, &cell_arrivals, from, to) {
                Some(cells) => print_trail(&compiled.world, &cells),
                None => println!("    (no monotone trail through watched conductors)"),
            }
        }
    }
}

type Cell = (i32, i32, i32);

/// Every conductor and source cell of `world`, labelled by its coordinate,
/// for a transition that should be walked cell by cell.
fn conductor_watch(world: &World) -> Vec<(Position, String)> {
    [
        BlockKind::RedstoneWire,
        BlockKind::Repeater,
        BlockKind::Comparator,
        BlockKind::Torch,
        BlockKind::WallTorch,
        BlockKind::Lever,
    ]
    .into_iter()
    .flat_map(|kind| world.positions_of(kind).collect::<Vec<_>>())
    .map(|flat| {
        let (x, y, z) = world.decode(flat);
        (Position::new(x, y, z), format!("@{x},{y},{z}"))
    })
    .collect()
}

/// One measured hop, walked cell by cell: the shortest chain of watched
/// cells from `from` to `to` whose measured arrivals lie between theirs and
/// never go back in time. A step spans at most two cells, so it may pass
/// through the one unwatched block a torch, repeater or dust powers; and
/// time may only step up onto a cell that delays (a torch, repeater or
/// comparator), since dust carries a change within the tick.
fn trail(
    world: &World,
    arrivals: &BTreeMap<Cell, u64>,
    from: Cell,
    to: Cell,
) -> Option<Vec<(Cell, u64)>> {
    let &start = arrivals.get(&from)?;
    let &end = arrivals.get(&to)?;
    let mut previous = BTreeMap::from([(from, from)]);
    let mut queue = std::collections::VecDeque::from([from]);
    while let Some(cell) = queue.pop_front() {
        if cell == to {
            let mut path = vec![(to, end)];
            let mut at = to;
            while at != from {
                at = previous[&at];
                path.push((at, arrivals[&at]));
            }
            path.reverse();
            return Some(path);
        }
        let now = arrivals[&cell];
        for dx in -2i32..=2 {
            for dy in -2i32..=2 {
                for dz in -2i32..=2 {
                    if dx.abs() + dy.abs() + dz.abs() == 0 || dx.abs() + dy.abs() + dz.abs() > 2 {
                        continue;
                    }
                    let next = (cell.0 + dx, cell.1 + dy, cell.2 + dz);
                    let Some(&tick) = arrivals.get(&next) else {
                        continue;
                    };
                    if tick < now.max(start) || tick > end || previous.contains_key(&next) {
                        continue;
                    }
                    let delays = matches!(
                        world.get(next.0, next.1, next.2).kind,
                        BlockKind::Repeater
                            | BlockKind::Comparator
                            | BlockKind::Torch
                            | BlockKind::WallTorch
                    );
                    if tick > now && !delays {
                        continue;
                    }
                    previous.insert(next, cell);
                    queue.push_back(next);
                }
            }
        }
    }
    None
}

/// Where a trail spent its time: every cell at which the arrival steps up,
/// with what stands there and how far the trail had come since the last
/// step.
fn print_trail(world: &World, cells: &[(Cell, u64)]) {
    let mut last = cells[0];
    let mut since = 0usize;
    let mut steps = Vec::new();
    for &(at, tick) in &cells[1..] {
        since += 1;
        if tick > last.1 {
            let kind = world.get(at.0, at.1, at.2).kind;
            steps.push(format!("+{} {kind:?}@{at:?} after {since}", tick - last.1));
            since = 0;
        }
        last = (at, tick);
    }
    println!("    trail {} cells: {}", cells.len() - 1, steps.join(", "));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(index) = args.iter().position(|arg| arg == "--recursive") {
        let name = args
            .get(index + 1)
            .expect("--recursive takes an acceptance fixture name, e.g. segment_a");
        run_recursive(name);
        return;
    }
    let (and4, and4_output) = build_and4_netlist();
    run_and_report("and4", &and4, &AND4_INPUTS, &[and4_output]);

    let (full_adder, full_adder_outputs) = build_full_adder_netlist();
    run_and_report(
        "full_adder",
        &full_adder,
        &ADDER_INPUTS,
        &[
            full_adder_outputs["sum"].clone(),
            full_adder_outputs["cout"].clone(),
        ],
    );

    let (segment_a, segment_a_output) = build_single_segment_netlist(0);
    run_and_report(
        "segment_a",
        &segment_a,
        &DECODER_INPUTS,
        &[segment_a_output],
    );

    let (seven_segment, seven_segment_outputs) = build_seven_segment_netlist();
    let mut outputs: Vec<String> = seven_segment_outputs.values().cloned().collect();
    outputs.sort();
    run_and_report("seven_segment", &seven_segment, &DECODER_INPUTS, &outputs);
}
