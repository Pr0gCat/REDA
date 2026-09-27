//! Milestone 4 shadow validation, per
//! `docs/native-wasm-verilog-compiler-plan.md`'s "Milestone 4: Shadow
//! validation" and "Verification" sections.
//!
//! Production callers still use `synthesize_verilog` (the Yosys path); this
//! file only *observes* `compile_systemverilog` beside it. It adds no
//! dependency and touches no file outside `tests/`: every oracle it compares
//! against is either an independent specification written in this file, or
//! one of the checked-in `verilog::CIRCUITS` baked-Yosys netlists the crate
//! already ships (`src/circuits/baked/*.netlist`). Nothing here spawns
//! Python or Yosys, so it runs wherever `cargo test` already does.
//!
//! What this file checks, in the plan's own words:
//!
//! * semantic, size, and lowering-cost differences between the REDA
//!   compiler and the checked-in Yosys reference, for the whole corpus
//!   (`and4`, `seven_segment`, `dff`) -- [`m4_consolidated_size_and_cost_report`];
//! * deterministic compiles across the corpus, plus deliberately invalid
//!   input -- [`diagnostics_and_netlists_are_deterministic`].
//!
//! Fuzzing (10,000+ random valid expressions against an independent
//! interpreter, plus arbitrary-UTF-8 no-panic fuzzing) lives in
//! `tests/systemverilog_fuzz.rs`, and the debug-sidecar fingerprint's
//! independent-oracle checks live in
//! `tests/netlist_fingerprint_independence.rs`; neither is duplicated here.
//!
//! Cost is reported, never gated: the plan is explicit that the first REDA
//! compiler may be larger than Yosys/ABC's output, and that correctness
//! comes first. Only the semantic-equivalence checks assert. Physical
//! wall-clock timing is intentionally measured by the separate ignored
//! `compile_timing_harness` target; this report must not present the baked
//! Yosys netlist's zero frontend duration as a measurement or hide a physical
//! compile failure behind a cost-only pass.

use std::fmt::Write as _;
use std::time::Instant;

use reda::circuits::verilog;
use reda::compile::lowering::{format_histogram, lower_optimised};
use reda::frontend::evaluate::{inputs_from_bits, Evaluator};
use reda::frontend::{
    compile_systemverilog, CompileArtifact, CompileOptions, Diagnostic, PortDirection, SourceInput,
};

const AND4: &str = include_str!("fixtures/and4.sv");
const SEVEN_SEGMENT: &str = include_str!("fixtures/seven_segment.sv");
const DFF: &str = include_str!("fixtures/dff.sv");

// ---------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------

fn compile_ok(name: &str, text: &str, top: &str) -> CompileArtifact {
    compile_systemverilog(&[SourceInput { name, text }], &CompileOptions::new(top)).unwrap_or_else(
        |diagnostics| {
            let rendered = diagnostics
                .iter()
                .map(|d| d.render(&[SourceInput { name, text }]))
                .collect::<Vec<_>>()
                .join("; ");
            panic!("`{top}` was expected to compile: {rendered}");
        },
    )
}

/// One corpus design's measured shape: gate count and histogram before and
/// after redstone lowering, and how long each stage took. Every field here
/// is a report value -- nothing in this struct is compared against a
/// threshold.
///
/// This stops at `lower_optimised`, the same boundary the rest of the
/// compiler work stops at: placement, routing, and the physical world are
/// unchanged by anything Milestone 4 touches. The physical compile-time
/// sample is therefore kept in the ignored timing harness rather than being
/// implied by this cost report.
struct DesignCost {
    label: String,
    frontend_gate_count: usize,
    frontend_histogram: String,
    frontend_compile_time: std::time::Duration,
    lowered_gate_count: usize,
    lowered_histogram: String,
    lowering_time: std::time::Duration,
}

fn measure(
    label: &str,
    netlist: &reda::compile::Netlist,
    frontend_compile_time: std::time::Duration,
) -> DesignCost {
    let lower_start = Instant::now();
    let lowered = lower_optimised(netlist).expect("netlist must lower into torches and merges");
    let lowering_time = lower_start.elapsed();

    DesignCost {
        label: label.to_string(),
        frontend_gate_count: netlist.gates.len(),
        frontend_histogram: format_histogram(netlist),
        frontend_compile_time,
        lowered_gate_count: lowered.gates.len(),
        lowered_histogram: format_histogram(&lowered),
        lowering_time,
    }
}

