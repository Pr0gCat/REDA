//! Acceptance tests for the Yosys-backed Verilog frontend
//! (`reda::frontend::synthesize_verilog`).
//!
//! Same proof standard as the hand-written reference circuits: compile,
//! simulate every input combination through the real redstone simulator, and
//! check the result against a truth table written independently of the
//! netlist (`reda::circuits::seven_segment::TRUTH_TABLE`, and a plain AND
//! for `and4`). The Verilog sources under `tests/fixtures/` describe the
//! *same* two circuits `reda::circuits::and4` and
//! `reda::circuits::seven_segment` build by hand, so each test here also
//! builds the hand-written netlist alongside the Yosys-derived one and
//! prints both side by side -- gate count, block count, and settle time --
//! which is the only real answer to "does a real synthesiser beat the
//! hand-written netlist" for this hardware.
//!
//! # Why this needs Python + `yowasp-yosys`
//!
//! `synthesize_verilog` shells out to Yosys via the `yowasp-yosys` Python
//! package (see `src/frontend/mod.rs`'s module doc comment for why: this
//! project provides the cost model, not a Verilog parser). No other test in
//! this crate depends on that -- these are the only ones that do, and if
//! `python` or `yowasp-yosys` is missing, they fail with a specific error
//! from `FrontendError`'s `Display` impl (see the last test below), not a
//! panic or a stack trace.

use std::collections::{BTreeMap, HashMap};

use reda::circuits::and4::build_and4_netlist;
use reda::circuits::seven_segment::{build_seven_segment_netlist, TRUTH_TABLE};
use reda::circuits::verilog;
use reda::compile::lowering::{format_histogram, lower, lower_optimised, LowerError};
use reda::compile::topology::GateKind;
use reda::compile::{compile, CompiledCircuit, Netlist};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;
use reda::timing::{
    game_ticks_to_redstone_ticks, game_ticks_to_seconds, observations_to_result,
    summarize_worst_case, watch_all_nets, TransitionResult,
};

const MAX_TICKS: u64 = 2000;

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) {
    let mut state = simulator
        .world()
        .get(position.0, position.1, position.2)
        .clone();
    state.lit = on;
    simulator
        .world_mut()
        .set(position.0, position.1, position.2, state);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle after changing an input");
}

/// Same as `set_lever`, but also records the transition's timing -- the
/// simulator must already have an observer attached (see `watch_all_nets`).
fn set_lever_and_record(
    simulator: &mut Simulator,
    position: (i32, i32, i32),
    on: bool,
    transitions: &mut Vec<TransitionResult>,
) {
    simulator.reset_observer();
    let start_tick = simulator.current_tick();
    set_lever(simulator, position, on);
    let settle_game_ticks = simulator.current_tick() - start_tick;
    transitions.push(observations_to_result(
        simulator.observations(),
        start_tick,
        settle_game_ticks,
    ));
}

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator
        .world()
        .get(position.0, position.1, position.2)
        .lit
}

