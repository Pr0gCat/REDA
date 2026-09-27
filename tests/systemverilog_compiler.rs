//! Acceptance tests for `reda::frontend::compile_systemverilog` -- the pure
//! Rust SystemVerilog frontend, Milestones 0 and 1.
//!
//! Nothing here runs Python, Yosys, the placer, the router, or the redstone
//! simulator. Truth tables come from `frontend::evaluate`'s logical machine,
//! and the one comparison against Yosys uses the *baked* netlist, which is a
//! checked-in artifact rather than a toolchain. That is the point of this
//! milestone: the compiler and its proofs run anywhere the crate builds,
//! including `wasm32-unknown-unknown`.
//!
//! This file is additive. `tests/verilog_frontend.rs` still owns the Yosys
//! path and its fixtures; `and2.sv` and `and4.sv` sit beside the unchanged
//! `.v` files, and no production caller moves until the plan's Milestone 5.

use std::collections::BTreeMap;

use reda::circuits::verilog;
use reda::compile::topology::GateKind;
use reda::frontend::debug::{
    canonical_netlist_render, netlist_fingerprint, BitBinding, DebugDatabase, GateRef, Realisation,
    TransformReason,
};
use reda::frontend::evaluate::{inputs_from_bits, Evaluator};
use reda::frontend::{
    compile_systemverilog, CompileArtifact, CompileOptions, Diagnostic, FileId, PortDirection,
    Severity, SourceInput, Span,
};

const AND2: &str = include_str!("fixtures/and2.sv");
const AND4: &str = include_str!("fixtures/and4.sv");

fn compile(text: &str, top: &str) -> CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "test.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|diagnostics| {
        panic!(
            "expected `{top}` to compile, got: {}",
            render(&diagnostics, text)
        )
    })
}

fn errors(text: &str, top: &str) -> Vec<Diagnostic> {
    match compile_systemverilog(
        &[SourceInput {
            name: "test.sv",
            text,
        }],
        &CompileOptions::new(top),
    ) {
        Ok(_) => panic!("expected `{top}` to be rejected, but it compiled"),
        Err(diagnostics) => diagnostics,
    }
}

fn render(diagnostics: &[Diagnostic], text: &str) -> String {
    let sources = [SourceInput {
        name: "test.sv",
        text,
    }];
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.render(&sources))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The span of the first occurrence of `needle` in `text`.
fn span_of(text: &str, needle: &str) -> Span {
    span_of_nth(text, needle, 0)
}

/// The span of the `nth` (0-based) occurrence of `needle` in `text` -- for
/// the cases where the same spelling appears both in a declaration and in
/// the statement being checked.
fn span_of_nth(text: &str, needle: &str, nth: usize) -> Span {
    let start = text
        .match_indices(needle)
        .nth(nth)
        .map(|(index, _)| index)
        .unwrap_or_else(|| panic!("`{needle}` does not occur {} time(s)", nth + 1));
    Span::new(FileId(0), start as u32, (start + needle.len()) as u32)
}

fn outputs_of(artifact: &CompileArtifact, row: u32) -> BTreeMap<String, bool> {
    let evaluator = Evaluator::new(&artifact.netlist).expect("an emitted netlist is evaluable");
    let values = evaluator
        .evaluate(&inputs_from_bits(&artifact.netlist.inputs, row))
        .expect("evaluates");
    artifact
        .ports
        .iter()
        .filter(|port| port.direction == PortDirection::Output)
        .map(|port| (port.name.clone(), values[&port.signal]))
        .collect()
}

// ---------------------------------------------------------------------
// Milestone 1: the AND slice
// ---------------------------------------------------------------------

/// The plan's first implementation slice: two inputs, one `GateKind::And`,
/// four rows.
#[test]
fn and2_compiles_to_one_and_gate_whose_four_rows_hold() {
    let artifact = compile(AND2, "and2");

    assert_eq!(artifact.netlist.inputs, ["a", "b"]);
    assert_eq!(artifact.netlist.gates.len(), 1);
    assert_eq!(artifact.netlist.gates[0].kind, GateKind::And);
    assert_eq!(artifact.netlist.gates[0].inputs, ["a", "b"]);
    assert_eq!(
        artifact.netlist.outputs,
        [artifact.netlist.gates[0].output.clone()]
    );

    for row in 0..4u32 {
        let expected = row == 0b11;
        assert_eq!(outputs_of(&artifact, row)["y"], expected, "row {row:02b}");
    }
}

