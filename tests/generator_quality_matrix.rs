//! Independent physical truth tables for additional circuit shapes.
use reda::compile::{compile, lowering::lower_optimised, metrics::physical_metrics};
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;
use serde_json::json;

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
            let planner = format!("{:?}", compiled.planner_kind());
            let mut simulator = Simulator::new(compiled.world);
            simulator.run_until_stable(2000).unwrap();
            let states = 1usize << names.len();
            let mut worst = 0;
            let mut sum = 0;
            for from in 0..states {
                for to in 0..states {
                    for (phase, value) in [from, to].into_iter().enumerate() {
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
            rows.push(json!({"case":top,"source":source,"frontend":frontend,"lowered_gates":lowered.gates.len(),"physical":metrics,"repeaters":repeaters,"planner":planner,"world_fingerprint":fingerprint,"transitions":states*states,"worst_settle_ticks":worst,"mean_settle_ticks":sum as f64/(states*states) as f64}));
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