impl std::fmt::Display for DesignCost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{label:<24} frontend {fg:>3}g [{fh}] ({ft:>8.0?})  lowered {lg:>3}g [{lh}] ({lt:>8.0?})",
            label = self.label,
            fg = self.frontend_gate_count,
            fh = self.frontend_histogram,
            ft = self.frontend_compile_time,
            lg = self.lowered_gate_count,
            lh = self.lowered_histogram,
            lt = self.lowering_time,
        )
    }
}

// ---------------------------------------------------------------------
// Tier B: REDA netlist vs the checked-in baked Yosys netlist
// ---------------------------------------------------------------------

/// `and4` and `seven_segment` both have a checked-in baked Yosys netlist
/// (`src/circuits/verilog.rs`'s `CIRCUITS`), so both get a real Tier B
/// comparison here. `dff` does not: no `dff.v` fixture and no baked
/// `verilog:dff` catalog entry exist, and adding either would be a catalog
/// change, not a shadow-validation harness. Its row instead runs Tier A
/// (the documented state trace already proven in
/// `tests/systemverilog_dff.rs`) and the consolidated report says so
/// explicitly rather than silently comparing nothing.
#[test]
fn and4_and_seven_segment_are_semantically_equivalent_to_the_baked_yosys_netlist() {
    for (top, source, catalog_name) in [
        ("and4", AND4, "verilog:and4"),
        ("seven_segment", SEVEN_SEGMENT, "verilog:seven_segment"),
    ] {
        let ours = compile_ok("shadow.sv", source, top);
        let circuit = verilog::find(catalog_name).expect("catalog entry ships with the crate");
        let (yosys_netlist, labels) = circuit.baked_netlist();
        let yosys = Evaluator::new(&yosys_netlist).expect("baked netlist is evaluable");
        let ours_evaluator =
            Evaluator::new(&ours.netlist).expect("an emitted netlist is evaluable");

        // Bind rows by declared port *name*, not by position in either
        // netlist's own `inputs` vector: the Yosys JSON bridge exposes
        // inputs alphabetically while the REDA compiler preserves
        // declaration order (the plan's own cutover-fingerprint-churn risk),
        // so `and4`'s `a b c d` happens to agree with both but
        // `seven_segment`'s `d3 d2 d1 d0` does not. Every named input this
        // module declares is present, by name, in both netlists, so a
        // shared name-keyed row is the only comparison that is not an
        // accident of whichever order a frontend chose.
        let input_names: Vec<String> = ours
            .ports
            .iter()
            .filter(|port| port.direction == PortDirection::Input)
            .map(|port| port.name.clone())
            .collect();
        let input_bits = input_names.len() as u32;
        assert_eq!(
            input_bits,
            yosys_netlist.inputs.len() as u32,
            "{top}: REDA and Yosys disagree on input count"
        );

        for row in 0..(1u32 << input_bits) {
            let inputs = inputs_from_bits(&input_names, row);
            let ours_values = ours_evaluator.evaluate(&inputs).expect("evaluates");
            let yosys_values = yosys.evaluate(&inputs).expect("evaluates");
            for (port, signal) in &labels {
                let ours_port = ours
                    .port(port)
                    .unwrap_or_else(|| panic!("REDA has no `{port}` port"));
                assert_eq!(
                    ours_values[&ours_port.signal], yosys_values[signal],
                    "{top} row {row}: output `{port}` disagrees with the baked Yosys netlist"
                );
            }
        }
    }
}

/// `dff`'s Tier A oracle: the documented feed-forward trace from
/// `tests/systemverilog_dff.rs`, run again here as part of the shadow
/// corpus rather than assumed still passing.
#[test]
fn dff_matches_its_documented_state_trace() {
    let artifact = compile_ok("dff.sv", DFF, "dff");
    let mut evaluator = Evaluator::new(&artifact.netlist).expect("valid DFF netlist");
    let output = artifact.netlist.outputs[0].clone();
    let trace = [
        (true, false, false),
        (true, true, true),
        (false, true, true),
        (false, false, true),
        (false, true, false),
    ];
    for (index, &(d, clk, expected)) in trace.iter().enumerate() {
        let inputs =
            std::collections::BTreeMap::from([("d".to_string(), d), ("clk".to_string(), clk)]);
        let got = evaluator.step(&inputs).expect("steps")[&output];
        assert_eq!(got, expected, "dff trace step {index}");
    }
}

// ---------------------------------------------------------------------
// Consolidated size / lowering-cost report
// ---------------------------------------------------------------------