/// Four-input chained AND, exhaustively -- Tier A, against a predicate
/// written independently of the netlist.
#[test]
fn and4_compiles_and_matches_the_independent_predicate() {
    let artifact = compile(AND4, "and4");

    assert_eq!(artifact.netlist.inputs, ["a", "b", "c", "d"]);
    assert_eq!(artifact.netlist.gates.len(), 3);
    assert!(artifact
        .netlist
        .gates
        .iter()
        .all(|gate| gate.kind == GateKind::And));

    for row in 0..16u32 {
        assert_eq!(
            outputs_of(&artifact, row)["y"],
            row == 0b1111,
            "row {row:04b}"
        );
    }
}

/// Tier B without a toolchain: the REDA compiler's netlist and the Yosys
/// netlist baked from `and4.v` agree on every row.
///
/// The two gate graphs are *not* required to be identical -- the
/// compatibility contract is semantic. Here they happen to be the same size;
/// what is asserted is the truth table.
#[test]
fn and4_agrees_with_the_baked_yosys_netlist_row_for_row() {
    let ours = compile(AND4, "and4");
    let circuit = verilog::find("verilog:and4").expect("catalog entry");
    let (yosys_netlist, labels) = circuit.baked_netlist();
    let yosys = Evaluator::new(&yosys_netlist).expect("evaluable");
    let yosys_y = labels
        .iter()
        .find(|(port, _)| port == "y")
        .map(|(_, signal)| signal.clone())
        .expect("a `y` label");

    for row in 0..16u32 {
        let theirs = yosys
            .evaluate(&inputs_from_bits(&yosys_netlist.inputs, row))
            .expect("evaluates");
        assert_eq!(
            outputs_of(&ours, row)["y"],
            theirs[&yosys_y],
            "row {row:04b}"
        );
    }
}

/// Every port bit is reported, sorted by name, with the netlist signal it
/// actually is -- and `y` resolves to the root gate of the expression.
#[test]
fn the_output_port_resolves_to_the_root_gate() {
    let artifact = compile(AND4, "and4");

    let names: Vec<&str> = artifact
        .ports
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(names, ["a", "b", "c", "d", "y"]);

    let y = artifact.port("y").expect("a `y` port");
    assert_eq!(y.direction, PortDirection::Output);
    assert_eq!(y.bit, 0);
    let root = artifact.netlist.gates.last().expect("gates");
    assert_eq!(y.signal, root.output);
    assert_eq!(artifact.netlist.outputs, [root.output.clone()]);
    assert_eq!(artifact.output_map()["y"], root.output);

    // And the same answer arrives through the debug database, from the
    // signal rather than from the port table.
    assert_eq!(
        artifact.debug.gates_for_signal_bit("y", 0),
        vec![GateRef {
            index: 2,
            output: root.output.clone(),
        }]
    );
}

/// A passthrough output needs a gate of its own: `compile` requires every
/// declared output to be driven by one, so `assign y = a;` becomes a `Buf`.
#[test]
fn a_passthrough_output_becomes_a_buffer() {
    let text = "module m(input logic a, output logic y);\n  assign y = a;\nendmodule\n";
    let artifact = compile(text, "m");

    assert_eq!(artifact.netlist.gates.len(), 1);
    assert_eq!(artifact.netlist.gates[0].kind, GateKind::Buf);
    assert_eq!(artifact.netlist.gates[0].inputs, ["a"]);

    for row in 0..2u32 {
        assert_eq!(outputs_of(&artifact, row)["y"], row == 1);
    }

    // The buffer is a mapping event, not a second realisation of `a`: the
    // input node keeps `Realisation::Input`, and the gate is still reachable
    // from the source in both directions.
    let gate = GateRef {
        index: 0,
        output: artifact.netlist.gates[0].output.clone(),
    };
    assert!(artifact
        .debug
        .realisations
        .iter()
        .any(|realisation| *realisation == Realisation::Input("a".to_string())));
    assert!(artifact
        .debug
        .transformations
        .iter()
        .any(|transformation| transformation.reason == TransformReason::Map));
    assert_eq!(
        artifact.debug.gates_for_span(span_of(text, "a;")),
        vec![gate]
    );
}

// ---------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------

