//! Independent physical truth tables for additional circuit shapes.
use reda::compile::{compile, lowering::lower_optimised, metrics::physical_metrics};
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;
use serde_json::json;

fn signal_quality(traces: &[serde_json::Value], label: &str) -> serde_json::Value {
    let mut glitches = 0u64;
    let mut single = 0u64;
    let mut worst = 0u64;
    let mut total = 0u64;
    let mut longest = 0u64;
    for trace in traces {
        let Some(changes) = trace["changes"][label].as_array() else {
            continue;
        };
        let glitched = changes.len() > 1;
        glitches += u64::from(glitched);
        single += u64::from(
            glitched
                && (trace["from"].as_u64().unwrap() ^ trace["to"].as_u64().unwrap()).count_ones()
                    == 1,
        );
        worst = worst.max(changes.last().unwrap()[0].as_u64().unwrap());
        for interval in changes.windows(2) {
            if interval[0][1] != trace["expected"] {
                let width = interval[1][0].as_u64().unwrap() - interval[0][0].as_u64().unwrap();
                total += width;
                longest = longest.max(width);
            }
        }
    }
    json!({"glitches":glitches,"single_input_glitches":single,"worst_change_ticks":worst,
        "total_pulse_ticks":total,"worst_pulse_ticks":longest})
}

#[test]
fn additional_circuit_shapes_match_independent_truth_tables() {
    let mut rows = Vec::new();
    for (top, source, names) in [
        ("xor2", "module xor2(input logic a,b, output logic y); assign y=a^b; endmodule", vec!["a","b"]),
        ("mux2", "module mux2(input logic a,b,s, output logic y); assign y=s?b:a; endmodule", vec!["a","b","s"]),
        ("majority3", "module majority3(input logic a,b,c, output logic y); assign y=(a&b)|(a&c)|(b&c); endmodule", vec!["a","b","c"]),
    ] {
        let artifact = compile_systemverilog(&[SourceInput {name: "matrix.sv", text: source}], &CompileOptions::new(top)).unwrap();
        let outputs = artifact.output_map();
        let oracle = synthesize_verilog(source, top).unwrap();
        for (frontend, netlist, outputs) in [("native", artifact.netlist, outputs), ("yosys", oracle.0, oracle.1)] {
            let lowered = lower_optimised(&netlist).unwrap();
            let compiled = compile(&lowered).unwrap();
            let metrics = physical_metrics(&compiled.world, lowered.gates.len() as u64);
            let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world);
            let repeaters = compiled.world.positions_of(BlockKind::Repeater).count();
            let inputs: Vec<_> = names.iter().map(|name| compiled.input_positions[*name]).collect();
            let output = compiled.output_positions[&outputs["y"]];
            let driver = compiled.gate_output_positions[&outputs["y"]];
            let planner = format!("{:?}", compiled.planner_kind());
            let mut simulator = Simulator::new(compiled.world);
            simulator.run_until_stable(2000).unwrap();
            simulator.attach_observer([
                (reda::redstone::simulator::position::Position::new(output.0,output.1,output.2),"lamp".to_string()),
                (reda::redstone::simulator::position::Position::new(driver.0,driver.1,driver.2),"driver".to_string()),
            ]);
            let mut traces = Vec::new();
            let states = 1usize << names.len();
            let mut worst = 0;
            let mut sum = 0;
            for from in 0..states {
                for to in 0..states {
                    let mut observed = None;
                    for (phase, value) in [from, to].into_iter().enumerate() {
                        if phase == 1 { simulator.reset_observer(); }
                        for (i, &(x,y,z)) in inputs.iter().enumerate() {
                            let mut block = simulator.world().get(x,y,z).clone();
                            block.lit = value & (1 << i) != 0;
                            simulator.world_mut().set(x,y,z,block);
                        }
                        let start = simulator.current_tick();
                        simulator.run_until_stable(2000).unwrap();
                        if phase == 1 {
                            let ticks = simulator.current_tick() - start;
                            worst = worst.max(ticks);
                            sum += ticks;
                            observed = Some(reda::timing::observations_to_result(simulator.observations(),start,ticks));
                        }
                    }
                    let a = to & 1 != 0;
                    let b = to & 2 != 0;
                    let third = to & 4 != 0;
                    let expected = match top {
                        "xor2" => a != b,
                        "mux2" => if third {b} else {a},
                        "majority3" => [a,b,third].into_iter().filter(|&v|v).count() >= 2,
                        _ => unreachable!(),
                    };
                    assert_eq!(simulator.world().get(output.0,output.1,output.2).lit, expected, "{top}/{frontend}: {from}->{to}");
                    let state = simulator.world().get(driver.0,driver.1,driver.2);
                    let actual = if state.kind == BlockKind::RedstoneWire {state.power > 0} else {state.lit};
                    assert_eq!(actual,expected,"{top}/{frontend}: driver {from}->{to}");
                    let timing = observed.unwrap();
                    let changes: std::collections::BTreeMap<_,_> = timing.nets.iter().map(|(name,net)| (name.clone(),json!(net.changes()))).collect();
                    traces.push(json!({"from":from,"to":to,"expected":expected,"changes":changes}));
                }
            }
            let quality = json!({"lamp":signal_quality(&traces,"lamp"),"driver":signal_quality(&traces,"driver")});
            let signal_baselines: serde_json::Value = serde_json::from_str(include_str!(
                "../docs/generator-matrix-signal-baseline-2026-09-30.json"
            )).unwrap();
            let signal_baseline = signal_baselines["measurements"].as_array().unwrap().iter()
                .find(|row| row["case"] == top && row["frontend"] == frontend).unwrap();
            let diagnostic_target = std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
            std::fs::write(diagnostic_target.join(format!("matrix_signals_{top}_{frontend}.json")),
                serde_json::to_string_pretty(&json!({"case":top,"frontend":frontend,
                    "world_fingerprint":fingerprint,"signal_quality":quality,"traces":traces})).unwrap()).unwrap();
            for label in ["lamp","driver"] {
                for (metric,value) in quality[label].as_object().unwrap() {
                    let limit = signal_baseline["signal_quality"][label][metric].as_u64().unwrap();
                    assert!(value.as_u64().unwrap() <= limit,
                        "{top}/{frontend}: {label} {metric} regression: {value} > {limit}");
                }
            }
            let baselines: serde_json::Value = serde_json::from_str(
                include_str!("../docs/generator-quality-matrix-2026-09-30.json")
            ).unwrap();
            let baseline = baselines["measurements"].as_array().unwrap().iter().find(|row|
                row["case"] == top && row["frontend"] == frontend
            ).expect("fixed timing baseline exists");
            assert!(worst <= baseline["worst_settle_ticks"].as_u64().unwrap(),
                "{top}/{frontend}: worst settle regression: {worst}");
            let mean = sum as f64 / (states * states) as f64;
            assert!(mean <= baseline["mean_settle_ticks"].as_f64().unwrap(),
                "{top}/{frontend}: mean settle regression: {mean}");
            rows.push(json!({"case":top,"source":source,"frontend":frontend,"lowered_gates":lowered.gates.len(),"physical":metrics,"repeaters":repeaters,"planner":planner,"world_fingerprint":fingerprint,"signal_quality":quality,"traces":traces,"transitions":states*states,"worst_settle_ticks":worst,"mean_settle_ticks":sum as f64/(states*states) as f64}));
        }
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
    std::fs::write(
        target.join("generator_quality_matrix.json"),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}