/// Prints (with `--nocapture`) and writes to `target/m4_shadow_report.txt`
/// one consolidated table: REDA's frontend gate count and lowered
/// (redstone) gate count, beside the same numbers for the checked-in Yosys
/// reference where one exists. This is reporting only -- the plan is
/// explicit that a size or lowering-cost gap is a known risk to measure, not
/// a milestone gate, so nothing here asserts on any of these numbers.
#[test]
fn m4_consolidated_size_and_cost_report() {
    let mut report = String::new();
    writeln!(
        report,
        "# Milestone 4 shadow validation: size and lowering cost"
    )
    .unwrap();
    writeln!(
        report,
        "# frontend = CompileArtifact.netlist; lowered = compile::lowering::lower_optimised()\n"
    )
    .unwrap();
    writeln!(
        report,
        "# physical wall-clock timing is reported by the ignored compile_timing_harness;"
    )
    .unwrap();
    writeln!(
        report,
        "# this report has no duration threshold and does not turn physical failures into passes\n"
    )
    .unwrap();

    for (top, source, catalog_name) in [
        ("and4", AND4, Some("verilog:and4")),
        (
            "seven_segment",
            SEVEN_SEGMENT,
            Some("verilog:seven_segment"),
        ),
        ("dff", DFF, None),
    ] {
        let frontend_start = Instant::now();
        let ours = compile_ok("report.sv", source, top);
        let frontend_time = frontend_start.elapsed();
        let ours_cost = measure(&format!("{top} (REDA)"), &ours.netlist, frontend_time);
        writeln!(report, "{ours_cost}").unwrap();

        match catalog_name {
            Some(catalog_name) => {
                let circuit = verilog::find(catalog_name).expect("catalog entry");
                let (yosys_netlist, _labels) = circuit.baked_netlist();
                let yosys_cost = measure(
                    &format!("{top} (Yosys, baked)"),
                    &yosys_netlist,
                    std::time::Duration::ZERO,
                );
                writeln!(report, "{yosys_cost}").unwrap();
                writeln!(
                    report,
                    "  -> frontend gate delta: {:+} ({} REDA vs {} Yosys); lowered gate delta: {:+}\n",
                    ours_cost.frontend_gate_count as i64 - yosys_cost.frontend_gate_count as i64,
                    ours_cost.frontend_gate_count,
                    yosys_cost.frontend_gate_count,
                    ours_cost.lowered_gate_count as i64 - yosys_cost.lowered_gate_count as i64,
                )
                .unwrap();
            }
            None => {
                writeln!(
                    report,
                    "  -> no checked-in baked Yosys reference for `{top}`: no `{top}.v` fixture \
                     or `verilog:{top}` catalog entry exists (adding one is a catalog change, out \
                     of scope for a shadow-validation harness). Compared instead against the \
                     documented Tier A state trace in `dff_matches_its_documented_state_trace`.\n"
                )
                .unwrap();
            }
        }
    }

    println!("{report}");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("m4_shadow_report.txt");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("target/ exists or can be created");
    }
    std::fs::write(&path, &report).expect("can write the shadow report");
    assert!(
        report.contains("and4") && report.contains("seven_segment") && report.contains("dff"),
        "the consolidated report must cover the whole corpus"
    );
}

// ---------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------

/// Compiling the same source twice must be byte-identical: same netlist,
/// same ports, same debug JSON -- checked for the whole named corpus, plus
/// that a deliberately invalid input reports the same diagnostics twice.
/// The randomized-expression side of determinism (compiling generated fuzz
/// programs twice) is covered in `tests/systemverilog_fuzz.rs` and is not
/// repeated here.
#[test]
fn diagnostics_and_netlists_are_deterministic() {
    for (top, source) in [
        ("and4", AND4),
        ("seven_segment", SEVEN_SEGMENT),
        ("dff", DFF),
    ] {
        let first = compile_ok("det.sv", source, top);
        let second = compile_ok("det.sv", source, top);
        assert_eq!(
            first.netlist, second.netlist,
            "{top}: netlist not deterministic"
        );
        assert_eq!(first.ports, second.ports, "{top}: ports not deterministic");
        assert_eq!(
            first.debug.to_json(),
            second.debug.to_json(),
            "{top}: debug sidecar not deterministic"
        );
    }

    let invalid = "module m(input logic a, output logic y);\n  assign y = q;\nendmodule\n";
    let first_errors = diagnostics_of(invalid, "m");
    let second_errors = diagnostics_of(invalid, "m");
    assert_eq!(
        first_errors, second_errors,
        "diagnostics are not deterministic"
    );
}

fn diagnostics_of(text: &str, top: &str) -> Vec<Diagnostic> {
    compile_systemverilog(
        &[SourceInput {
            name: "det-err.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .expect_err("this case is deliberately invalid")
}
