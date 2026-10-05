use reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use reda::frontend::{
    compile_systemverilog, compile_systemverilog_physical, evaluate::Evaluator, CompileOptions,
    ResolvedPinout, SourceInput,
};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn ordinary_api_and_cli_bind_each_declared_output_occurrence() {
    for width in [0usize, 1, 2, 3, 4, 5, 6, 7] {
        let top = format!("occurrence{width}");
        let input = if width == 0 {
            String::new()
        } else {
            format!("input [{}:0] a,", width - 1)
        };
        let primary_source = if width == 0 {
            "1'b0".to_string()
        } else {
            "a[0]".to_string()
        };
        let inverse_source = if width == 0 {
            "~1'b0".to_string()
        } else {
            format!("~a[{}]", width - 1)
        };
        let source = format!("module {top}({input}output low,high,primary,inverse,alias_primary,alias_low,alias_high,alias_inverse); assign low=1'b0; assign high=1'b1; assign primary={primary_source}; assign inverse={inverse_source}; assign alias_primary={primary_source}; assign alias_low=1'b0; assign alias_high=1'b1; assign alias_inverse={inverse_source}; endmodule");
        let sources = [SourceInput {
            name: "occurrences.sv",
            text: &source,
        }];
        let options = CompileOptions::new(&top);
        let artifact = compile_systemverilog(&sources, &options).unwrap();
        let circuit = compile_systemverilog_physical(&sources, &options).unwrap();
        circuit
            .physical
            .validate_rewrite_view(&artifact.netlist)
            .unwrap();
        let rewrite = circuit.physical.physical_rewrite();
        if let Some(rewrite) = rewrite {
            assert_eq!(rewrite.source(), &artifact.netlist);
        }
        assert_eq!(artifact.ordered_output_ports.len(), 8);
        assert_eq!(circuit.pinout.outputs.len(), 8);
        assert_eq!(circuit.pinout.inputs.len(), width);
        assert!(circuit.pinout.inouts.is_empty());
        assert_eq!(circuit.physical.output_occurrences().len(), 8);
        if let Some(rewrite) = rewrite {
            assert_eq!(rewrite.source_output_positions().len(), 8);
        }
        let unique: BTreeSet<_> = circuit.pinout.outputs.values().collect();
        assert_eq!(
            unique.len(),
            8,
            "each declared occurrence needs its own terminal"
        );
        for (ordinal, port) in artifact.ordered_output_ports.iter().enumerate() {
            let (signal, position) = &circuit.physical.output_occurrences()[ordinal];
            assert_eq!(signal, &port.signal);
            if let Some(rewrite) = rewrite {
                assert_eq!(*position, rewrite.source_output_positions()[ordinal]);
            }
            assert_eq!(
                circuit.pinout.outputs[&port.name],
                [position.0, position.1, position.2]
            );
        }
        let mut simulator = Simulator::new(circuit.physical.world.clone());
        let evaluator = Evaluator::new(circuit.implementation_netlist()).unwrap();
        for address in 0..1usize << width {
            let mut source_values = BTreeMap::new();
            for bit in 0..width {
                let name = if width == 1 {
                    "a".to_string()
                } else {
                    format!("a[{bit}]")
                };
                let value = address & (1 << bit) != 0;
                let position = circuit.pinout.inputs[&name];
                let mut lever = simulator
                    .world()
                    .get(position[0], position[1], position[2])
                    .clone();
                assert_eq!(lever.kind, BlockKind::Lever);
                lever.lit = value;
                simulator
                    .world_mut()
                    .set(position[0], position[1], position[2], lever);
                source_values.insert(artifact.port(&name).unwrap().signal.clone(), value);
            }
            simulator.run_until_stable(2000).unwrap();
            let primary = address & 1 != 0;
            let inverse = width == 0 || address & (1 << (width - 1)) == 0;
            for (name, expected) in [
                ("low", false),
                ("high", true),
                ("primary", primary),
                ("inverse", inverse),
                ("alias_primary", primary),
                ("alias_low", false),
                ("alias_high", true),
                ("alias_inverse", inverse),
            ] {
                let position = circuit.pinout.outputs[name];
                let lamp = simulator.world().get(position[0], position[1], position[2]);
                assert_eq!(lamp.kind, BlockKind::Lamp);
                assert_eq!(
                    lamp.lit, expected,
                    "width={width}, address={address}, output={name}"
                );
            }
            let inputs = if let Some(rewrite) = rewrite {
                rewrite
                    .source()
                    .inputs
                    .iter()
                    .zip(
                        rewrite
                            .rewrite_certificate()
                            .rewritten_inputs_in_source_order(),
                    )
                    .map(|(source, implemented)| (implemented.clone(), source_values[source]))
                    .collect()
            } else {
                source_values
            };
            let values = evaluator.settle(&inputs).unwrap();
            for gate in &circuit.implementation_netlist().gates {
                let position = circuit.physical.gate_output_positions[&gate.output];
                let cell = simulator.world().get(position.0, position.1, position.2);
                let actual = if cell.kind == BlockKind::RedstoneWire {
                    cell.power > 0
                } else {
                    cell.lit
                };
                assert_eq!(
                    actual, values[&gate.output],
                    "width={width}, address={address}, actual gate={}",
                    gate.output
                );
            }
        }
        assert_exports(&top, &source, &circuit);
    }
}

