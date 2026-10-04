use std::path::{Path, PathBuf};

use reda::compile::fragment_synth::benchmark::{
    build_acceptance_report, legacy_benchmark_evaluator, shipping_config_source,
    write_acceptance_json, write_generated_text, BenchmarkBaseline,
};

const BUDGETS: [u64; 5] = [0, 1, 2, 4, 8];

struct Arguments {
    baseline: PathBuf,
    output: PathBuf,
    shipping_source: PathBuf,
    shuffle_seed: u64,
}

fn parse_seed(value: &str) -> Result<u64, String> {
    u64::from_str_radix(value.strip_prefix("0x").unwrap_or(value), 16)
        .map_err(|error| format!("invalid --shuffle-seed `{value}`: {error}"))
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut baseline = None;
    let mut output = None;
    let mut shipping_source = None;
    let mut shuffle_seed = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("{argument} requires a value"))?;
        match argument.as_str() {
            "--baseline" => baseline = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--shipping-source" => shipping_source = Some(PathBuf::from(value)),
            "--shuffle-seed" => shuffle_seed = Some(parse_seed(&value)?),
            unknown => return Err(format!("unknown argument `{unknown}`")),
        }
    }
    Ok(Arguments {
        baseline: baseline.ok_or_else(|| "--baseline is required".to_string())?,
        output: output.ok_or_else(|| "--output is required".to_string())?,
        shipping_source: shipping_source
            .ok_or_else(|| "--shipping-source is required".to_string())?,
        shuffle_seed: shuffle_seed.ok_or_else(|| "--shuffle-seed is required".to_string())?,
    })
}

fn read_baseline(path: &Path) -> Result<BenchmarkBaseline, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| format!("invalid baseline {}: {error}", path.display()))
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments()?;
    if arguments.shipping_source.exists() {
        return Err(format!(
            "shipping source {} must be absent before the gate",
            arguments.shipping_source.display()
        ));
    }
    let baseline = read_baseline(&arguments.baseline)?;
    let evaluator = legacy_benchmark_evaluator()?;
    let report = build_acceptance_report(&evaluator, &baseline, &BUDGETS, arguments.shuffle_seed);
    write_acceptance_json(&arguments.output, &report)?;
    if report.replacement_gate_passed {
        let source = shipping_config_source(&report)?;
        write_generated_text(&arguments.shipping_source, &source)?;
    }
    println!(
        "replacement_gate_passed={} shipping_evaluations={:?} failures={}",
        report.replacement_gate_passed,
        report.shipping_evaluations,
        report.failures.len()
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fragment_acceptance: {error}");
        std::process::exit(2);
    }
}
