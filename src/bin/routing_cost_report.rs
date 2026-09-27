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
    attribute_transition, critical_trunks, verify_partition, AttributionError, HopKind,
};
use reda::compile::fragment_synth::benchmark::{
    legacy_benchmark_evaluator, MAX_TRANSITION_GAME_TICKS,
};
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
use reda::compile::routing_stats::{
    analyze, distinct_totals_by_part, EdgeRoute, PartTotals, RoutePart, ALL_PARTS,
};
use reda::compile::{compile_legacy, Netlist};
use reda::redstone::simulator::Simulator;
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

/// Measure the production recursive product over the acceptance evaluator's
/// exact transition manifest, then attribute the same worst transition.
fn run_recursive_segment_a() {
    let evaluator = legacy_benchmark_evaluator().expect("acceptance fixtures must load");
    let fixture = evaluator.fixture("segment_a").expect("segment_a fixture");
    let netlist = fixture.lowered_netlist();
    let result = compile_fragment_synth(
        SynthesisInput {
            lowered: netlist,
            source_provenance: None,
            pins: Some(fixture.placements()),
        },
        SynthesisBudget::Evaluations(0),
    )
    .expect("segment_a recursive production compile");
    let compiled = &result.compiled;
    let acceptance = evaluator
        .evaluate_world("segment_a", compiled)
        .expect("acceptance manifest must measure the recursive world");
    let acceptance_ticks = acceptance
        .max_observed_settle_game_ticks_on_manifest
        .expect("acceptance report has a measured settle");

    let manifest = fixture.transition_manifest();
    let mut initial = Simulator::new(compiled.world.clone());
    initial
        .run_until_stable(MAX_TRANSITION_GAME_TICKS)
        .expect("recursive world must initially settle");
    let mut warm = initial.world().clone();
    warm.take_dirty();
    let watched = watch_all_nets(compiled);
    let mut transitions = Vec::with_capacity(manifest.transitions().len());
    for transition in manifest.transitions() {
        let mut simulator = Simulator::new(warm.clone());
        for (signal, bit) in manifest.input_ports().iter().zip(&transition.from) {
            let &(x, y, z) = compiled
                .input_positions
                .get(signal)
                .expect("input position");
            let mut lever = simulator.world().get(x, y, z).clone();
            lever.lit = *bit;
            simulator.world_mut().set(x, y, z, lever);
        }
        simulator
            .run_until_stable(MAX_TRANSITION_GAME_TICKS)
            .expect("manifest from-state must settle");
        simulator.attach_observer(watched.clone());
        simulator.reset_observer();
        let start_tick = simulator.current_tick();
        for (signal, bit) in manifest.input_ports().iter().zip(&transition.to) {
            let &(x, y, z) = compiled
                .input_positions
                .get(signal)
                .expect("input position");
            let mut lever = simulator.world().get(x, y, z).clone();
            lever.lit = *bit;
            simulator.world_mut().set(x, y, z, lever);
        }
        let settle = simulator
            .run_until_stable(MAX_TRANSITION_GAME_TICKS)
            .expect("manifest to-state must settle");
        transitions.push(observations_to_result(
            simulator.observations(),
            start_tick,
            settle,
        ));
    }
    let outputs = netlist.outputs.clone();
    let summary = summarize_worst_case(netlist, compiled, &outputs, &transitions);
    assert_eq!(
        summary.worst_settle_game_ticks, acceptance_ticks,
        "report must reproduce the acceptance manifest metric"
    );
    let worst = transitions
        .iter()
        .find(|transition| transition.settle_game_ticks == summary.worst_settle_game_ticks)
        .expect("worst transition is in the acceptance manifest sweep");
    let attribution = attribute_transition(netlist, &summary.critical_path, worst)
        .expect("critical path must map onto production chunks and observations");
    assert!(attribution.reconciles());
    // The attribution keys on the static production partition; the product
    // says what it actually built. The two must agree, leaf for leaf, or
    // the attribution is about a decomposition that never existed.
    let diagnostics = result
        .recursive_diagnostics
        .as_ref()
        .ok_or(AttributionError::NoDiagnostics)
        .expect("segment_a takes the packed recursive shape, which records its leaves");
    verify_partition(netlist, &diagnostics.leaves)
        .expect("the product's leaves must be the static partition's leaves");
    let tied = critical_trunks(&attribution, &diagnostics.root_trunks)
        .expect("every trunk hop on the critical path must ride a root trunk");

    println!("================ segment_a recursive production ================");
    println!(
        "budget=Evaluations(0), acceptance transitions={}, acceptance/report worst settle={} ticks",
        manifest.transitions().len(),
        summary.worst_settle_game_ticks
    );
    println!("Critical path: {}", summary.critical_path.join(" -> "));
    println!(
        "Measured attribution: child={} ticks; trunk={} ticks; input lead={} ticks; output/settle tail={} ticks; total={} ticks",
        attribution.intra_ticks,
        attribution.trunk_ticks,
        attribution.lead_ticks,
        attribution.tail_ticks,
        attribution.settle_game_ticks,
    );
    for hop in &attribution.hops {
        let kind = match &hop.kind {
            HopKind::Intra(chunk) => format!("child {chunk:?}"),
            HopKind::Trunk { from, to } => format!("trunk {from:?} -> {to:?}"),
            HopKind::FromInput(input) => format!("input {input:?}"),
        };
        println!(
            "  {} -> {}: {kind}, {} measured ticks",
            hop.from, hop.to, hop.ticks
        );
    }
    println!(
        "Actual product leaves: {} (static partition: {} leaves, verified gate for gate)",
        diagnostics.leaves.len(),
        attribution.chunks.len().max(diagnostics.leaves.len())
    );
    for leaf in &diagnostics.leaves {
        println!("  leaf {:?}: {} gates", leaf.chunk, leaf.gates.len());
    }
    println!(
        "Root trunks: {} (signals: {})",
        diagnostics.root_trunks.len(),
        diagnostics
            .root_trunks
            .iter()
            .map(|trunk| trunk.signal.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (hop, trunk) in &tied {
        println!(
            "Critical trunk hop {} -> {} rides root trunk `{}`: {} conductor cells, {} floors, {} repeaters among conductors, branch terminal repeaters {:?}, lane {:?}, {} measured ticks",
            hop.from,
            hop.to,
            trunk.signal,
            trunk.cells,
            trunk.floors,
            trunk.repeaters,
            trunk.branch_terminal_repeaters,
            trunk.lane,
            hop.ticks
        );
    }
    println!(
        "Repeater ownership: root trunk repeaters are read off the shipped route trees above; per-hop repeaters inside a child are not exposed by the product and are not inferred."
    );
}

fn main() {
    if std::env::args().any(|arg| arg == "--recursive-segment-a") {
        run_recursive_segment_a();
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