fn assert_exports(top: &str, source: &str, circuit: &reda::frontend::CompiledNativeCircuit) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = root
        .join("target/qualification/native-output-occurrence-pinout")
        .join(&top);
    std::fs::create_dir_all(&directory).unwrap();
    let fixture = directory.join("source.sv");
    std::fs::write(&fixture, &source).unwrap();
    let bundle = directory.join("api.litematic");
    circuit.save_bundle(&bundle).unwrap();
    let (loaded, pinout) = ResolvedPinout::load_bundle(&bundle, &circuit).unwrap();
    assert_eq!(pinout, circuit.pinout);
    assert_eq!(
        canonical_world_fingerprint(&loaded),
        canonical_world_fingerprint(&circuit.physical.world)
    );
    let fingerprint = canonical_world_fingerprint(&circuit.physical.world);
    for mode in ["native", "auto"] {
        let destination = directory.join(mode);
        std::fs::create_dir_all(&destination).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_build_circuit"))
            .args(["--frontend", mode, "verilog"])
            .arg(&fixture)
            .arg(&top)
            .current_dir(&destination)
            .env("PATH", "")
            .env("REDA_PYTHON", "/nonexistent/reda-python")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "top={top}, mode={mode}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = destination.join("output").join(format!("{top}.litematic"));
        let world = reda::formats::litematic::load(&path).unwrap();
        assert_eq!(canonical_world_fingerprint(&world), fingerprint);
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("pinout.json")).unwrap())
                .unwrap();
        let inputs: BTreeMap<String, [i32; 3]> =
            serde_json::from_value(json["inputs"].clone()).unwrap();
        let outputs: BTreeMap<String, [i32; 3]> =
            serde_json::from_value(json["outputs"].clone()).unwrap();
        assert_eq!(inputs, circuit.pinout.inputs);
        assert_eq!(outputs.len(), circuit.pinout.outputs.len());
        for (name, position) in &circuit.pinout.outputs {
            let key = json["output_labels"][name]
                .as_str()
                .expect("source label survives CLI export");
            assert_eq!(
                outputs[key], *position,
                "top={top}, mode={mode}, output={name}"
            );
        }
    }
}

#[test]
fn generic_boundary_keeps_nonliteral_aliases_and_large_constant_fanout_distinct() {
    for (top, source, xor) in [
        ("large_constants", "module large_constants(output [16:0] y); assign y=17'b0; endmodule", false),
        ("xor_alias", "module xor_alias(input a,b,output y,alias_y); assign y=a^b; assign alias_y=y; endmodule", true),
    ] {
        let sources = [SourceInput { name: "generic.sv", text: source }];
        let options = CompileOptions::new(top);
        let artifact = compile_systemverilog(&sources, &options).unwrap();
        let circuit = compile_systemverilog_physical(&sources, &options).unwrap();
        circuit.physical.validate_rewrite_view(&artifact.netlist).unwrap();
        let rewrite = circuit.physical.physical_rewrite().unwrap();
        assert_eq!(rewrite.source(), &artifact.netlist);
        assert_eq!(circuit.pinout.outputs.len(), if xor { 2 } else { 17 });
        assert_eq!(circuit.pinout.outputs.values().collect::<BTreeSet<_>>().len(), circuit.pinout.outputs.len());
        for (ordinal, port) in artifact.ordered_output_ports.iter().enumerate() {
            let (signal, position) = &circuit.physical.output_occurrences()[ordinal];
            assert_eq!(signal, &port.signal);
            assert_eq!(circuit.pinout.outputs[&port.name], [position.0, position.1, position.2]);
        }
        let mut simulator = Simulator::new(circuit.physical.world.clone());
        let evaluator = Evaluator::new(circuit.implementation_netlist()).unwrap();
        for state in 0..if xor { 4 } else { 1 } {
            let mut source_values = BTreeMap::new();
            for (bit, name) in ["a", "b"].into_iter().enumerate().filter(|_| xor) {
                let value = state & (1 << bit) != 0;
                let [x,y,z] = circuit.pinout.inputs[name];
                let mut lever = simulator.world().get(x,y,z).clone();
                assert_eq!(lever.kind, BlockKind::Lever);
                lever.lit = value;
                simulator.world_mut().set(x,y,z,lever);
                source_values.insert(artifact.port(name).unwrap().signal.clone(),value);
            }
            simulator.run_until_stable(2000).unwrap();
            for position in circuit.pinout.outputs.values() {
                let lamp = simulator.world().get(position[0],position[1],position[2]);
                assert_eq!(lamp.kind,BlockKind::Lamp);
                assert_eq!(lamp.lit, xor && ((state & 1 != 0) ^ (state & 2 != 0)));
            }
            let inputs = rewrite.source().inputs.iter().zip(rewrite.rewrite_certificate().rewritten_inputs_in_source_order())
                .map(|(source,implemented)| (implemented.clone(),source_values[source])).collect();
            let values = evaluator.settle(&inputs).unwrap();
            for gate in &circuit.implementation_netlist().gates {
                let (x,y,z) = circuit.physical.gate_output_positions[&gate.output];
                let cell = simulator.world().get(x,y,z);
                let value = if cell.kind == BlockKind::RedstoneWire { cell.power > 0 } else { cell.lit };
                assert_eq!(value,values[&gate.output],"{top}: state={state}, gate={}",gate.output);
            }
        }
        assert_exports(top, source, &circuit);
    }
}
