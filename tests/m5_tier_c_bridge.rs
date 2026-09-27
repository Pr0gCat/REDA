//! Milestone 5 readiness, Tier C (`docs/native-wasm-verilog-compiler-plan.md`,
//! "Verification" table: "existing physical simulator tests").
//!
//! Every other REDA-frontend test (`systemverilog_compiler.rs`,
//! `systemverilog_seven_segment.rs`, `systemverilog_dff.rs`, `m4_shadow_validation.rs`)
//! checks `compile_systemverilog`'s output purely at the logical `Netlist`
//! level, through `frontend::evaluate::Evaluator`. None of them ever call
//! `compile::lowering::lower`, `compile::compile`, or the redstone
//! `Simulator` -- so no test proves the REDA frontend's `Netlist` actually
//! survives lowering, placement, routing, and simulation the way the
//! Yosys-backed `synthesize_verilog` path already does in
//! `tests/verilog_frontend.rs`.
//!
//! This file closes that gap. It is deliberately the smallest possible
//! bridge: reuse the existing lowering/compile/simulate pipeline exactly as
//! `verilog_frontend.rs` does, but feed it `compile_systemverilog`'s
//! `Netlist` instead of a Yosys one, and check the physical simulation
//! against the same independent oracles Tier A already uses (a plain
//! four-input AND, `reda::circuits::seven_segment::TRUTH_TABLE`, and a
//! documented DFF edge/hold trace) rather than against Yosys.

use std::collections::HashMap;

use reda::circuits::seven_segment::TRUTH_TABLE;
use reda::compile::lowering::{lower, lower_optimised};
use reda::compile::topology::GateKind;
use reda::compile::{compile, CompiledCircuit};
use reda::frontend::{compile_systemverilog, CompileArtifact, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;

const AND4: &str = include_str!("fixtures/and4.sv");
const SEVEN_SEGMENT: &str = include_str!("fixtures/seven_segment.sv");
const DFF: &str = include_str!("fixtures/dff.sv");
const DFF_ENABLE: &str = include_str!("fixtures/dff_enable.sv");

fn compile_sv(top: &str, text: &str) -> CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "top.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|diagnostics| panic!("{top} should compile: {diagnostics:?}"))
}

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
        .run_until_stable(2000)
        .expect("circuit must settle after changing an input");
}

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator
        .world()
        .get(position.0, position.1, position.2)
        .lit
}