fn non_air_blocks(compiled: &CompiledCircuit) -> usize {
    let (size_x, size_y, size_z) = compiled.world.size();
    let mut count = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if compiled.world.get(x, y, z).kind != BlockKind::Air {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Print the worst case across an instrumented sweep: settle time, the
/// logic-depth lower bound and the ratio between them, and the critical path.
fn report_timing(
    label: &str,
    netlist: &Netlist,
    outputs: &[String],
    transitions: &[TransitionResult],
) -> u64 {
    let summary = summarize_worst_case(netlist, outputs, transitions);
    eprintln!(
        "{label} timing: worst-case settle = {} game ticks ({:.1} redstone ticks, {:.3}s)",
        summary.worst_settle_game_ticks,
        game_ticks_to_redstone_ticks(summary.worst_settle_game_ticks),
        game_ticks_to_seconds(summary.worst_settle_game_ticks),
    );
    eprintln!(
        "{label} timing: logic-depth bound (netlist, layout-independent) = {} gates -> {} game ticks; \
         ratio (measured/bound) = {:.2}x; critical path {} gates: {:?}",
        summary.logic_depth,
        summary.logic_depth_bound_game_ticks,
        summary.ratio,
        summary.critical_path_gate_count,
        summary.critical_path,
    );
    summary.worst_settle_game_ticks
}

/// Lower `netlist`, compile it, simulate every input combination, and assert
/// the output at `output_of(value)` matches `expected(value)`. Returns
/// `(gate_count, block_count, settle_game_ticks)` for the comparison table,
/// where `gate_count` is the **lowered** netlist's -- the gates that really
/// get placed.
///
/// The lowering happens here, once, and everything downstream uses its
/// result: `compile` requires it (see that function's own doc comment for
/// why it will not do it for you), and `summarize_worst_case` has to be
/// handed the same netlist the circuit was compiled from or its backward
/// walk is correlating two different graphs.
#[allow(clippy::too_many_arguments)]
fn compile_simulate_and_check(
    label: &str,
    source_netlist: &Netlist,
    lower_netlist: fn(&Netlist) -> Result<Netlist, LowerError>,
    input_names: &[&str],
    input_count: u32,
    output_positions_of: impl Fn(&CompiledCircuit) -> Vec<(i32, i32, i32)>,
    expected: impl Fn(u8) -> Vec<bool>,
) -> (usize, usize, u64) {
    let netlist =
        &lower_netlist(source_netlist).expect("netlist must lower into torches and merges");
    if netlist.gates.len() != source_netlist.gates.len() {
        eprintln!(
            "{label} cells: {} ({} gates) -> lowered {} ({} gates)",
            format_histogram(source_netlist),
            source_netlist.gates.len(),
            format_histogram(netlist),
            netlist.gates.len(),
        );
    }
    let gate_count = netlist.gates.len();
    let compiled = compile(netlist).expect("netlist must be acyclic and fully driven");
    let block_count = non_air_blocks(&compiled);

    let lever_positions: HashMap<&str, (i32, i32, i32)> = input_names
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let output_positions = output_positions_of(&compiled);

    let watched = watch_all_nets(&compiled);
    let mut simulator = Simulator::new(compiled.bench.clone());
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle before the first reading");
    simulator.attach_observer(watched);

    let mut mismatches = Vec::new();
    let mut transitions: Vec<TransitionResult> = Vec::new();
    let combinations = 1u32 << input_count;
    for combination in 0..combinations {
        let bits: Vec<u8> = (0..input_count)
            .rev()
            .map(|i| ((combination >> i) & 1) as u8)
            .collect();
        for (&name, &bit) in input_names.iter().zip(bits.iter()) {
            set_lever_and_record(
                &mut simulator,
                lever_positions[name],
                bit == 1,
                &mut transitions,
            );
        }

        let expected_values = expected(combination as u8);
        for (i, &position) in output_positions.iter().enumerate() {
            let actual = read_output(&simulator, position);
            if actual != expected_values[i] {
                mismatches.push(format!(
                    "inputs={bits:?} output[{i}]: expected {}, got {actual}",
                    expected_values[i]
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "{label} does not match its truth table ({}/{combinations} wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    // The occupied extent, which is what README's table reports -- not the
    // world's allocated size, which is larger and arbitrary.
    let (size_x, size_y, size_z) = compiled.world.size();
    let (mut min, mut max) = ((i32::MAX, i32::MAX, i32::MAX), (0, 0, 0));
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if compiled.world.get(x, y, z).kind != reda::redstone::world::block::BlockKind::Air
                {
                    min = (min.0.min(x), min.1.min(y), min.2.min(z));
                    max = (max.0.max(x), max.1.max(y), max.2.max(z));
                }
            }
        }
    }
    eprintln!(
        "{label} bounding box: {}x{}x{}",
        max.0 - min.0 + 1,
        max.1 - min.1 + 1,
        max.2 - min.2 + 1
    );

    let outputs: Vec<String> = netlist.outputs.clone();
    let settle = report_timing(label, netlist,
        &outputs,
        &transitions,
    );

    eprintln!(
        "{label}: {gate_count} gates, {block_count} blocks, {settle} game ticks settle, \
         {:.1} blocks/gate",
        block_count as f64 / gate_count as f64
    );

    (gate_count, block_count, settle)
}

#[test]
fn the_verilog_and4_matches_its_truth_table() {
    let source = std::fs::read_to_string("tests/fixtures/and4.v").expect("fixture must exist");
    let (netlist, port_map) = synthesize_verilog(&source, "and4").expect("and4.v must synthesize");
    let output_signal = port_map["y"].clone();

    let verilog_stats = compile_simulate_and_check(
        "verilog and4",
        &netlist,
        lower,
        &["a", "b", "c", "d"],
        4,
        |compiled| vec![*compiled.output_positions.get(&output_signal).unwrap()],
        |bits| vec![(0..4).all(|i| (bits >> (3 - i)) & 1 == 1)],
    );

    let (hand_netlist, hand_output) = build_and4_netlist();
    let hand_stats = compile_simulate_and_check(
        "hand-written and4",
        &hand_netlist,
        lower,
        &["a", "b", "c", "d"],
        4,
        |compiled| vec![*compiled.output_positions.get(&hand_output).unwrap()],
        |bits| vec![(0..4).all(|i| (bits >> (3 - i)) & 1 == 1)],
    );

    eprintln!(
        "and4 comparison: verilog {}g/{}b/{}t vs hand-written {}g/{}b/{}t",
        verilog_stats.0, verilog_stats.1, verilog_stats.2, hand_stats.0, hand_stats.1, hand_stats.2
    );
}

#[test]
fn verilog_dff_reaches_the_physical_simulator() {
    let source = std::fs::read_to_string("tests/fixtures/dff.v").expect("fixture must exist");
    let (netlist, port_map) =
        synthesize_verilog(&source, "dff_example").expect("dff.v must synthesize");
    assert!(netlist
        .gates
        .iter()
        .any(|gate| gate.kind == GateKind::DffPosedge));

    let lowered = lower(&netlist).expect("DFF netlist must lower");
    let compiled = compile(&lowered).expect("DFF netlist must compile");

    let d = compiled.input_positions["d"];
    let clk = compiled.input_positions["clk"];
    let q = compiled.output_positions[&port_map["q"]];
    let mut simulator = Simulator::new(compiled.bench);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("DFF must settle");

    let mut apply = |position, on| {
        set_lever(&mut simulator, position, on);
        read_output(&simulator, q)
    };
    assert!(!apply(clk, true));
    assert!(!apply(clk, false));
    assert!(!apply(d, true));
    assert!(apply(clk, true));
    assert!(apply(d, false));
    assert!(apply(clk, false));
    assert!(!apply(clk, true));
}

#[test]
/// Release measurement: 47 lowered gates, 10,088 blocks, and 86 game ticks.
/// This is Pareto-better than the all-positive 56 / 12,348 / 88 baseline.
fn optimised_lowering_preserves_every_verilog_decoder_vector() {
    let source =
        std::fs::read_to_string("tests/fixtures/seven_segment.v").expect("fixture must exist");
    let (netlist, port_map) =
        synthesize_verilog(&source, "bcd_seven_segment").expect("seven_segment.v must synthesize");

    let segment_names = ["a", "b", "c", "d", "e", "f", "g"];
    let output_signals: Vec<String> = segment_names.iter().map(|&s| port_map[s].clone()).collect();
    let output_signals_for_closure = output_signals.clone();

    let expected_segments = |value: u8| -> Vec<bool> {
        if (value as usize) < TRUTH_TABLE.len() {
            TRUTH_TABLE[value as usize]
                .iter()
                .map(|&bit| bit == 1)
                .collect()
        } else {
            vec![false; 7]
        }
    };

    let verilog_stats = compile_simulate_and_check(
        "verilog seven_segment",
        &netlist,
        lower_optimised,
        &["d3", "d2", "d1", "d0"],
        4,
        move |compiled| {
            output_signals_for_closure
                .iter()
                .map(|s| *compiled.output_positions.get(s).unwrap())
                .collect()
        },
        expected_segments,
    );

    // Gates are what lowering decides; blocks and ticks belong to the
    // generator, which does not optimise them.
    assert!(
        verilog_stats.0 < 56,
        "optimised lowering must beat the 56-gate baseline: got {} gates",
        verilog_stats.0
    );

    let (hand_netlist, hand_segment_signal) = build_seven_segment_netlist();
    let hand_signals: Vec<String> = segment_names
        .iter()
        .map(|&s| hand_segment_signal[s].clone())
        .collect();
    let hand_stats = compile_simulate_and_check(
        "hand-written seven_segment",
        &hand_netlist,
        lower,
        &["d3", "d2", "d1", "d0"],
        4,
        move |compiled| {
            hand_signals
                .iter()
                .map(|s| *compiled.output_positions.get(s).unwrap())
                .collect()
        },
        expected_segments,
    );

    eprintln!(
        "seven_segment comparison: verilog {}g/{}b/{}t vs hand-written {}g/{}b/{}t",
        verilog_stats.0, verilog_stats.1, verilog_stats.2, hand_stats.0, hand_stats.1, hand_stats.2
    );
}

/// The checked-in baked netlists (`src/circuits/baked/*.netlist`) still say
/// what Yosys actually produces.
///
/// A baked netlist exists because the browser viewer's `wasm32` build cannot
/// run Yosys and yet needs a synthesised circuit -- see
/// `VerilogCircuit::baked_netlist` for the whole argument. That makes each
/// one a standing claim about this project's own compiler, published in a
/// view whose entire purpose is showing what a circuit really is. A stale
/// one does not go quietly out of date; it misrepresents the compiler to
/// the one audience looking specifically for the truth about it.
///
/// So this re-runs synthesis on the very same embedded source and requires
/// the result to render **byte-for-byte** identically to the checked-in
/// file. Comparing rendered text rather than `Netlist` values is not a
/// weaker check: `render_and_parse_round_trip_every_catalog_entry` (in
/// `reda::circuits::verilog`) pins the two together, and comparing text is
/// what makes the failure message a diff a person can act on. The fix is
/// always the same, and the assertion says so: run `bake_verilog` and commit
/// the result.
///
/// This lives here, alongside the other tests that need Python and
/// `yowasp-yosys`, because it needs them for exactly the same reason.
#[test]
fn the_baked_netlists_match_fresh_synthesis() {
    for circuit in verilog::CIRCUITS {
        let (netlist, output_labels) = circuit
            .synthesize()
            .unwrap_or_else(|error| panic!("{} must synthesize: {error}", circuit.name));
        let fresh = verilog::baked::render(circuit, &netlist, &output_labels)
            .unwrap_or_else(|error| panic!("{}'s fresh netlist {error}", circuit.name));

        if fresh != circuit.baked {
            let first_difference = fresh
                .lines()
                .zip(circuit.baked.lines())
                .position(|(a, b)| a != b)
                .map(|index| {
                    format!(
                        "first differing line is {}:\n  fresh: {}\n  baked: {}",
                        index + 1,
                        fresh.lines().nth(index).unwrap_or(""),
                        circuit.baked.lines().nth(index).unwrap_or("")
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "one is a prefix of the other: fresh has {} lines, baked has {}",
                        fresh.lines().count(),
                        circuit.baked.lines().count()
                    )
                });
            panic!(
                "{} is stale: {} no longer matches a fresh synthesis of {}.\n{first_difference}\n\
                 Re-run `cargo run --release --bin bake_verilog` and commit the result.",
                circuit.name, circuit.baked_path, circuit.source_path
            );
        }

        // ...and the netlist a caller with no Yosys gets back really is the
        // one Yosys just produced, not merely a file that happens to render
        // the same way.
        let (baked_netlist, baked_labels) = circuit.baked_netlist();
        assert_eq!(
            baked_netlist, netlist,
            "{}: baked netlist differs from the fresh one",
            circuit.name
        );
        assert_eq!(
            baked_labels, output_labels,
            "{}: baked output labels differ",
            circuit.name
        );
    }
}

/// The frontend's whole point is to fail loudly, not silently or with a
/// panic, when something goes wrong -- this checks the most common failure a
/// user will actually hit (a typo'd or missing top module name) produces a
/// specific, readable message rather than a crash.
#[test]
fn synthesizing_an_unknown_top_module_fails_with_a_clear_error() {
    let source = std::fs::read_to_string("tests/fixtures/and4.v").expect("fixture must exist");
    let message = match synthesize_verilog(&source, "this_module_does_not_exist") {
        Ok(_) => panic!("an unknown top module must not synthesize"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("this_module_does_not_exist") || message.contains("not found"),
        "expected the error to name the missing module, got: {message}"
    );
}

// ---------------------------------------------------------------------
// Native-frontend variants
// ---------------------------------------------------------------------
//
// The five tests above all go through `synthesize_verilog`, which shells out
// to Python + `yowasp-yosys`. On a machine with no toolchain they fail with
// `FrontendError::PythonNotFound` -- an environment limit, not a product
// fault. The native frontend (`compile_systemverilog`) lowers the very same
// `.v` fixtures with no Python, no subprocess, and no filesystem, and stops
// at exactly the same gate-level `Netlist` boundary. These variants run the
// identical proof standard -- compile, simulate every input combination
// through the real redstone simulator, check against the same independently
// written truth table -- through that native entry point, so each feature
// here is verified natively. They do not replace the Yosys tests: the fresh
// synthesis comparison is a separate, optional independent check.

/// Lower `path`'s Verilog source with the native frontend, returning the
/// gate-level netlist plus its `port name -> internal signal` map in the
/// same shape `synthesize_verilog` returns.
fn native_netlist(path: &str, top: &str) -> (Netlist, HashMap<String, String>) {
    let source = std::fs::read_to_string(path).expect("fixture must exist");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: path,
            text: &source,
        }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|diags| panic!("{path} must compile natively as {top}: {diags:?}"));
    (artifact.netlist.clone(), artifact.output_map())
}

#[test]
fn native_verilog_and4_matches_its_truth_table() {
    let (netlist, port_map) = native_netlist("tests/fixtures/and4.v", "and4");
    let output_signal = port_map["y"].clone();

    let native_stats = compile_simulate_and_check(
        "native and4",
        &netlist,
        lower,
        &["a", "b", "c", "d"],
        4,
        |compiled| vec![*compiled.output_positions.get(&output_signal).unwrap()],
        |bits| vec![(0..4).all(|i| (bits >> (3 - i)) & 1 == 1)],
    );

    let (hand_netlist, hand_output) = build_and4_netlist();
    let hand_stats = compile_simulate_and_check(
        "hand-written and4",
        &hand_netlist,
        lower,
        &["a", "b", "c", "d"],
        4,
        |compiled| vec![*compiled.output_positions.get(&hand_output).unwrap()],
        |bits| vec![(0..4).all(|i| (bits >> (3 - i)) & 1 == 1)],
    );

    eprintln!(
        "and4 native comparison: native {}g/{}b/{}t vs hand-written {}g/{}b/{}t",
        native_stats.0, native_stats.1, native_stats.2, hand_stats.0, hand_stats.1, hand_stats.2
    );
}

#[test]
fn native_verilog_dff_reaches_the_physical_simulator() {
    let (netlist, port_map) = native_netlist("tests/fixtures/dff.v", "dff_example");
    assert!(netlist
        .gates
        .iter()
        .any(|gate| gate.kind == GateKind::DffPosedge));

    let lowered = lower(&netlist).expect("native DFF netlist must lower");
    let compiled = compile(&lowered).expect("native DFF netlist must compile");

    let d = compiled.input_positions["d"];
    let clk = compiled.input_positions["clk"];
    let q = compiled.output_positions[&port_map["q"]];
    let mut simulator = Simulator::new(compiled.bench);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("native DFF must settle");

    let mut apply = |position, on| {
        set_lever(&mut simulator, position, on);
        read_output(&simulator, q)
    };
    assert!(!apply(clk, true));
    assert!(!apply(clk, false));
    assert!(!apply(d, true));
    assert!(apply(clk, true));
    assert!(apply(d, false));
    assert!(apply(clk, false));
    assert!(!apply(clk, true));
}

#[test]
fn native_optimised_lowering_preserves_every_verilog_decoder_vector() {
    let (netlist, port_map) = native_netlist("tests/fixtures/seven_segment.v", "bcd_seven_segment");

    let segment_names = ["a", "b", "c", "d", "e", "f", "g"];
    let output_signals: Vec<String> = segment_names.iter().map(|&s| port_map[s].clone()).collect();
    let output_signals_for_closure = output_signals.clone();

    let expected_segments = |value: u8| -> Vec<bool> {
        if (value as usize) < TRUTH_TABLE.len() {
            TRUTH_TABLE[value as usize]
                .iter()
                .map(|&bit| bit == 1)
                .collect()
        } else {
            vec![false; 7]
        }
    };

    let native_stats = compile_simulate_and_check(
        "native seven_segment",
        &netlist,
        lower_optimised,
        &["d3", "d2", "d1", "d0"],
        4,
        move |compiled| {
            output_signals_for_closure
                .iter()
                .map(|s| *compiled.output_positions.get(s).unwrap())
                .collect()
        },
        expected_segments,
    );

    // Gates are what lowering decides; blocks and ticks belong to the
    // generator, which does not optimise them.
    assert!(
        native_stats.0 < 56,
        "native optimised lowering must beat the 56-gate baseline: got {} gates",
        native_stats.0
    );

    let (hand_netlist, hand_segment_signal) = build_seven_segment_netlist();
    let hand_signals: Vec<String> = segment_names
        .iter()
        .map(|&s| hand_segment_signal[s].clone())
        .collect();
    let hand_stats = compile_simulate_and_check(
        "hand-written seven_segment",
        &hand_netlist,
        lower,
        &["d3", "d2", "d1", "d0"],
        4,
        move |compiled| {
            hand_signals
                .iter()
                .map(|s| *compiled.output_positions.get(s).unwrap())
                .collect()
        },
        expected_segments,
    );

    eprintln!(
        "seven_segment native comparison: native {}g/{}b/{}t vs hand-written {}g/{}b/{}t",
        native_stats.0, native_stats.1, native_stats.2, hand_stats.0, hand_stats.1, hand_stats.2
    );
}

/// The native frontend's netlist for each shipped Verilog circuit computes
/// the same function as the baked Yosys netlist, evaluated through the
/// structural `Evaluator` over every input combination. Native lowering
/// invents its own gate names (`g0`, `g1`, ...), so byte-for-byte equality
/// with the baked file is impossible; this is the semantic equivalence that
/// matters -- same port, same inputs, same output value.
#[test]
fn native_baked_netlists_match_semantic_equivalence() {
    for circuit in verilog::CIRCUITS {
        let source = std::fs::read_to_string(circuit.source_path).expect("fixture must exist");
        let native = compile_systemverilog(
            &[SourceInput {
                name: circuit.source_path,
                text: &source,
            }],
            &CompileOptions::new(circuit.top_module),
        )
        .unwrap_or_else(|diags| panic!("{} must compile natively: {diags:?}", circuit.name));
        let (baked_netlist, baked_labels) = circuit.baked_netlist();

        let native_map = native.output_map();
        let baked_map: HashMap<String, String> = baked_labels.into_iter().collect();

        let native_eval = Evaluator::new(&native.netlist).expect("native netlist must evaluate");
        let baked_eval = Evaluator::new(&baked_netlist).expect("baked netlist must evaluate");

        // Native lowers inputs in declaration order (`d3`..`d0`), baked in
        // sorted order (`d0`..`d3`); the truth-table evaluation maps by name,
        // so the set of input names is what must match, not their order.
        let inputs = native.netlist.inputs.clone();
        let mut baked_inputs = baked_netlist.inputs.clone();
        baked_inputs.sort();
        let mut native_inputs = inputs.clone();
        native_inputs.sort();
        assert_eq!(
            native_inputs, baked_inputs,
            "{}: native and baked input name sets differ",
            circuit.name
        );
        let n = inputs.len();
        for combo in 0..(1u32 << n) {
            let mut map = BTreeMap::new();
            for (i, name) in inputs.iter().enumerate() {
                map.insert(name.clone(), (combo >> (n - 1 - i)) & 1 == 1);
            }
            let native_out = native_eval.evaluate(&map).expect("native must evaluate");
            let baked_out = baked_eval.evaluate(&map).expect("baked must evaluate");
            for port in native_map.keys() {
                assert_eq!(
                    native_out[&native_map[port]], baked_out[&baked_map[port]],
                    "{} combo {combo} port {port}: native and baked disagree",
                    circuit.name
                );
            }
        }
    }
}

#[test]
fn native_synthesizing_an_unknown_top_module_fails_with_a_clear_error() {
    let source = std::fs::read_to_string("tests/fixtures/and4.v").expect("fixture must exist");
    let diags = compile_systemverilog(
        &[SourceInput {
            name: "and4.v",
            text: &source,
        }],
        &CompileOptions::new("this_module_does_not_exist"),
    )
    .expect_err("an unknown top module must not compile natively");
    let message = diags
        .iter()
        .map(|d| d.message.clone())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        message.contains("this_module_does_not_exist"),
        "expected the native error to name the missing module, got: {message}"
    );
}
