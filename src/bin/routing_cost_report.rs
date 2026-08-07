//! Routing-cost breakdown report: for each reference circuit, decomposes
//! every routed netlist edge into its structural parts (column, ramp, track,
//! gate entry) and prints length/repeater distributions -- for all edges,
//! and separately for the critical path found by `reda::timing`.
//!
//! See `docs/superpowers/specs/2026-08-07-routing-cost-breakdown.md`, which
//! this binary's output feeds directly into. This is a measurement tool, not
//! part of the library's public surface -- it is fine for it to panic on
//! anything unexpected in a reference circuit.

use std::collections::BTreeMap;

use reda::circuits::and4::{build_and4_netlist, INPUT_NAMES as AND4_INPUTS};
use reda::circuits::full_adder::{build_full_adder_netlist, INPUT_NAMES as ADDER_INPUTS};
use reda::circuits::seven_segment::{
    build_seven_segment_netlist, build_single_segment_netlist, INPUT_NAMES as DECODER_INPUTS,
};
use reda::compile::routing_stats::{analyze, distinct_totals_by_part, EdgeRoute, PartTotals, RoutePart, ALL_PARTS};
use reda::compile::{compile, Netlist};
use reda::redstone::simulator::Simulator;
use reda::timing::{observations_to_result, summarize_worst_case, watch_all_nets, TransitionResult};

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
            let settle = simulator.run_until_stable(MAX_TICKS).expect("reference circuits must settle");
            transitions.push(observations_to_result(simulator.observations(), start_tick, settle));
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
    let median = if n % 2 == 1 { v[n / 2] as f64 } else { (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0 };
    Stats { min: v[0], median, mean, max: v[n - 1] }
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
    }
}

fn print_distribution(title: &str, edges: &[&EdgeRoute]) {
    println!("{title} (n={}):", edges.len());
    for &part in &ALL_PARTS {
        let lengths: Vec<i64> = edges.iter().map(|e| e.part(part).length).collect();
        let repeaters: Vec<i64> = edges.iter().map(|e| e.part(part).repeaters as i64).collect();
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
fn critical_edges<'a>(netlist: &Netlist, edges: &'a [EdgeRoute], critical_path: &[String]) -> Vec<&'a EdgeRoute> {
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

    let compiled = compile(netlist).expect("reference circuits must compile");
    let report = analyze(netlist, &compiled).expect("reference circuits must analyze");
    let by_part = distinct_totals_by_part(netlist, &compiled).expect("reference circuits must analyze");

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
        println!("  {:<12} length={:<6} repeaters={}", part_name(part), t.length, t.repeaters);
    }
    println!("  {:<12} length={:<6} repeaters={}", "TOTAL", grand.length, grand.repeaters);

    let all_edges: Vec<&EdgeRoute> = report.edges.iter().collect();
    println!();
    print_distribution("All edges", &all_edges);

    let mut hop_histogram: BTreeMap<usize, usize> = BTreeMap::new();
    for edge in &all_edges {
        *hop_histogram.entry(edge.hops).or_default() += 1;
    }
    println!("Hop-count histogram (edges by channels crossed): {hop_histogram:?}");

    let lever_positions: Vec<(i32, i32, i32)> =
        input_names.iter().map(|&n| *compiled.input_positions.get(n).unwrap()).collect();
    let watched = watch_all_nets(&compiled);
    // Simulate on a clone of the world -- `compiled` is kept intact (its
    // block kinds/positions never change during simulation) so it can still
    // be handed to `summarize_worst_case` below, which needs it to count the
    // actual measured critical path's repeaters.
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(MAX_TICKS).expect("must settle before the first reading");
    simulator.attach_observer(watched);
    let transitions = sweep(&mut simulator, &lever_positions);
    let summary = summarize_worst_case(netlist, &compiled, outputs, &transitions);

    println!(
        "\nWorst-case settle: {} game ticks; logic-depth bound: {} game ticks; ratio: {:.2}x",
        summary.worst_settle_game_ticks, summary.logic_depth_bound_game_ticks, summary.ratio
    );
    println!(
        "Critical-path settle model: {} gates + {} repeaters -> {} game ticks predicted ({} measured)",
        summary.critical_path_gate_count,
        summary.critical_path_repeater_count,
        summary.critical_path_model_game_ticks,
        summary.worst_settle_game_ticks,
    );
    println!("Critical path: {}", summary.critical_path.join(" -> "));

    let crit_edges = critical_edges(netlist, &report.edges, &summary.critical_path);
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
                println!("      {:<12} length={:<6} repeaters={}", part_name(part), p.length, p.repeaters);
            }
        }
    }

    let crit_total_repeaters: usize = crit_edges.iter().map(|e| e.total().repeaters).sum();
    println!(
        "\nSum of critical-path edges' own repeater counts (each edge's full path, trunk included where shared): {crit_total_repeaters}"
    );
}

fn main() {
    let (and4, and4_output) = build_and4_netlist();
    run_and_report("and4", &and4, &AND4_INPUTS, &[and4_output]);

    let (full_adder, full_adder_outputs) = build_full_adder_netlist();
    run_and_report(
        "full_adder",
        &full_adder,
        &ADDER_INPUTS,
        &[full_adder_outputs["sum"].clone(), full_adder_outputs["cout"].clone()],
    );

    let (segment_a, segment_a_output) = build_single_segment_netlist(0);
    run_and_report("segment_a", &segment_a, &DECODER_INPUTS, &[segment_a_output]);

    let (seven_segment, seven_segment_outputs) = build_seven_segment_netlist();
    let mut outputs: Vec<String> = seven_segment_outputs.values().cloned().collect();
    outputs.sort();
    run_and_report("seven_segment", &seven_segment, &DECODER_INPUTS, &outputs);
}
