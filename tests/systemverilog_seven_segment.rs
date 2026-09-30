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

#[test]
fn mutually_exclusive_case_arms_keep_data_defaults_and_source_origins() {
    // Exercise dynamic arm data, a missing selector value, nonzero defaults,
    // and reversed source order; no decoder-specific truth table is involved.
    for default in ["2'b00", "2'b10", "{a, b}"] {
        for reverse in [false, true] {
            let mut arms = ["2'd0: y = {a, b};", "2'd2: y = {b, a};"];
            if reverse {
                arms.reverse();
            }
            let source = format!("module top(input logic [1:0] s, input logic a, input logic b, output logic [1:0] y); always_comb begin case (s) {} {} default: y = {default}; endcase end endmodule", arms[0], arms[1]);
            let artifact = compile_systemverilog(
                &[SourceInput {
                    name: "case.sv",
                    text: &source,
                }],
                &CompileOptions::new("top"),
            )
            .unwrap();
            let evaluator = Evaluator::new(&artifact.netlist).unwrap();
            for s in 0..4 {
                for data in 0..4 {
                    let (a, b) = (data & 2 != 0, data & 1 != 0);
                    let inputs = BTreeMap::from([
                        ("s[0]".into(), s & 1 != 0),
                        ("s[1]".into(), s & 2 != 0),
                        ("a".into(), a),
                        ("b".into(), b),
                    ]);
                    let expected = match s {
                        0 => [b, a],
                        2 => [a, b],
                        _ => match default {
                            "2'b00" => [false, false],
                            "2'b10" => [false, true],
                            _ => [b, a],
                        },
                    };
                    let got = evaluator.evaluate(&inputs).unwrap();
                    for (i, expected) in expected.into_iter().enumerate() {
                        assert_eq!(
                            got[&artifact.port(&format!("y[{i}]")).unwrap().signal],
                            expected
                        );
                    }
                }
            }
            for gate in &artifact.debug.gates {
                assert!(!artifact.debug.origins_for_gate(gate).is_empty());
            }
        }
    }
}

#[test]
fn constant_cases_match_generated_truth_tables_including_nonzero_defaults() {
    let mut seed = 0x9e3779b9u32;
    for width in [1u32, 3, 6, 7] {
        for sample in 0..8 {
            let default = sample & 1 != 0;
            let mut expected = vec![default; 1 << width];
            let mut arms = Vec::new();
            for (index, value) in expected.iter_mut().enumerate() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                if index < 2 || seed & 3 == 0 {
                    *value = if index < 2 { index == 1 } else { seed & 8 != 0 };
                    arms.push(format!("{width}'d{index}: y = 1'b{};", u8::from(*value)));
                }
            }
            if sample & 2 != 0 {
                arms.reverse();
            }
            let source = format!("module top(input logic [{}:0] s, output logic y); always_comb begin case (s) {} default: y = 1'b{}; endcase end endmodule", width - 1, arms.join(" "), u8::from(default));
            let inputs = [SourceInput {
                name: "generated.sv",
                text: &source,
            }];
            let artifact = compile_systemverilog(&inputs, &CompileOptions::new("top")).unwrap();
            let again = compile_systemverilog(&inputs, &CompileOptions::new("top")).unwrap();
            assert_eq!(artifact.netlist, again.netlist);
            assert_eq!(artifact.debug.to_json(), again.debug.to_json());
            let evaluator = Evaluator::new(&artifact.netlist).unwrap();
            for (value, expected) in expected.into_iter().enumerate() {
                let inputs = (0..width)
                    .map(|bit| {
                        (
                            if width == 1 {
                                "s".to_string()
                            } else {
                                format!("s[{bit}]")
                            },
                            value & (1 << bit) != 0,
                        )
                    })
                    .collect();
                assert_eq!(
                    evaluator.evaluate(&inputs).unwrap()[&artifact.port("y").unwrap().signal],
                    expected,
                    "width={width} sample={sample} input={value}"
                );
            }
        }
    }
}

#[test]
fn minimized_case_keeps_label_and_value_source_links() {
    use reda::frontend::source::{FileId, Span};
    let artifact = compile();
    for spelling in ["4'd0", "7'b1111110", "4'd9", "7'b1111011", "7'b0000000"] {
        let start = SOURCE.find(spelling).unwrap() as u32;
        let span = Span::new(FileId(0), start, start + spelling.len() as u32);
        let gates = artifact.debug.gates_for_span(span);
        assert!(
            !gates.is_empty(),
            "{spelling} lost during case minimization"
        );
        assert!(gates.iter().any(|gate| artifact
            .debug
            .origins_for_gate(gate)
            .iter()
            .any(|origin| origin.span == span)));
    }
}
