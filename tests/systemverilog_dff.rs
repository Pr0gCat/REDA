use std::collections::BTreeMap;

use reda::compile::topology::GateKind;
use reda::frontend::debug::SyntheticOrigin;
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{
    compile_systemverilog, CompileArtifact, CompileOptions, SourceInput, SourceKind,
};

const DFF: &str = include_str!("fixtures/dff.sv");
const DFF_ENABLE: &str = include_str!("fixtures/dff_enable.sv");

fn compile(text: &str) -> CompileArtifact {
    compile_top("dff", text)
}

fn compile_top(top: &str, text: &str) -> CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "dff.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|errors| panic!("DFF compile failed: {errors:?}"))
}

fn rejected(text: &str, expected: &str) {
    rejected_top("dff", text, expected)
}

fn rejected_top(top: &str, text: &str, expected: &str) {
    let errors = compile_systemverilog(
        &[SourceInput {
            name: "dff.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .expect_err("unsupported DFF form compiled");
    assert!(
        errors.iter().any(|error| error.message.contains(expected)),
        "expected `{expected}` in {errors:?}"
    );
}

fn inputs(d: bool, clk: bool) -> BTreeMap<String, bool> {
    [("d".to_string(), d), ("clk".to_string(), clk)]
        .into_iter()
        .collect()
}

fn enable_inputs(d: bool, clk: bool, en: bool) -> BTreeMap<String, bool> {
    [
        ("d".to_string(), d),
        ("clk".to_string(), clk),
        ("en".to_string(), en),
    ]
    .into_iter()
    .collect()
}

#[test]
fn emits_one_posedge_dff_with_d_then_clock_pins() {
    let artifact = compile(DFF);
    assert_eq!(artifact.netlist.gates.len(), 1);
    let gate = &artifact.netlist.gates[0];
    assert_eq!(gate.kind, GateKind::DffPosedge);
    assert_eq!(gate.inputs, ["d", "clk"]);
    assert_eq!(artifact.netlist.outputs, [gate.output.clone()]);
    assert_eq!(artifact.port("q").unwrap().signal, gate.output);
}

#[test]
fn captures_only_on_rising_edges_and_holds_elsewhere() {
    let artifact = compile(DFF);
    let mut evaluator = Evaluator::new(&artifact.netlist).expect("valid DFF netlist");
    let trace = [
        (true, false, false),
        (true, true, true),
        (false, true, true),
        (false, false, true),
        (false, true, false),
    ];
    for &(d, clk, expected) in &trace {
        assert_eq!(
            evaluator.step(&inputs(d, clk)).unwrap()[&artifact.netlist.outputs[0]],
            expected
        );
    }
}

#[test]
fn dff_compile_is_deterministic_and_provenance_reaches_the_gate() {
    let first = compile(DFF);
    let second = compile(DFF);
    assert_eq!(first.debug.to_json(), second.debug.to_json());
    assert_eq!(first.netlist, second.netlist);
    let q = first.port("q").unwrap();
    let gate = first.debug.gates_for_signal_bit(&q.name, q.bit);
    assert_eq!(gate.len(), 1);
    assert_eq!(first.debug.origins_for_gate(&gate[0]).len(), 1);
    assert!(first.debug.matches(&first.netlist));
}

#[test]
fn nonblocking_assign_source_node_keeps_its_statement_span() {
    let artifact = compile(DFF);
    let nodes: Vec<_> = artifact
        .debug
        .source_nodes
        .iter()
        .filter(|node| node.kind == SourceKind::NonblockingAssign)
        .collect();

    assert_eq!(nodes.len(), 1);
    let node = nodes[0];
    let span = node.span;
    assert_eq!(&DFF[span.start as usize..span.end as usize], "q <= d;");
}

#[test]
fn rejects_every_form_outside_the_feed_forward_slice() {
    rejected(
        &DFF.replace("posedge", "negedge"),
        "only supports `posedge`",
    );
    rejected(
        &DFF.replace("posedge clk", "posedge clk or posedge reset"),
        "multiple clocks",
    );
    rejected(&DFF.replace("q <= d;", "q = d;"), "requires a nonblocking");
    rejected(&DFF.replace("q <= d;", "q <= q;"), "feedback");
}

#[test]
fn clock_rejections_keep_the_operator_or_expression_span() {
    let async_reset = DFF.replace("posedge clk", "posedge clk or posedge reset");
    let errors = compile_systemverilog(
        &[SourceInput {
            name: "async_reset.sv",
            text: &async_reset,
        }],
        &CompileOptions::new("dff"),
    )
    .expect_err("asynchronous reset must be rejected by the parser");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("multiple clocks"));
    assert!(errors[0]
        .message
        .contains("asynchronous resets are not supported"));
    assert_eq!(
        &async_reset[errors[0].span.start as usize..errors[0].span.end as usize],
        "or"
    );

    let derived_clock = "module dff(\n  input logic d,\n  input logic clk,\n  input logic en,\n  output logic q\n);\n  always_ff @(posedge (clk & en)) begin\n    q <= d;\n  end\nendmodule\n";
    let errors = compile_systemverilog(
        &[SourceInput {
            name: "derived_clock.sv",
            text: derived_clock,
        }],
        &CompileOptions::new("dff"),
    )
    .expect_err("a derived clock expression must be rejected by elaboration");
    assert_eq!(errors.len(), 1);
    assert!(errors[0]
        .message
        .contains("clock must be one top-level input identifier"));
    assert_eq!(
        &derived_clock[errors[0].span.start as usize..errors[0].span.end as usize],
        "clk & en"
    );
}

#[test]
fn emits_one_posedge_dff_gated_by_a_hold_mux_with_d_then_clock_pins() {
    let artifact = compile_top("dff_enable", DFF_ENABLE);
    assert_eq!(artifact.netlist.gates.len(), 2);

    let dff = artifact
        .netlist
        .gates
        .iter()
        .find(|gate| gate.kind == GateKind::DffPosedge)
        .expect("exactly one DFF gate");
    let mux = artifact
        .netlist
        .gates
        .iter()
        .find(|gate| gate.kind == GateKind::Mux)
        .expect("exactly one hold mux gate");

    // `GateKind::DffPosedge`'s fixed pin order is `[D, C]`; the hold mux's
    // own output is what feeds the DFF's D pin.
    assert_eq!(dff.inputs, [mux.output.clone(), "clk".to_string()]);
    assert_eq!(artifact.netlist.outputs, [dff.output.clone()]);
    assert_eq!(artifact.port("q").unwrap().signal, dff.output);
}

#[test]
fn captures_on_enabled_rising_edges_and_holds_otherwise() {
    let artifact = compile_top("dff_enable", DFF_ENABLE);
    let mut evaluator = Evaluator::new(&artifact.netlist).expect("valid hold-mux DFF netlist");
    // (d, clk, en, expected q after the step)
    let trace = [
        (true, false, true, false), // no edge yet
        (true, true, false, false), // rising edge, but disabled: hold
        (true, true, true, false),  // no edge (clk already high): hold
        (true, false, true, false), // falling edge: hold regardless of `en`
        (true, true, true, true),   // rising edge, enabled: capture d=1
        (false, true, true, true),  // no edge: hold
        (false, false, true, true), // falling edge: hold
        (false, true, false, true), // rising edge, disabled: hold
        (false, false, true, true), // falling edge: hold
        (false, true, true, false), // rising edge, enabled: capture d=0
    ];
    for &(d, clk, en, expected) in &trace {
        assert_eq!(
            evaluator.step(&enable_inputs(d, clk, en)).unwrap()[&artifact.netlist.outputs[0]],
            expected
        );
    }
}

#[test]
fn hold_mux_is_queryable_in_the_debug_database_with_its_source_span() {
    let artifact = compile_top("dff_enable", DFF_ENABLE);
    let q = artifact.port("q").unwrap();
    let gates = artifact.debug.gates_for_signal_bit(&q.name, q.bit);
    // The DFF's own gate, reached through its embedded origin.
    assert_eq!(gates.len(), 1);

    let mux_gate = artifact
        .netlist
        .gates
        .iter()
        .position(|gate| gate.kind == GateKind::Mux)
        .expect("a hold mux gate");
    let mux_ref = &artifact.debug.gates[mux_gate];
    let origins = artifact.debug.origins_for_gate(mux_ref);
    assert!(
        origins
            .iter()
            .any(|origin| origin.synthetic == Some(SyntheticOrigin::HoldMux)),
        "expected a HoldMux origin among {origins:?}"
    );
    let span = origins[0].span;
    assert!(
        span.end > span.start,
        "the hold mux keeps a real source span"
    );
}

#[test]
fn enable_form_compile_is_deterministic() {
    let first = compile_top("dff_enable", DFF_ENABLE);
    let second = compile_top("dff_enable", DFF_ENABLE);
    assert_eq!(first.debug.to_json(), second.debug.to_json());
    assert_eq!(first.netlist, second.netlist);
    assert!(first.debug.matches(&first.netlist));
}

#[test]
fn rejects_every_enable_form_outside_the_slice() {
    rejected_top(
        "dff_enable",
        &DFF_ENABLE.replace("if (en) q <= d;", "if (en) q <= d; else q <= d;"),
        "`else`",
    );
    rejected_top(
        "dff_enable",
        &DFF_ENABLE.replace("q <= d;", "q <= q;"),
        "feedback",
    );
    rejected_top(
        "dff_enable",
        &DFF_ENABLE.replace("if (en) q <= d;", "if (q) q <= d;"),
        "feedback",
    );
}
