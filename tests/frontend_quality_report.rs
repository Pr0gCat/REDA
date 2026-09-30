//! Physical frontend quality gate, with a fresh Yosys comparison.
//! Run: source tools/env.sh && cargo test --release --test frontend_quality_report -- --nocapture
use std::collections::HashMap;
use std::time::Instant;

use reda::circuits::{seven_segment::TRUTH_TABLE, verilog};
use reda::compile::{
    compile, compile_legacy, lowering::lower_optimised, metrics::physical_metrics, Netlist,
};
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use serde_json::{json, Value};

type Position = (i32, i32, i32);

fn apply(sim: &mut Simulator, inputs: &[Position], bits: u32) -> u64 {
    for (i, &(x, y, z)) in inputs.iter().enumerate() {
        let mut state = sim.world().get(x, y, z).clone();
        state.lit = bits & (1 << (inputs.len() - 1 - i)) != 0;
        sim.world_mut().set(x, y, z, state);
    }
    let start = sim.current_tick();
    sim.run_until_stable(2000)
        .expect("input transition must settle");
    sim.current_tick() - start
}

fn measure(top: &str, frontend: &str, netlist: Netlist, outputs: HashMap<String, String>) -> Value {
    let started = Instant::now();
    let lowered = lower_optimised(&netlist).expect("lowering succeeds");
    let compiled = compile(&lowered).expect("physical compilation succeeds");
    let compile_ms = started.elapsed().as_secs_f64() * 1000.0;
    let metrics = physical_metrics(&compiled.world, lowered.gates.len() as u64);
    let fingerprint =
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world);
    let input_names: &[&str] = if top == "and4" {
        &["a", "b", "c", "d"]
    } else {
        &["d3", "d2", "d1", "d0"]
    };
    let output_names: &[&str] = if top == "and4" {
        &["y"]
    } else {
        &["a", "b", "c", "d", "e", "f", "g"]
    };
    let inputs: Vec<_> = input_names
        .iter()
        .map(|name| compiled.input_positions[*name])
        .collect();
    let positions: Vec<_> = output_names
        .iter()
        .map(|name| compiled.output_positions[&outputs[*name]])
        .collect();
    let planner = format!("{:?}", compiled.planner_kind());
    let mut sim = Simulator::new(compiled.world);
    sim.run_until_stable(2000).expect("initial world settles");
    let mut worst_ticks = 0;
    let mut worst_pair = (0, 0);
    let mut sum_ticks = 0;
    for from in 0..16 {
        for to in 0..16 {
            apply(&mut sim, &inputs, from);
            let ticks = apply(&mut sim, &inputs, to);
            if ticks > worst_ticks {
                worst_ticks = ticks;
                worst_pair = (from, to);
            }
            sum_ticks += ticks;
            let expected: Vec<bool> = if top == "and4" {
                vec![to == 15]
            } else if to < 10 {
                TRUTH_TABLE[to as usize]
                    .iter()
                    .map(|&bit| bit == 1)
                    .collect()
            } else {
                vec![false; 7]
            };
            for (i, &(x, y, z)) in positions.iter().enumerate() {
                assert_eq!(
                    sim.world().get(x, y, z).lit,
                    expected[i],
                    "{top}/{frontend}: {from}->{to}, output {}",
                    output_names[i]
                );
            }
        }
    }
    if frontend == "REDA" {
        let (blocks, volume, ticks) = if top == "and4" {
            (228, 3528, 28)
        } else {
            (7136, 68460, 46)
        };
        assert!(
            metrics.non_air_blocks <= blocks,
            "{top}: block regression: {metrics:?}"
        );
        assert!(
            metrics.occupied_volume <= volume,
            "{top}: volume regression: {metrics:?}"
        );
        assert!(
            worst_ticks <= ticks,
            "{top}: settle regression: {worst_ticks}"
        );
    }
    let legacy_baseline = if top == "seven_segment" {
        let original = compile_legacy(&lowered).expect("historical channel baseline compiles");
        let baseline = physical_metrics(&original.world, lowered.gates.len() as u64);
        assert!(metrics.non_air_blocks < baseline.non_air_blocks);
        assert!(metrics.occupied_volume < baseline.occupied_volume);
        Some(json!({"physical": baseline, "world_fingerprint":
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&original.world)}))
    } else {
        None
    };
    let row = json!({
        "case": top, "frontend": frontend,
        "frontend_gates": netlist.gates.len(), "lowered_gates": lowered.gates.len(),
        "lowered_histogram": reda::compile::lowering::format_histogram(&lowered),
        "physical": metrics, "historical_channel_baseline": legacy_baseline, "planner": planner, "world_fingerprint": fingerprint,
        "lower_and_compile_ms": compile_ms,
        "transitions": 256, "worst_settle_ticks": worst_ticks,
        "worst_transition": worst_pair, "mean_settle_ticks": sum_ticks as f64 / 256.0,
    });
    println!("{row}");
    row
}

#[test]
fn compare_frontend_physical_quality() {
    let mut rows = Vec::new();
    for (top, source, catalog_name) in [
        ("and4", include_str!("fixtures/and4.sv"), "verilog:and4"),
        (
            "seven_segment",
            include_str!("fixtures/seven_segment.sv"),
            "verilog:seven_segment",
        ),
    ] {
        let artifact = compile_systemverilog(
            &[SourceInput {
                name: "quality.sv",
                text: source,
            }],
            &CompileOptions::new(top),
        )
        .expect("REDA frontend succeeds");
        let outputs = artifact.output_map();
        rows.push(measure(top, "REDA", artifact.netlist, outputs));
        let reference = verilog::find(catalog_name).unwrap();
        // Use the checked-in equivalent Verilog fixture as the independent oracle.
        let (netlist, outputs) = synthesize_verilog(reference.source, reference.top_module)
            .expect("fresh Yosys succeeds");
        rows.push(measure(top, "Yosys-fresh", netlist, outputs));
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("frontend_quality_report.json"),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();
}
