//! Milestone 2 acceptance fixture for the SystemVerilog seven-segment decoder.
//!
//! The fixture intentionally uses the modern, synthesizable SystemVerilog
//! spelling (`logic`, `always_comb`, packed vectors, and a concatenated case
//! selector).  It is kept separate from the Yosys tests so this remains a
//! pure-Rust/native+WASM compiler contract.

use std::collections::BTreeMap;

use reda::circuits::seven_segment::{SEGMENT_NAMES, TRUTH_TABLE};
use reda::circuits::verilog;
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, PortDirection, SourceInput};

const SOURCE: &str = include_str!("fixtures/seven_segment.sv");

fn compile() -> reda::frontend::CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "seven_segment.sv",
            text: SOURCE,
        }],
        &CompileOptions::new("seven_segment"),
    )
    .unwrap_or_else(|diagnostics| {
        let rendered = diagnostics
            .iter()
            .map(|diagnostic| {
                diagnostic.render(&[SourceInput {
                    name: "seven_segment.sv",
                    text: SOURCE,
                }])
            })
            .collect::<Vec<_>>()
            .join("; ");
        panic!("seven-segment fixture should compile: {rendered}");
    })
}

/// The M2 fixture's complete observable contract: input rows 0..15 map to
/// `seg[6:0]` in the same `a..g` order as REDA's reference truth table.
#[test]
fn seven_segment_matches_reference_for_all_bcd_rows() {
    let artifact = compile();
    let evaluator = Evaluator::new(&artifact.netlist).expect("emitted netlist is evaluable");
    let (baked, baked_labels) = verilog::find("verilog:seven_segment")
        .expect("checked-in Yosys fixture exists")
        .baked_netlist();
    let baked_evaluator = Evaluator::new(&baked).expect("baked netlist is evaluable");
    let outputs: Vec<_> = artifact
        .ports
        .iter()
        .filter(|port| port.direction == PortDirection::Output)
        .collect();
    assert_eq!(
        outputs.len(),
        7,
        "only the seven segment outputs are public"
    );

    for row in 0..16u32 {
        let inputs = BTreeMap::from([
            ("d3".to_string(), row & 0b1000 != 0),
            ("d2".to_string(), row & 0b0100 != 0),
            ("d1".to_string(), row & 0b0010 != 0),
            ("d0".to_string(), row & 0b0001 != 0),
        ]);
        let values = evaluator.evaluate(&inputs).expect("evaluation succeeds");
        let baked_values = baked_evaluator
            .evaluate(&inputs)
            .expect("baked evaluation succeeds");
        let expected = TRUTH_TABLE.get(row as usize).copied().unwrap_or([0; 7]);
        for (index, name) in SEGMENT_NAMES.iter().enumerate() {
            let binding = artifact
                .ports
                .iter()
                .find(|port| port.name == *name)
                .unwrap_or_else(|| panic!("missing output binding `{name}`"));
            assert_eq!(
                values[&binding.signal],
                expected[index] != 0,
                "row {row}, {name}"
            );
            let baked_signal = baked_labels
                .iter()
                .find(|(port, _)| port == name)
                .map(|(_, signal)| signal)
                .unwrap_or_else(|| panic!("missing baked output `{name}`"));
            assert_eq!(
                values[&binding.signal], baked_values[baked_signal],
                "native compiler differs from baked Yosys at row {row}, {name}"
            );
        }
    }
}

#[test]
fn seven_segment_compile_is_deterministic_and_provenance_is_joinable() {
    let first = compile();
    let second = compile();
    assert_eq!(first.netlist, second.netlist);
    assert_eq!(first.debug.to_json(), second.debug.to_json());
    assert!(first.debug.matches(&first.netlist));
    assert_eq!(first.output_map().len(), 7);
    assert!(first
        .ports
        .iter()
        .filter(|port| port.direction == PortDirection::Output)
        .all(|port| SEGMENT_NAMES.contains(&port.name.as_str())));
    for port in first
        .ports
        .iter()
        .filter(|port| port.direction == PortDirection::Output)
    {
        let gate = first
            .debug
            .gates
            .iter()
            .find(|gate| gate.output == port.signal)
            .unwrap_or_else(|| panic!("output `{}` has no debug gate", port.name));
        assert!(!first.debug.origins_for_gate(gate).is_empty());
    }
}

#[test]
fn seven_segment_fixture_rejects_a_missing_top_module() {
    let result = compile_systemverilog(
        &[SourceInput {
            name: "seven_segment.sv",
            text: SOURCE,
        }],
        &CompileOptions::new("does_not_exist"),
    );
    if let Err(diagnostics) = result {
        assert!(!diagnostics.is_empty());
    } else {
        panic!("missing top module must be rejected");
    }
}

fn rejection(body: &str) -> Vec<reda::frontend::Diagnostic> {
    let source = format!(
        "module top(input logic [1:0] d, output logic y, output logic z);\n\
         always_comb begin\n{body}\nend\nendmodule\n"
    );
    compile_systemverilog(
        &[SourceInput {
            name: "reject.sv",
            text: &source,
        }],
        &CompileOptions::new("top"),
    )
    .expect_err("the unsupported case shape must be rejected")
}

#[test]
fn incomplete_and_ambiguous_cases_are_rejected_with_spans() {
    let cases = [
        (
            "case (d)\n2'd0: y = 1'b0;\nendcase",
            "needs exactly one `default` arm",
        ),
        (
            "case (d)\ndefault: y = 1'b0;\ndefault: y = 1'b1;\nendcase",
            "only one `default` arm",
        ),
        (
            "case (d)\n2'd0: y = 1'b0;\n2'd0: y = 1'b1;\ndefault: y = 1'b0;\nendcase",
            "duplicate case label",
        ),
        (
            "case (d)\n1'd0: y = 1'b0;\ndefault: y = 1'b1;\nendcase",
            "case label is 1 bit(s) wide but the selector is 2 bit(s)",
        ),
        (
            "case (d)\n2'd0: y = 1'b0;\ndefault: z = 1'b1;\nendcase",
            "must assign the same signal slice",
        ),
    ];

    for (body, expected) in cases {
        let diagnostics = rejection(body);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains(expected))
            .unwrap_or_else(|| panic!("missing `{expected}` in {diagnostics:?}"));
        assert!(diagnostic.span.end > diagnostic.span.start);
    }
}
