//! Fresh-Yosys differential for the native SystemVerilog DFF slice.
//!
//! This is intentionally separate from the baked-catalog shadow tests: it
//! invokes the existing Yosys bridge on `dff.v` at test time, then compares
//! logical state transitions against the REDA compiler's `dff.sv` netlist.
//! The comparison is semantic (the same edge/hold trace), not a gate-shape
//! comparison, so Yosys remains free to name or arrange its cells differently.

use std::collections::BTreeMap;

use reda::compile::topology::GateKind;
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{
    compile_systemverilog, synthesize_verilog, CompileArtifact, CompileOptions, SourceInput,
};

const SYSTEMVERILOG_DFF: &str = include_str!("fixtures/dff.sv");
const VERILOG_DFF: &str = include_str!("fixtures/dff.v");

fn compile_systemverilog_dff() -> CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "tests/fixtures/dff.sv",
            text: SYSTEMVERILOG_DFF,
        }],
        &CompileOptions::new("dff"),
    )
    .unwrap_or_else(|diagnostics| {
        let rendered = diagnostics
            .iter()
            .map(|diagnostic| {
                diagnostic.render(&[SourceInput {
                    name: "tests/fixtures/dff.sv",
                    text: SYSTEMVERILOG_DFF,
                }])
            })
            .collect::<Vec<_>>()
            .join("; ");
        panic!("fresh DFF differential: REDA SystemVerilog compile failed: {rendered}");
    })
}

fn fresh_yosys_dff() -> (
    reda::compile::Netlist,
    std::collections::HashMap<String, String>,
) {
    let python = std::env::var("REDA_PYTHON").unwrap_or_default();
    assert!(
        !python.trim().is_empty(),
        "fresh DFF differential requires REDA_PYTHON to point to Python with yowasp-yosys; run:\n  REDA_PYTHON=/private/tmp/reda-uv/bin/python3 cargo test --test m3_m4_dff_yosys_differential -- --nocapture"
    );

    synthesize_verilog(VERILOG_DFF, "dff_example").unwrap_or_else(|error| {
        panic!(
            "fresh DFF differential: Yosys synthesis failed using REDA_PYTHON={python:?}: {error}"
        )
    })
}

fn inputs(d: bool, clk: bool) -> BTreeMap<String, bool> {
    [("d".to_string(), d), ("clk".to_string(), clk)]
        .into_iter()
        .collect()
}

#[test]
fn native_systemverilog_dff_matches_fresh_yosys_on_edge_and_hold_trace() {
    let reda = compile_systemverilog_dff();
    let (yosys_netlist, yosys_ports) = fresh_yosys_dff();

    assert_eq!(reda.netlist.inputs, vec!["d", "clk"]);
    assert!(
        reda.netlist
            .gates
            .iter()
            .any(|gate| gate.kind == GateKind::DffPosedge),
        "REDA DFF fixture must emit a positive-edge DFF"
    );
    assert!(
        yosys_netlist
            .gates
            .iter()
            .any(|gate| gate.kind == GateKind::DffPosedge),
        "fresh Yosys DFF fixture must emit a positive-edge DFF"
    );

    let reda_q = reda
        .port("q")
        .unwrap_or_else(|| panic!("REDA DFF fixture has no output port `q`"))
        .signal
        .clone();
    let yosys_q = yosys_ports.get("q").unwrap_or_else(|| {
        panic!("fresh Yosys DFF fixture has no output port `q`; ports: {yosys_ports:?}")
    });

    let mut reda_eval = Evaluator::new(&reda.netlist)
        .unwrap_or_else(|error| panic!("REDA DFF netlist is not evaluable: {error}"));
    let mut yosys_eval = Evaluator::new(&yosys_netlist)
        .unwrap_or_else(|error| panic!("fresh Yosys DFF netlist is not evaluable: {error}"));

    // (d, clk, expected q), starting from both Evaluators' all-zero state.
    // This covers a non-edge, a capture, high-level hold, falling-edge hold,
    // and a second capture with a changed data value.
    let trace = [
        (true, false, false),
        (true, true, true),
        (false, true, true),
        (false, false, true),
        (false, true, false),
    ];
    for (step, &(d, clk, expected)) in trace.iter().enumerate() {
        let row = inputs(d, clk);
        let reda_values = reda_eval
            .step(&row)
            .unwrap_or_else(|error| panic!("REDA DFF trace step {step} failed: {error}"));
        let yosys_values = yosys_eval
            .step(&row)
            .unwrap_or_else(|error| panic!("fresh Yosys DFF trace step {step} failed: {error}"));
        let reda_q_value = reda_values[&reda_q];
        let yosys_q_value = yosys_values[yosys_q];
        assert_eq!(
            reda_q_value, yosys_q_value,
            "fresh DFF differential mismatch at step {step}: d={d}, clk={clk}, REDA q={reda_q_value}, Yosys q={yosys_q_value}"
        );
        assert_eq!(
            reda_q_value, expected,
            "fresh DFF differential trace oracle mismatch at step {step}: d={d}, clk={clk}"
        );
    }
}
