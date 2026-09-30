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
    let drivers: Vec<_> = output_names
        .iter()
        .map(|name| compiled.gate_output_positions[&outputs[*name]])
        .collect();
    let planner = format!("{:?}", compiled.planner_kind());
    let mut sim = Simulator::new(compiled.world);
    sim.run_until_stable(2000).expect("initial world settles");
    sim.attach_observer(
        positions
            .iter()
            .zip(output_names)
            .map(|(&(x, y, z), name)| {
                (
                    reda::redstone::simulator::position::Position::new(x, y, z),
                    (*name).to_string(),
                )
            })
            .chain(drivers.iter().zip(output_names).map(|(&(x, y, z), name)| {
                (
                    reda::redstone::simulator::position::Position::new(x, y, z),
                    format!("driver:{name}"),
                )
            })),
    );
    let mut driver_traces = Vec::new();
    let mut worst_driver_ticks = 0;
    let mut driver_glitch_transitions = 0;
    let mut driver_single_input_glitches = 0;
    let mut worst_driver_pulse_ticks = 0;
    let mut total_driver_pulse_ticks = 0;
    let mut output_traces = Vec::new();
    let mut worst_output_ticks = 0;
    let mut glitch_transitions = 0;
    let mut worst_ticks = 0;
    let mut worst_pair = (0, 0);
    let mut sum_ticks = 0;
    for from in 0..16 {
        for to in 0..16 {
            apply(&mut sim, &inputs, from);
            sim.reset_observer();
            let start_tick = sim.current_tick();
            let ticks = apply(&mut sim, &inputs, to);
            let timing =
                reda::timing::observations_to_result(sim.observations(), start_tick, ticks);
            let output_ticks = timing
                .nets
                .iter()
                .filter(|(name, _)| !name.starts_with("driver:"))
                .filter_map(|(_, net)| net.arrival_tick())
                .max()
                .unwrap_or(0);
            worst_output_ticks = worst_output_ticks.max(output_ticks);
            let glitched = timing
                .nets
                .iter()
                .any(|(name, net)| !name.starts_with("driver:") && net.glitched());
            glitch_transitions += usize::from(glitched);
            output_traces.push(json!({"from":from, "to":to,
                "last_output_change_tick":output_ticks, "glitched":glitched,
                "changes": timing.nets.iter().filter(|(name,_)| !name.starts_with("driver:")).map(|(name, net)|
                    (name.clone(), json!(net.changes()))).collect::<std::collections::BTreeMap<_,_>>() }));
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
            let driver_timings: Vec<_> = timing
                .nets
                .iter()
                .filter(|(name, _)| name.starts_with("driver:"))
                .collect();
            let driver_ticks = driver_timings
                .iter()
                .filter_map(|(_, net)| net.arrival_tick())
                .max()
                .unwrap_or(0);
            let driver_glitched = driver_timings.iter().any(|(_, net)| net.glitched());
            worst_driver_ticks = worst_driver_ticks.max(driver_ticks);
            driver_glitch_transitions += usize::from(driver_glitched);
            driver_single_input_glitches +=
                usize::from(driver_glitched && (from ^ to).count_ones() == 1);
            let mut pulses = std::collections::BTreeMap::new();
            for (i, name) in output_names.iter().enumerate() {
                if let Some(net) = timing.nets.get(&format!("driver:{name}")) {
                    let widths: Vec<_> = net
                        .changes()
                        .windows(2)
                        .filter(|events| events[0].1 != expected[i])
                        .map(|events| events[1].0 - events[0].0)
                        .collect();
                    worst_driver_pulse_ticks =
                        worst_driver_pulse_ticks.max(widths.iter().copied().max().unwrap_or(0));
                    total_driver_pulse_ticks += widths.iter().sum::<u64>();
                    if !widths.is_empty() {
                        pulses.insert(*name, widths);
                    }
                }
                let (x, y, z) = drivers[i];
                let state = sim.world().get(x, y, z);
                let value = if state.kind == reda::redstone::world::block::BlockKind::RedstoneWire {
                    state.power > 0
                } else {
                    state.lit
                };
                assert_eq!(
                    value, expected[i],
                    "{top}/{frontend}: driver {name}, {from}->{to}"
                );
            }
            driver_traces.push(json!({"from":from,"to":to,"last_driver_change_tick":driver_ticks,
                "glitched":driver_glitched,"pulse_widths":pulses,
                "changes":driver_timings.iter().map(|(name,net)| ((*name).clone(),json!(net.changes())))
                    .collect::<std::collections::BTreeMap<_,_>>() }));
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
    // Retain complete measurements even when a later quality gate rejects
    // the candidate; aggregate counters alone cannot locate the new pulse.
    let diagnostic_target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    std::fs::create_dir_all(&diagnostic_target).unwrap();
    std::fs::write(diagnostic_target.join(format!("driver_timing_{top}_{frontend}.json")),
        serde_json::to_string_pretty(&json!({"case":top,"frontend":frontend,
            "world_fingerprint":fingerprint,"worst_driver_change_ticks":worst_driver_ticks,
            "driver_glitch_transitions":driver_glitch_transitions,
            "driver_single_input_glitches":driver_single_input_glitches,
            "worst_driver_pulse_ticks":worst_driver_pulse_ticks,"total_driver_pulse_ticks":total_driver_pulse_ticks,
            "driver_traces":&driver_traces,"output_traces":&output_traces})).unwrap()).unwrap();
    let single_input_glitches = output_traces
        .iter()
        .filter(|trace| {
            trace["glitched"] == true
                && (trace["from"].as_u64().unwrap() ^ trace["to"].as_u64().unwrap()).count_ones()
                    == 1
        })
        .count();
    let output_baselines: Value = serde_json::from_str(include_str!(
        "../docs/generator-output-timing-baseline-2026-09-30.json"
    ))
    .unwrap();
    let output_baseline = output_baselines["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["case"] == top && row["frontend"] == frontend)
        .unwrap();
    eprintln!("{top}/{frontend}: world={worst_ticks}, output={worst_output_ticks}, mean={}, glitches={glitch_transitions}, single_input_glitches={single_input_glitches}",sum_ticks as f64 / 256.0);
    assert!(
        worst_output_ticks
            <= output_baseline["worst_output_change_ticks"]
                .as_u64()
                .unwrap(),
        "{top}/{frontend}: output timing regression"
    );
    assert!(
        glitch_transitions as u64 <= output_baseline["glitch_transitions"].as_u64().unwrap(),
        "{top}/{frontend}: output glitch regression"
    );
    assert!(
        single_input_glitches as u64
            <= output_baseline["single_input_glitch_transitions"]
                .as_u64()
                .unwrap(),
        "{top}/{frontend}: single input glitch regression"
    );
    let driver_baselines: Value = serde_json::from_str(include_str!(
        "../docs/generator-driver-timing-baseline-2026-09-30.json"
    ))
    .unwrap();
    let driver_baseline = driver_baselines["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["case"] == top && row["frontend"] == frontend)
        .unwrap();
    for (name, measured) in [
        ("worst_driver_change_ticks", worst_driver_ticks),
        (
            "driver_glitch_transitions",
            driver_glitch_transitions as u64,
        ),
        (
            "driver_single_input_glitches",
            driver_single_input_glitches as u64,
        ),
        ("worst_driver_pulse_ticks", worst_driver_pulse_ticks),
        ("total_driver_pulse_ticks", total_driver_pulse_ticks),
    ] {
        assert!(
            measured <= driver_baseline[name].as_u64().unwrap(),
            "{top}/{frontend}: {name} regression: {measured}"
        );
    }
    // Fixed pre-experiment timing baselines; material counts are diagnostic only.
    let (tick_limit, total_tick_limit) = match (top, frontend) {
        ("and4", "REDA") => (28, 1681),
        ("and4", "Yosys-fresh") => (22, 1465),
        ("seven_segment", "REDA") => (46, 9086),
        ("seven_segment", "Yosys-fresh") => (64, 12301),
        _ => panic!("missing independent timing baseline: {top}/{frontend}"),
    };
    assert!(
        worst_ticks <= tick_limit,
        "{top}/{frontend}: worst settle regression: {worst_ticks} > {tick_limit}"
    );
    assert!(
        sum_ticks <= total_tick_limit,
        "{top}/{frontend}: total settle regression: {sum_ticks} > {total_tick_limit}"
    );
    let legacy_baseline = if top == "seven_segment" {
        let original = compile_legacy(&lowered).expect("historical channel baseline compiles");
        let baseline = physical_metrics(&original.world, lowered.gates.len() as u64);
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
        "worst_driver_change_ticks":worst_driver_ticks,"driver_glitch_transitions":driver_glitch_transitions,
        "driver_single_input_glitches":driver_single_input_glitches,
        "worst_driver_pulse_ticks":worst_driver_pulse_ticks,"total_driver_pulse_ticks":total_driver_pulse_ticks,
        "driver_traces":driver_traces,
        "worst_output_change_ticks": worst_output_ticks,
        "glitch_transitions": glitch_transitions, "single_input_glitch_transitions": single_input_glitches, "output_traces": output_traces,
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