/// The complete expression and one nested subexpression each map forward to
/// their own gates, and every gate maps back to the source that asked for
/// it.
#[test]
fn source_expressions_and_gates_find_each_other_in_both_directions() {
    let artifact = compile(AND4, "and4");

    // Forward, at two scales: the whole right-hand side is every gate, and
    // the innermost `a & b` is exactly the first one.
    let whole = artifact
        .debug
        .gates_for_span(span_of(AND4, "a & b & c & d"));
    assert_eq!(whole.len(), 3);
    assert_eq!(
        whole.iter().map(|gate| gate.index).collect::<Vec<_>>(),
        [0, 1, 2]
    );

    let nested = artifact.debug.gates_for_span(span_of(AND4, "a & b"));
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].index, 0);

    // The whole `assign` statement finds the same three gates: a statement
    // span contains its expressions' spans.
    assert_eq!(
        artifact
            .debug
            .gates_for_span(span_of(AND4, "assign y = a & b & c & d;")),
        whole
    );

    // Reverse: the root gate names the complete expression, and the first
    // gate names only `a & b`.
    let root_origins = artifact.debug.origins_for_gate(&whole[2]);
    assert_eq!(root_origins.len(), 1);
    assert_eq!(root_origins[0].span, span_of(AND4, "a & b & c & d"));
    assert_eq!(root_origins[0].bit, 0);
    assert!(!root_origins[0].extra);
    assert!(root_origins[0].synthetic.is_none());

    let nested_origins = artifact.debug.origins_for_gate(&nested[0]);
    assert_eq!(nested_origins.len(), 1);
    assert_eq!(nested_origins[0].span, span_of(AND4, "a & b"));

    // A span containing no expression contains no gates.
    assert!(artifact
        .debug
        .gates_for_span(span_of(AND4, "input  logic a"))
        .is_empty());
}

/// One gate serving two identical expressions keeps both origins: the first
/// is the node's embedded parent, the second arrives through interning.
#[test]
fn an_interned_gate_reports_every_expression_that_shares_it() {
    let text = "module m(input logic a, input logic b, output logic y, output logic z);\n  \
                assign y = a & b;\n  assign z = b & a;\nendmodule\n";
    let artifact = compile(text, "m");

    // Commutative operands are ordered before interning, so `a & b` and
    // `b & a` are one gate -- and therefore one netlist output signal with
    // two port names.
    assert_eq!(artifact.netlist.gates.len(), 1);
    assert_eq!(artifact.netlist.outputs.len(), 1);
    assert_eq!(
        artifact.port("y").unwrap().signal,
        artifact.port("z").unwrap().signal
    );

    let gate = GateRef {
        index: 0,
        output: artifact.netlist.gates[0].output.clone(),
    };
    let origins = artifact.debug.origins_for_gate(&gate);
    assert_eq!(origins.len(), 2);
    assert_eq!(origins[0].span, span_of(text, "a & b"));
    assert!(!origins[0].extra);
    assert_eq!(origins[1].span, span_of(text, "b & a"));
    assert!(
        origins[1].extra,
        "the second origin arrived through interning"
    );

    // Both spans find the gate going forward, too.
    assert_eq!(
        artifact.debug.gates_for_span(span_of(text, "b & a")),
        vec![gate]
    );
}

/// `signal_bits` tells a primary input, a folded constant, and real logic
/// apart -- and `why_missing` explains the source that produced no gate.
#[test]
fn signal_bits_and_why_missing_explain_what_has_no_gate() {
    let text = "module m(input logic a, input logic b, output logic y);\n  \
                logic z;\n  assign z = a & 1'b0;\n  assign y = (a & b) | z;\nendmodule\n";
    let artifact = compile(text, "m");

    // `z` folds away, so `y` is just `a & b`: one gate, no merge.
    assert_eq!(artifact.netlist.gates.len(), 1);
    assert_eq!(artifact.netlist.gates[0].kind, GateKind::And);

    let a = artifact.debug.signal_bits("a").expect("`a` is bound");
    assert_eq!(a.bits, vec![BitBinding::Input("a".to_string())]);

    let z = artifact.debug.signal_bits("z").expect("`z` is bound");
    assert_eq!(z.bits, vec![BitBinding::Const(false)]);

    let y = artifact.debug.signal_bits("y").expect("`y` is bound");
    assert!(matches!(y.bits.as_slice(), [BitBinding::Logic(_)]));

    // The folded expression has no gate, and the database says why and what
    // survived instead.
    let folded = source_node_at(&artifact.debug, span_of(text, "a & 1'b0"));
    let missing = artifact.debug.why_missing(folded).expect("no gate");
    assert_eq!(missing.reason, TransformReason::Fold);
    assert!(missing.surviving.contains(&Realisation::Const(false)));
    assert!(artifact
        .debug
        .gates_for_span(span_of(text, "a & 1'b0"))
        .is_empty());

    // The surviving expression still has one.
    assert_eq!(
        artifact.debug.gates_for_span(span_of(text, "a & b")).len(),
        1
    );
}

