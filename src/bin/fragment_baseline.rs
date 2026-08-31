use std::path::PathBuf;

use reda::compile::fragment_synth::benchmark::{
    current_git_commit, legacy_benchmark_evaluator, refuse_existing_output, write_baseline_json,
};

struct Arguments {
    output: PathBuf,
    replace: bool,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut output = None;
    let mut replace = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--output" => {
                let path = arguments
                    .next()
                    .ok_or_else(|| "--output requires a path".to_string())?;
                if output.replace(PathBuf::from(path)).is_some() {
                    return Err("--output may be supplied only once".to_string());
                }
            }
            "--replace" => replace = true,
            unknown => return Err(format!("unknown argument `{unknown}`")),
        }
    }
    Ok(Arguments {
        output: output
            .ok_or_else(|| "usage: fragment_baseline --output PATH [--replace]".to_string())?,
        replace,
    })
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments()?;
    refuse_existing_output(&arguments.output, arguments.replace)?;
    let evaluator = legacy_benchmark_evaluator()?;
    let baseline = evaluator.capture_legacy(current_git_commit()?);
    write_baseline_json(&arguments.output, &baseline, arguments.replace)?;

    for case in &baseline.cases {
        if case.certified {
            let blocks = case
                .physical
                .as_ref()
                .map(|physical| physical.non_air_blocks)
                .unwrap_or(0);
            let ticks = case.max_observed_settle_game_ticks_on_manifest.unwrap_or(0);
            println!(
                "{:<34} certified  blocks={blocks}  max_ticks={ticks}",
                case.name
            );
        } else {
            println!("{:<34} new-coverage", case.name);
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fragment_baseline: {error}");
        std::process::exit(2);
    }
}
