use reda::frontend::{compile_systemverilog_physical, CompileOptions, SourceInput, PortDirection, ResolvedPinout};
#[test]
fn native_physical_entry_retains_shared_inout_and_exports_reloadable_bundle() {
    let source = SourceInput { name: "fixture.sv", text: include_str!("fixtures/resolved-inout.sv") };
    let circuit = compile_systemverilog_physical(&[source], &CompileOptions::new("top")).unwrap();
    assert_eq!(circuit.pinout.inouts.len(), 1);
    assert_eq!(circuit.pinout.inouts[0].source.direction, PortDirection::Inout);
    assert_eq!(circuit.pinout.inputs.len(), 4);
    assert_eq!(circuit.pinout.outputs.len(), 1);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qualification/native-physical-inout");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("native.litematic");
    circuit.save_bundle(&path).unwrap();
    let (_, pinout) = ResolvedPinout::load_bundle(&path, &circuit).unwrap();
    assert_eq!(pinout, circuit.pinout);
}
#[test]
fn native_physical_entry_preserves_boolean_source_ports() {
    let source = SourceInput { name: "ordinary.sv", text: "module top(input a,output y); assign y=~a; endmodule" };
    let circuit = compile_systemverilog_physical(&[source], &CompileOptions::new("top")).unwrap();
    assert!(circuit.pinout.inouts.is_empty());
    assert!(circuit.pinout.inputs.contains_key("a"));
    assert!(circuit.pinout.outputs.contains_key("y"));
}

#[test]
fn ordinary_native_physical_entry_exposes_actual_decoder_implementation() {
    use reda::frontend::{compile_systemverilog, evaluate::Evaluator};
    use reda::redstone::{simulator::Simulator, world::block::BlockKind};

    let sources = [SourceInput {
        name: "seven_segment.sv",
        text: include_str!("fixtures/seven_segment.sv"),
    }];
    let options = CompileOptions::new("seven_segment");
    let original = compile_systemverilog(&sources, &options).unwrap();
    let circuit = compile_systemverilog_physical(&sources, &options).unwrap();
    assert!(circuit.physical.physical_rewrite().is_some());
    assert_ne!(circuit.implementation_netlist(), &original.netlist);
    let evaluator = Evaluator::new(circuit.implementation_netlist()).unwrap();
    let mut simulator = Simulator::new(circuit.physical.world.clone());
    let glyphs = [0x3fu8, 0x06, 0x5b, 0x4f, 0x66, 0x6d, 0x7d, 0x07,
                  0x7f, 0x6f, 0, 0, 0, 0, 0, 0];
    for (state, glyph) in glyphs.into_iter().enumerate() {
        let mut inputs = std::collections::BTreeMap::new();
        for bit in 0..4 {
            let name = format!("d{bit}");
            let value = state & (1 << bit) != 0;
            let [x, y, z] = circuit.pinout.inputs[&name];
            let mut cell = simulator.world().get(x, y, z).clone();
            assert_eq!(cell.kind, BlockKind::Lever);
            cell.lit = value;
            simulator.world_mut().set(x, y, z, cell);
            inputs.insert(name, value);
        }
        simulator.run_until_stable(2000).unwrap();
        for (bit, name) in ["a", "b", "c", "d", "e", "f", "g"].into_iter().enumerate() {
            let [x, y, z] = circuit.pinout.outputs[name];
            assert_eq!(simulator.world().get(x, y, z).lit, glyph & (1 << bit) != 0,
                       "state={state}, output={name}");
        }
        let rewrite = circuit.physical.physical_rewrite().unwrap();
        let implemented_inputs = rewrite.source().inputs.iter()
            .zip(rewrite.rewrite_certificate().rewritten_inputs_in_source_order())
            .map(|(source, implemented)| (implemented.clone(), inputs[source]))
            .collect();
        let values = evaluator.settle(&implemented_inputs).unwrap();
        for gate in &circuit.implementation_netlist().gates {
            let &(x, y, z) = circuit.physical.gate_output_positions.get(&gate.output)
                .unwrap_or_else(|| panic!("implementation gate {} has no physical observation", gate.output));
            let cell = simulator.world().get(x, y, z);
            let actual = if cell.kind == BlockKind::RedstoneWire { cell.power > 0 } else { cell.lit };
            assert_eq!(actual, values[&gate.output], "state={state}, gate={}", gate.output);
        }
    }
}

#[test]
fn default_cli_exports_shared_bus_in_native_and_auto_without_yosys() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/resolved-inout.sv");
    for mode in ["native", "auto"] {
        let dir = root.join("target/qualification/native-inout-cli").join(mode);
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_build_circuit"))
            .args(["--frontend", mode, "verilog"])
            .arg(&fixture).arg("top").current_dir(&dir)
            .env("PATH", "").env("REDA_PYTHON", "/nonexistent/reda-python")
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let source = SourceInput { name: "fixture.sv", text: include_str!("fixtures/resolved-inout.sv") };
        let expected = compile_systemverilog_physical(&[source], &CompileOptions::new("top")).unwrap();
        let (_, loaded) = ResolvedPinout::load_bundle(&dir.join("output/top.litematic"), &expected).unwrap();
        assert_eq!(loaded.inouts.len(), 1);
    }
}