/// Dead logic stays in the arena and stays queryable although no gate is
/// emitted for it.
#[test]
fn dead_logic_is_recorded_rather_than_forgotten() {
    let text = "module m(input logic a, input logic b, output logic y);\n  \
                logic unused;\n  assign unused = a ^ b;\n  assign y = a & b;\nendmodule\n";
    let artifact = compile(text, "m");

    // Only the live AND is emitted.
    assert_eq!(artifact.netlist.gates.len(), 1);
    assert_eq!(artifact.netlist.gates[0].kind, GateKind::And);

    let dead = source_node_at(&artifact.debug, span_of(text, "a ^ b"));
    let missing = artifact.debug.why_missing(dead).expect("no gate");
    assert_eq!(missing.reason, TransformReason::Dead);
    assert_eq!(missing.surviving, vec![Realisation::Dead]);
    assert!(artifact
        .debug
        .transformations
        .iter()
        .any(|transformation| transformation.reason == TransformReason::Dead));
}

/// The first source node whose span is exactly `span`.
fn source_node_at(debug: &DebugDatabase, span: Span) -> reda::frontend::SourceNodeId {
    debug
        .source_nodes
        .iter()
        .position(|node| node.span == span)
        .map(|index| reda::frontend::SourceNodeId(index as u32))
        .unwrap_or_else(|| panic!("no source node spans {span:?}"))
}

// ---------------------------------------------------------------------
// Determinism and the fingerprint
// ---------------------------------------------------------------------

#[test]
fn compiling_the_same_source_twice_is_byte_identical() {
    let first = compile(AND4, "and4");
    let second = compile(AND4, "and4");

    assert_eq!(
        canonical_netlist_render(&first.netlist),
        canonical_netlist_render(&second.netlist)
    );
    assert_eq!(first.netlist, second.netlist);
    assert_eq!(first.ports, second.ports);
    assert_eq!(first.debug.to_json(), second.debug.to_json());
}

/// Milestone 0's exit criterion: an empty debug database serializes
/// identically twice, and round-trips.
#[test]
fn an_empty_debug_database_serializes_identically_twice() {
    let empty = DebugDatabase::default();
    assert_eq!(empty.to_json(), empty.to_json());
    assert_eq!(empty.to_json(), DebugDatabase::default().to_json());
    assert_eq!(
        DebugDatabase::from_json(&empty.to_json()).expect("round-trips"),
        empty
    );
}

