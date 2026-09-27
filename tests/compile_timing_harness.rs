//! Milestone 4 ("Shadow validation"): "Record frontend time beside physical
//! compile time without changing baked netlists or generator baselines."
//!
//! This is a measurement harness, not a benchmark gate: it prints wall-clock
//! timings for the SystemVerilog frontend and for the existing physical
//! compiler side by side, for every checked-in fixture, and asserts nothing
//! about how long either phase takes. No baked netlist, generator baseline,
//! or production caller is touched -- `reda::compile::compile` and
//! `reda::compile::lowering::lower` are called exactly as any other caller
//! already calls them.
//!
//! `#[ignore]`d because wall-clock measurement does not belong in the
//! ordinary, deterministic test run. Run it explicitly:
//!
//! ```text
//! cargo test --test compile_timing_harness -- --ignored --nocapture
//! ```

use std::time::Instant;

use reda::compile::lowering::lower;
use reda::compile::{compile, Netlist};
use reda::frontend::{compile_systemverilog, CompileOptions, Diagnostic, SourceInput};

const AND4: &str = include_str!("fixtures/and4.sv");
const SEVEN_SEGMENT: &str = include_str!("fixtures/seven_segment.sv");
const DFF: &str = include_str!("fixtures/dff.sv");
const DFF_ENABLE: &str = include_str!("fixtures/dff_enable.sv");

struct Case {
    file_name: &'static str,
    source: &'static str,
    top: &'static str,
}

const CASES: &[Case] = &[
    Case {
        file_name: "and4.sv",
        source: AND4,
        top: "and4",
    },
    Case {
        file_name: "seven_segment.sv",
        source: SEVEN_SEGMENT,
        top: "seven_segment",
    },
    Case {
        file_name: "dff.sv",
        source: DFF,
        top: "dff",
    },
    Case {
        file_name: "dff_enable.sv",
        source: DFF_ENABLE,
        top: "dff_enable",
    },
];

fn render(diagnostics: &[Diagnostic], name: &str, text: &str) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.render(&[SourceInput { name, text }]))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A measured stage. Duration is deliberately informational; a stage failure
/// is not. In particular, a physical compile error must remain visible and
/// fail the ignored harness rather than being converted into a passing result.
enum Stage {
    Ok(std::time::Duration),
    Failed {
        elapsed: std::time::Duration,
        error: String,
    },
    Skipped(&'static str),
}

impl Stage {
    fn ok(elapsed: std::time::Duration) -> Self {
        Self::Ok(elapsed)
    }

    fn failed(elapsed: std::time::Duration, error: impl Into<String>) -> Self {
        Self::Failed {
            elapsed,
            error: error.into().replace('\n', "\\n"),
        }
    }

    fn is_ok(&self) -> bool {
        matches!(self, Self::Ok(_))
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok(elapsed) => write!(formatter, "ok ({elapsed:?})"),
            Self::Failed { elapsed, error } => {
                write!(formatter, "FAIL ({elapsed:?}): {error}")
            }
            Self::Skipped(reason) => write!(formatter, "SKIP ({reason})"),
        }
    }
}

struct Timing {
    file_name: &'static str,
    top: &'static str,
    gate_count: Option<usize>,
    frontend: Stage,
    lowering: Stage,
    physical: Stage,
}

fn measure(case: &Case) -> Timing {
    let frontend_start = Instant::now();
    let frontend_result = compile_systemverilog(
        &[SourceInput {
            name: case.file_name,
            text: case.source,
        }],
        &CompileOptions::new(case.top),
    );
    let frontend_elapsed = frontend_start.elapsed();
    let (artifact, frontend) = match frontend_result {
        Ok(artifact) => (Some(artifact), Stage::ok(frontend_elapsed)),
        Err(diagnostics) => (
            None,
            Stage::failed(
                frontend_elapsed,
                render(&diagnostics, case.file_name, case.source),
            ),
        ),
    };

    let Some(artifact) = artifact else {
        return Timing {
            file_name: case.file_name,
            top: case.top,
            gate_count: None,
            frontend,
            lowering: Stage::Skipped("frontend failed"),
            physical: Stage::Skipped("frontend failed"),
        };
    };

    let lowering_start = Instant::now();
    let lowering_result: Result<Netlist, _> = lower(&artifact.netlist);
    let lowering_elapsed = lowering_start.elapsed();
    let (lowered, lowering) = match lowering_result {
        Ok(lowered) => (Some(lowered), Stage::ok(lowering_elapsed)),
        Err(error) => (None, Stage::failed(lowering_elapsed, error.to_string())),
    };

    let Some(lowered) = lowered else {
        return Timing {
            file_name: case.file_name,
            top: case.top,
            gate_count: Some(artifact.netlist.gates.len()),
            frontend,
            lowering,
            physical: Stage::Skipped("lowering failed"),
        };
    };

    let physical_start = Instant::now();
    let physical_result = compile(&lowered);
    let physical_elapsed = physical_start.elapsed();
    let physical = match physical_result {
        Ok(compiled) => {
            // Touch the result so it is not dead code the optimizer could
            // reasonably remove, without asserting anything about its shape.
            let _ = &compiled;
            Stage::ok(physical_elapsed)
        }
        Err(error) => Stage::failed(physical_elapsed, error.to_string()),
    };

    Timing {
        file_name: case.file_name,
        top: case.top,
        gate_count: Some(artifact.netlist.gates.len()),
        frontend,
        lowering,
        physical,
    }
}

#[test]
#[ignore = "wall-clock measurement harness; run with `--ignored` to see timings"]
fn frontend_and_physical_compile_time_side_by_side() {
    let timings: Vec<Timing> = CASES.iter().map(measure).collect();

    println!("M4 timing sample (wall-clock; no duration threshold)");
    println!("stage failures are test failures; durations are informational");
    for timing in &timings {
        println!(
            "fixture={} top={} gates={} frontend={} lowering={} physical={}",
            timing.file_name,
            timing.top,
            timing
                .gate_count
                .map_or_else(|| "-".to_string(), |count| count.to_string()),
            timing.frontend,
            timing.lowering,
            timing.physical,
        );
    }

    // Do not compare durations: they vary with host load and are not a CI
    // performance gate. Do assert stage outcomes, so a physical failure is
    // never reported as a successful timing sample.
    assert_eq!(timings.len(), CASES.len());
    assert!(
        timings.iter().all(|timing| {
            timing.frontend.is_ok() && timing.lowering.is_ok() && timing.physical.is_ok()
        }),
        "one or more frontend/lowering/physical stages failed; see the timing rows above"
    );
}