/// Compile a purely combinational REDA-frontend `Netlist` all the way to
/// redstone and check it against `expected`, the same shape
/// `verilog_frontend.rs`'s `compile_simulate_and_check` uses, trimmed to what
/// this bridge test needs (no timing report, no gate/block comparison table
/// -- those already have dedicated coverage against Yosys).
fn simulate_combinational(
    artifact: &CompileArtifact,
    lower_netlist: fn(
        &reda::compile::Netlist,
    ) -> Result<reda::compile::Netlist, reda::compile::lowering::LowerError>,
    input_names: &[&str],
    output_names: &[&str],
    expected: impl Fn(u32) -> Vec<bool>,
) {
    let output_map = artifact.output_map();
    let lowered = lower_netlist(&artifact.netlist).expect("REDA netlist must lower");
    let compiled: CompiledCircuit = compile(&lowered).expect("REDA netlist must compile");

    let input_positions: HashMap<&str, (i32, i32, i32)> = input_names
        .iter()
        .map(|&name| {
            let port = artifact
                .port(name)
                .unwrap_or_else(|| panic!("port `{name}` must exist"));
            (name, *compiled.input_positions.get(&port.signal).unwrap())
        })
        .collect();
    let output_positions: Vec<(i32, i32, i32)> = output_names
        .iter()
        .map(|&name| *compiled.output_positions.get(&output_map[name]).unwrap())
        .collect();

    let mut simulator = Simulator::new(compiled.world);
    simulator
        .run_until_stable(2000)
        .expect("must settle before the first reading");

    let combinations = 1u32 << input_names.len();
    let mut mismatches = Vec::new();
    for combination in 0..combinations {
        for (i, &name) in input_names.iter().enumerate() {
            let bit = (combination >> (input_names.len() - 1 - i)) & 1;
            set_lever(&mut simulator, input_positions[name], bit == 1);
        }
        let expected_values = expected(combination);
        for (i, &position) in output_positions.iter().enumerate() {
            let actual = read_output(&simulator, position);
            if actual != expected_values[i] {
                mismatches.push(format!(
                    "inputs={combination:#06b} output {}[{i}]: expected {}, got {actual}",
                    output_names[i], expected_values[i]
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "physical simulation does not match the independent oracle ({}/{combinations} wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

/// Tier C for `and4.sv`: the REDA frontend's own `Netlist`, lowered, placed,
/// routed, and simulated with real redstone -- not just the logical
/// evaluator `systemverilog_compiler.rs` already exercises.
#[test]
fn and4_frontend_netlist_matches_its_truth_table_through_the_physical_simulator() {
    let artifact = compile_sv("and4", AND4);
    simulate_combinational(&artifact, lower, &["a", "b", "c", "d"], &["y"], |bits| {
        vec![(0..4).all(|i| (bits >> (3 - i)) & 1 == 1)]
    });
}

/// Tier C for `seven_segment.sv`, checked against the same
/// `reda::circuits::seven_segment::TRUTH_TABLE` Tier A already uses, now
/// through lowering, placement, and the redstone simulator.
#[test]
fn seven_segment_frontend_netlist_matches_its_truth_table_through_the_physical_simulator() {
    let artifact = compile_sv("seven_segment", SEVEN_SEGMENT);
    let segment_names = ["a", "b", "c", "d", "e", "f", "g"];
    simulate_combinational(
        &artifact,
        lower_optimised,
        &["d3", "d2", "d1", "d0"],
        &segment_names,
        |value| {
            if (value as usize) < TRUTH_TABLE.len() {
                TRUTH_TABLE[value as usize]
                    .iter()
                    .map(|&bit| bit == 1)
                    .collect()
            } else {
                vec![false; 7]
            }
        },
    );
}

/// Tier C for `dff.sv`: same edge/hold trace `verilog_frontend.rs`'s
/// `verilog_dff_reaches_the_physical_simulator` uses for the Yosys path, run
/// here against the REDA frontend's own `Netlist` instead.
#[test]
fn dff_frontend_netlist_captures_edges_and_holds_through_the_physical_simulator() {
    let artifact = compile_sv("dff", DFF);
    assert!(artifact
        .netlist
        .gates
        .iter()
        .any(|gate| gate.kind == GateKind::DffPosedge));

    let lowered = lower(&artifact.netlist).expect("DFF netlist must lower");
    let compiled = compile(&lowered).expect("DFF netlist must compile");

    let d = compiled.input_positions[&artifact.port("d").unwrap().signal];
    let clk = compiled.input_positions[&artifact.port("clk").unwrap().signal];
    let q = compiled.output_positions[&artifact.port("q").unwrap().signal];

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(2000).expect("DFF must settle");

    set_lever(&mut simulator, clk, false);
    set_lever(&mut simulator, d, false);
    let mut apply = |position, on| {
        set_lever(&mut simulator, position, on);
        read_output(&simulator, q)
    };
    assert!(
        !apply(clk, true),
        "priming rising edge with d=0 must capture q=0"
    );
    assert!(!apply(clk, false), "falling edge: q must hold at 0");
    assert!(
        !apply(d, true),
        "d changing without a clock edge must hold q"
    );
    assert!(apply(clk, true), "rising edge with d=1 must capture q=1");
    assert!(
        apply(d, false),
        "d changing without a clock edge must hold q at 1"
    );
    assert!(apply(clk, false), "falling edge must hold q at 1");
    assert!(!apply(clk, true), "rising edge with d=0 must capture q=0");
}

/// Tier C for the enable-gated `always_ff` form (`dff_enable.sv`): the hold
/// mux this frontend synthesizes (`systemverilog_dff.rs` proves its logical
/// truth table) must also gate capture correctly once placed and routed as
/// real redstone.
#[test]
fn dff_enable_frontend_netlist_captures_only_when_enabled_through_the_physical_simulator() {
    let artifact = compile_sv("dff_enable", DFF_ENABLE);
    assert!(artifact
        .netlist
        .gates
        .iter()
        .any(|gate| gate.kind == GateKind::DffPosedge));
    assert!(artifact
        .netlist
        .gates
        .iter()
        .any(|gate| gate.kind == GateKind::Mux));

    let lowered = lower(&artifact.netlist).expect("DFF-enable netlist must lower");
    let compiled = compile(&lowered).expect("DFF-enable netlist must compile");

    let d = compiled.input_positions[&artifact.port("d").unwrap().signal];
    let clk = compiled.input_positions[&artifact.port("clk").unwrap().signal];
    let en = compiled.input_positions[&artifact.port("en").unwrap().signal];
    let q = compiled.output_positions[&artifact.port("q").unwrap().signal];

    let mut simulator = Simulator::new(compiled.world);
    simulator
        .run_until_stable(2000)
        .expect("DFF-enable must settle");

    // The physical `DffPosedge` cell (shared by both frontends -- see
    // `verilog_frontend.rs::verilog_dff_reaches_the_physical_simulator`,
    // which relies on the same fact) powers on with an unspecified `q`
    // rather than 0, unlike the logical evaluator, which always initializes
    // `q` to 0. Force one real edge with a known `d` before trusting any
    // expectation, exactly as that existing test does for the plain form.
    set_lever(&mut simulator, clk, false);
    set_lever(&mut simulator, d, false);
    set_lever(&mut simulator, en, true);
    set_lever(&mut simulator, clk, true);
    assert!(!read_output(&simulator, q), "priming edge must capture d=0");
    set_lever(&mut simulator, clk, false);

    // Same (d, clk, en) -> expected-q trace
    // `systemverilog_dff.rs::captures_on_enabled_rising_edges_and_holds_otherwise`
    // checks against the logical evaluator, replayed here lever by lever
    // against real redstone. Each row sets `d`, then `en`, then `clk` (all
    // idempotent if already at that value) so a clock edge always sees that
    // row's final `d`/`en`, matching the evaluator's simultaneous-step
    // semantics; only a real 0-to-1 `clk` transition may change `q`.
    let trace = [
        (true, false, true, false),
        (true, true, false, false),
        (true, true, true, false),
        (true, false, true, false),
        (true, true, true, true),
        (false, true, true, true),
        (false, false, true, true),
        (false, true, false, true),
        (false, false, true, true),
        (false, true, true, false),
    ];
    for (row, &(target_d, target_clk, target_en, expected_q)) in trace.iter().enumerate() {
        set_lever(&mut simulator, d, target_d);
        set_lever(&mut simulator, en, target_en);
        set_lever(&mut simulator, clk, target_clk);
        assert_eq!(
            read_output(&simulator, q),
            expected_q,
            "row {row}: d={target_d} clk={target_clk} en={target_en}: expected q={expected_q}"
        );
    }
}