/// Inserting comments moves byte spans and nothing else: the semantic IDs,
/// the gate names, and the netlist fingerprint are unchanged.
#[test]
fn comments_move_spans_but_not_identities() {
    let plain = compile(AND4, "and4");
    let commented_text = AND4.replace(
        "assign y = a & b & c & d;",
        "/* the whole circuit */ assign y = a /* first */ & b & c & d; // done\n",
    );
    let commented = compile(&commented_text, "and4");

    assert_eq!(
        plain.debug.netlist_fingerprint,
        commented.debug.netlist_fingerprint
    );
    assert_eq!(plain.netlist, commented.netlist);
    assert_eq!(
        plain
            .netlist
            .gates
            .iter()
            .map(|gate| gate.name.clone())
            .collect::<Vec<_>>(),
        commented
            .netlist
            .gates
            .iter()
            .map(|gate| gate.name.clone())
            .collect::<Vec<_>>()
    );

    // Semantic IDs: the source node table has the same kinds and names in
    // the same order, the elaborated nodes are identical, and so is every
    // logic origin -- while the spans have all moved.
    assert_eq!(plain.debug.elab_nodes, commented.debug.elab_nodes);
    assert_eq!(plain.debug.logic_origins, commented.debug.logic_origins);
    assert_eq!(plain.debug.realisations, commented.debug.realisations);
    let kinds = |debug: &DebugDatabase| {
        debug
            .source_nodes
            .iter()
            .map(|node| (node.kind, node.name.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(&plain.debug), kinds(&commented.debug));
    let spans = |debug: &DebugDatabase| {
        debug
            .source_nodes
            .iter()
            .map(|node| node.span)
            .collect::<Vec<_>>()
    };
    assert_ne!(
        spans(&plain.debug),
        spans(&commented.debug),
        "the comments must actually have moved some spans"
    );
}

/// The fingerprint is the join key between this sidecar and any other
/// artifact describing the same netlist -- so it has to reject a netlist
/// that is not the one it was taken over.
#[test]
fn the_fingerprint_matches_its_own_netlist_and_no_other() {
    let artifact = compile(AND4, "and4");
    assert!(artifact.debug.matches(&artifact.netlist));
    assert_eq!(
        artifact.debug.netlist_fingerprint,
        netlist_fingerprint(&artifact.netlist)
    );

    let mut mutated = artifact.netlist.clone();
    mutated.gates.swap(0, 1);
    assert!(
        !artifact.debug.matches(&mutated),
        "reordering gates moves every GateRef index, so the sidecar must not claim it"
    );

    // A different circuit fingerprints differently.
    let other = compile(AND2, "and2");
    assert_ne!(
        artifact.debug.netlist_fingerprint,
        other.debug.netlist_fingerprint
    );
}

// ---------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------

/// Milestone 0's exit criterion: a minimal module gets a typed unsupported
/// diagnostic rather than an empty netlist.
#[test]
fn a_module_with_nothing_to_compile_is_rejected() {
    let text = "module top;\nendmodule\n";
    let diagnostics = errors(text, "top");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, Severity::Error);
    assert!(
        diagnostics[0].message.contains("no output ports"),
        "{}",
        diagnostics[0].message
    );
    assert_eq!(diagnostics[0].span, span_of(text, "top"));
}

#[test]
fn a_missing_top_module_names_what_is_there() {
    let diagnostics = errors(AND2, "and4");
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].message.contains("no module named `and4`"));
    assert!(diagnostics[0].message.contains("and2"));
}

/// Version 1 has no implicit extension or truncation, no unsized literals,
/// no ascending ranges, and no procedural blocks yet. Each rejection is one
/// diagnostic at a stable, pointed span.
#[test]
fn the_rejection_matrix_has_stable_messages_and_spans() {
    let cases: &[(&str, &str, usize, &str)] = &[
        (
            "module m(input logic [1:0] a, output logic y);\n  assign y = a;\nendmodule\n",
            "assign y = a;",
            0,
            "assignment width mismatch",
        ),
        (
            "module m(input logic a, input logic [1:0] b, output logic y);\n  assign y = a & b;\nendmodule\n",
            "a & b",
            0,
            "operands of `&`",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = 1;\nendmodule\n",
            "1",
            0,
            "unsized literal",
        ),
        (
            "module m(input logic [0:1] a, output logic y);\n  assign y = a[0];\nendmodule\n",
            "[0:1]",
            0,
            "must be declared as `[N-1:0]`",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = q;\nendmodule\n",
            "q",
            0,
            "unknown signal `q`",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = a[3];\nendmodule\n",
            "a[3]",
            0,
            "out of range",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = a\nendmodule\n",
            "endmodule",
            0,
            "expected `;`",
        ),
        (
            "module m(input logic a, output logic y);\n  always_comb y = a;\nendmodule\n",
            "y",
            2,
            "expected `begin`",
        ),
        (
            "module m(input logic a, output logic y);\n  reg z;\n  assign y = a;\nendmodule\n",
            "reg",
            0,
            "legacy `reg` is not supported",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = a;\n  assign y = ~a;\nendmodule\n",
            "assign y = ~a;",
            0,
            "driven by more than one",
        ),
        (
            "module m(input logic a, output logic y, output logic z);\n  assign y = a;\nendmodule\n",
            "output logic z",
            0,
            "has no driver",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = a & 1'b0;\nendmodule\n",
            "output logic y",
            0,
            "folds to the constant 0",
        ),
        (
            "module m(input logic w, output logic y);\n  assign w = y;\nendmodule\n",
            "w",
            1,
            "is an input port and cannot be assigned",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = 2'b11;\nendmodule\n",
            "assign y = 2'b11;",
            0,
            "assignment width mismatch",
        ),
        (
            "module m(input logic a, output logic y);\n  assign y = 1'b2;\nendmodule\n",
            "1'b2",
            0,
            "is not a binary digit",
        ),
    ];

    for (text, needle, occurrence, expected) in cases {
        let diagnostics = errors(text, "m");
        assert_eq!(
            diagnostics.len(),
            1,
            "`{expected}` should be one diagnostic, got: {}",
            render(&diagnostics, text)
        );
        assert!(
            diagnostics[0].message.contains(expected),
            "expected a message containing `{expected}`, got `{}`",
            diagnostics[0].message
        );
        assert_eq!(
            diagnostics[0].span,
            span_of_nth(text, needle, *occurrence),
            "`{expected}` pointed at `{}` instead of `{needle}`",
            &text[diagnostics[0].span.start as usize..diagnostics[0].span.end as usize]
        );
    }
}

/// Procedural rejection cases that make it past syntax parsing must still be
/// rejected at the pass that has enough information to explain them.  Keep
/// the source spans tight: these are editor-facing diagnostics, not merely a
/// boolean "unsupported" result.
#[test]
fn procedural_rejection_matrix_has_precise_diagnostics() {
    let read_before_write = "module m(input logic sel, input logic a, output logic y);\n  always_comb begin\n    case (sel)\n      1'b0: y = y & a;\n      default: y = a;\n    endcase\n  end\nendmodule\n";
    let diagnostics = errors(read_before_write, "m");
    assert_eq!(
        diagnostics.len(),
        1,
        "{}",
        render(&diagnostics, read_before_write)
    );
    assert!(diagnostics[0]
        .message
        .contains("read-before-write in `always_comb`"));
    assert_eq!(diagnostics[0].span, span_of_nth(read_before_write, "y", 3));

    let incomplete = "module m(input logic sel, input logic a, output logic y);\n  always_comb begin\n    case (sel)\n      1'b0: y = a;\n    endcase\n  end\nendmodule\n";
    let diagnostics = errors(incomplete, "m");
    assert_eq!(diagnostics.len(), 1, "{}", render(&diagnostics, incomplete));
    assert!(diagnostics[0]
        .message
        .contains("needs exactly one `default` arm to avoid a latch"));
    assert_eq!(
        diagnostics[0].span.start,
        span_of(incomplete, "case (sel)").start
    );
    assert!(diagnostics[0].span.end > diagnostics[0].span.start);

    let dual_writer = "module m(input logic sel, input logic a, output logic y);\n  always_comb begin\n    case (sel)\n      1'b0: y = a;\n      default: y = a;\n    endcase\n  end\n  always_comb begin\n    case (sel)\n      1'b0: y = ~a;\n      default: y = ~a;\n    endcase\n  end\nendmodule\n";
    let diagnostics = errors(dual_writer, "m");
    assert_eq!(
        diagnostics.len(),
        1,
        "{}",
        render(&diagnostics, dual_writer)
    );
    assert!(diagnostics[0].message.contains("driven by more than one"));
    assert_eq!(
        diagnostics[0].span.start,
        span_of_nth(dual_writer, "always_comb", 1).start
    );
    assert!(diagnostics[0].span.end > diagnostics[0].span.start);
}

/// Independent item-level errors accumulate and come back sorted by
/// position, rather than the compile stopping at the first one.
#[test]
fn elaboration_errors_accumulate_in_source_order() {
    let text =
        "module m(input logic a, output logic y);\n  assign y = p;\n  assign y = q;\nendmodule\n";
    let diagnostics = errors(text, "m");
    assert_eq!(diagnostics.len(), 2, "{}", render(&diagnostics, text));
    assert!(diagnostics[0].message.contains("`p`"));
    assert!(diagnostics[1].message.contains("`q`"));
    assert!(diagnostics[0].span.start < diagnostics[1].span.start);
}

/// A diagnostic renders with the host's own file name and a 1-based
/// line/column derived from its byte offsets.
#[test]
fn diagnostics_render_with_file_line_and_column() {
    let text = "module m(input logic a, output logic y);\n  assign y = q;\nendmodule\n";
    let diagnostics = errors(text, "m");
    assert_eq!(
        diagnostics[0].render(&[SourceInput {
            name: "widget.sv",
            text,
        }]),
        "widget.sv:2:14: unknown signal `q`"
    );
}

/// Several files in, several files out: `FileId` indexes the slice the
/// caller passed, so a diagnostic in the second file says so.
#[test]
fn a_source_set_can_hold_more_than_one_file() {
    let first = "module other(input logic a, output logic y);\n  assign y = a;\nendmodule\n";
    let second = "module m(input logic a, output logic y);\n  assign y = nope;\nendmodule\n";
    let diagnostics = compile_systemverilog(
        &[
            SourceInput {
                name: "first.sv",
                text: first,
            },
            SourceInput {
                name: "second.sv",
                text: second,
            },
        ],
        &CompileOptions::new("m"),
    )
    .expect_err("`nope` is undeclared");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].span.file, FileId(1));

    // And the module in the other file compiles on its own.
    let artifact = compile_systemverilog(
        &[
            SourceInput {
                name: "first.sv",
                text: first,
            },
            SourceInput {
                name: "second.sv",
                text: "module m(input logic a, output logic y);\n  assign y = ~a;\nendmodule\n",
            },
        ],
        &CompileOptions::new("other"),
    )
    .expect("compiles");
    assert_eq!(artifact.debug.files.len(), 2);
    assert_eq!(artifact.debug.files[1].name, "second.sv");
}

// ---------------------------------------------------------------------
// The rest of the version 1 expression vocabulary
// ---------------------------------------------------------------------

/// Every expression form continuous assignment can use, checked against a
/// predicate over all four input rows. Each one exercises a different path
/// through elaboration, RTL, the logic graph, and gate mapping.
#[test]
fn the_supported_expression_forms_all_evaluate_correctly() {
    let cases: &[(&str, fn(bool, bool) -> bool)] = &[
        ("a & b", |a, b| a && b),
        ("a | b", |a, b| a || b),
        ("a ^ b", |a, b| a ^ b),
        ("~a", |a, _| !a),
        ("!a", |a, _| !a),
        ("a && b", |a, b| a && b),
        ("a || b", |a, b| a || b),
        ("a == b", |a, b| a == b),
        ("a != b", |a, b| a != b),
        ("a ? b : ~b", |a, b| if a { b } else { !b }),
        ("{a, b} == 2'b10", |a, b| a && !b),
        ("(a & ~b) | (~a & b)", |a, b| a ^ b),
        ("{a, b} != {b, a}", |a, b| a != b),
    ];

    for (expression, expected) in cases {
        let text = format!(
            "module m(input logic a, input logic b, output logic y);\n  assign y = {expression};\nendmodule\n"
        );
        let artifact = compile(&text, "m");
        for row in 0..4u32 {
            let a = row & 1 == 1;
            let b = row & 2 == 2;
            assert_eq!(
                outputs_of(&artifact, row)["y"],
                expected(a, b),
                "`{expression}` with a={a} b={b}"
            );
        }
    }
}

/// Vectors, part selects, and per-bit ports: a multi-bit port becomes one
/// netlist signal per bit, named `name[i]` LSB-first, and each bit is bound
/// on its own.
#[test]
fn a_vector_port_is_one_netlist_signal_per_bit() {
    let text = "module m(input logic [3:0] a, output logic [1:0] y);\n  \
                assign y = a[3:2] & a[1:0];\nendmodule\n";
    let artifact = compile(text, "m");

    assert_eq!(artifact.netlist.inputs, ["a[0]", "a[1]", "a[2]", "a[3]"]);
    assert_eq!(artifact.netlist.gates.len(), 2);
    let names: Vec<&str> = artifact
        .ports
        .iter()
        .map(|port| port.name.as_str())
        .collect();
    assert_eq!(names, ["a[0]", "a[1]", "a[2]", "a[3]", "y[0]", "y[1]"]);

    let y = artifact.debug.signal_bits("y").expect("`y` is bound");
    assert_eq!(y.bits.len(), 2);

    for row in 0..16u32 {
        let outputs = outputs_of(&artifact, row);
        let high = (row >> 2) & 0b11;
        let low = row & 0b11;
        let expected = high & low;
        assert_eq!(outputs["y[0]"], expected & 1 == 1, "row {row:04b}");
        assert_eq!(outputs["y[1]"], expected & 2 == 2, "row {row:04b}");
    }
}
