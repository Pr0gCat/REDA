use reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use reda::frontend::{
    compile_systemverilog, compile_systemverilog_physical, evaluate::Evaluator, CompileOptions,
    PortDirection, SourceInput,
};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};
use std::collections::BTreeMap;

const WORLD: &str = "f3af4444f9776335f3c51a768a8a7ae3d5e387e66974807feb5009919b459b5a";
const SOURCE: &str = include_str!("fixtures/rom64_matrix.sv");

fn literal_words() -> Vec<u16> {
    let rows: BTreeMap<usize, u16> = SOURCE
        .lines()
        .filter_map(|line| {
            let line = line.strip_prefix("6'd")?;
            let (address, value) = line.split_once(": q = 16'h")?;
            Some((
                address.parse().unwrap(),
                u16::from_str_radix(value.trim_end_matches(';'), 16).unwrap(),
            ))
        })
        .collect();
    assert_eq!(rows.len(), 64);
    (0..64).map(|address| rows[&address]).collect()
}

#[test]
fn ordinary_native_rom64_api_and_cli_retain_qualified_world_and_actual_observations() {
    let source = [SourceInput {
        name: "rom64_matrix.sv",
        text: SOURCE,
    }];
    let options = CompileOptions::new("rom64");
    eprintln!("ROM64_STAGE artifact_compile start");
    let artifact = compile_systemverilog(&source, &options).unwrap();
    eprintln!(
        "ROM64_STAGE artifact_compile complete source_gates={}",
        artifact.netlist.gates.len()
    );
    eprintln!("ROM64_STAGE physical_compile start");
    let circuit = compile_systemverilog_physical(&source, &options).unwrap();
    eprintln!("ROM64_STAGE physical_compile complete");
    let physical = &circuit.physical;
    eprintln!("ROM64_STAGE certificate_view start");
    physical.validate_rewrite_view(&artifact.netlist).unwrap();
    let rewrite = physical
        .physical_rewrite()
        .expect("ordinary default selects certified ROM64 matrix");
    assert_eq!(rewrite.source(), &artifact.netlist);
    assert_eq!(canonical_world_fingerprint(&physical.world).as_str(), WORLD);
    assert_eq!(circuit.implementation_netlist(), rewrite.implemented());
    assert_eq!(rewrite.implemented().gates.len(), 2208);
    assert_eq!(
        physical.gate_output_positions.len(),
        rewrite.implemented().gates.len()
    );
    assert!(circuit.pinout.inouts.is_empty());
    assert_eq!(circuit.pinout.inputs.len(), 6);
    assert_eq!(circuit.pinout.outputs.len(), 16);
    for (signal, position) in artifact
        .netlist
        .inputs
        .iter()
        .zip(rewrite.source_input_positions())
    {
        let port = artifact
            .ports
            .iter()
            .find(|p| p.direction == PortDirection::Input && &p.signal == signal)
            .unwrap();
        assert_eq!(
            circuit.pinout.inputs[&port.name],
            [position.0, position.1, position.2]
        );
        assert_eq!(physical.input_positions[signal], *position);
    }
    for (port, position) in artifact
        .ordered_output_ports
        .iter()
        .zip(rewrite.source_output_positions())
    {
        assert_eq!(
            circuit.pinout.outputs[&port.name],
            [position.0, position.1, position.2]
        );
    }
    eprintln!("ROM64_STAGE certificate_view complete");
    eprintln!("ROM64_STAGE timing_watches start");
    let watches = reda::timing::watch_all_nets(physical);
    let unique: std::collections::BTreeSet<_> = watches.iter().map(|(_, name)| name).collect();
    assert_eq!(watches.len(), unique.len());
    eprintln!(
        "ROM64_STAGE timing_watches complete watches={}",
        watches.len()
    );
    eprintln!("ROM64_STAGE simulation start");
    let mut simulator = Simulator::new(physical.world.clone());
    let evaluator = Evaluator::new(rewrite.implemented()).unwrap();
    for (address, word) in literal_words().into_iter().enumerate() {
        eprintln!("ROM64_STAGE simulation address={address} start");
        let mut source_inputs = BTreeMap::new();
        for bit in 0..6 {
            let name = format!("address[{bit}]");
            let value = address & (1 << bit) != 0;
            let p = circuit.pinout.inputs[&name];
            let mut lever = simulator.world().get(p[0], p[1], p[2]).clone();
            assert_eq!(lever.kind, BlockKind::Lever);
            lever.lit = value;
            simulator.world_mut().set(p[0], p[1], p[2], lever);
            source_inputs.insert(artifact.port(&name).unwrap().signal.clone(), value);
        }
        simulator.run_until_stable(2000).unwrap();
        for bit in 0..16 {
            let p = circuit.pinout.outputs[&format!("q[{bit}]")];
            let lamp = simulator.world().get(p[0], p[1], p[2]);
            assert_eq!(lamp.kind, BlockKind::Lamp);
            assert_eq!(
                lamp.lit,
                word & (1 << bit) != 0,
                "address={address}, output={bit}"
            );
        }
        let inputs = rewrite
            .source()
            .inputs
            .iter()
            .zip(
                rewrite
                    .rewrite_certificate()
                    .rewritten_inputs_in_source_order(),
            )
            .map(|(source, implemented)| (implemented.clone(), source_inputs[source]))
            .collect();
        let values = evaluator.settle(&inputs).unwrap();
        for gate in &rewrite.implemented().gates {
            let p = physical.gate_output_positions[&gate.output];
            let cell = simulator.world().get(p.0, p.1, p.2);
            let value = if cell.kind == BlockKind::RedstoneWire {
                cell.power > 0
            } else {
                cell.lit
            };
            assert_eq!(
                value, values[&gate.output],
                "address={address}, actual gate={}",
                gate.output
            );
        }
    }
    eprintln!("ROM64_STAGE simulation complete addresses=64");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = root.join("tests/fixtures/rom64_matrix.sv");
    for mode in ["native", "auto"] {
        eprintln!("ROM64_STAGE cli mode={mode} start");
        let directory = root
            .join("target/qualification/native-rom64-default-cli")
            .join(mode);
        std::fs::create_dir_all(&directory).unwrap();
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_build_circuit"))
            .args(["--frontend", mode, "verilog"])
            .arg(&fixture)
            .arg("rom64")
            .current_dir(&directory)
            .env("PATH", "")
            .env("REDA_PYTHON", "/nonexistent/reda-python")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "mode={mode}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        eprintln!("ROM64_STAGE cli mode={mode} process_complete");
        let path = directory.join("output/rom64.litematic");
        let loaded = reda::formats::litematic::load(&path).unwrap();
        assert_eq!(canonical_world_fingerprint(&loaded).as_str(), WORLD);
        let pinout: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("pinout.json")).unwrap())
                .unwrap();
        let inputs: BTreeMap<String, [i32; 3]> =
            serde_json::from_value(pinout["inputs"].clone()).unwrap();
        let outputs: BTreeMap<String, [i32; 3]> =
            serde_json::from_value(pinout["outputs"].clone()).unwrap();
        assert_eq!(inputs, circuit.pinout.inputs);
        for (label, position) in &circuit.pinout.outputs {
            let key = pinout["output_labels"][label]
                .as_str()
                .expect("each source output has a CLI label binding");
            assert_eq!(outputs[key], *position);
        }
        assert_eq!(outputs.len(), 16);
        eprintln!("ROM64_STAGE cli mode={mode} reload_and_bindings complete");
    }
}

#[test]
fn six_input_default_preserves_constant_primary_and_repeated_output_occurrences() {
    use reda::compile::{selection::compile_selected_with_refresh_budget, Gate, Netlist};
    let source = Netlist {
        inputs: (0..6).map(|bit| format!("arbitrary_input_{bit}")).collect(),
        outputs: [
            "low",
            "high",
            "arbitrary_input_0",
            "inverse",
            "arbitrary_input_0",
            "low",
            "high",
            "inverse",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        gates: vec![
            Gate::nor("high", &[]),
            Gate::nor("low", &["high"]),
            Gate::nor("inverse", &["arbitrary_input_0"]),
        ],
    };
    for budget in [0, 256] {
        let compiled = compile_selected_with_refresh_budget(&source, budget).unwrap();
        compiled.validate_rewrite_view(&source).unwrap();
        let rewrite = compiled.physical_rewrite().unwrap();
        let evaluator = Evaluator::new(rewrite.implemented()).unwrap();
        let mut simulator = Simulator::new(compiled.world.clone());
        for address in 0..64 {
            let mut inputs = BTreeMap::new();
            for (bit, (position, name)) in rewrite
                .source_input_positions()
                .iter()
                .zip(
                    rewrite
                        .rewrite_certificate()
                        .rewritten_inputs_in_source_order(),
                )
                .enumerate()
            {
                let value = address & (1 << bit) != 0;
                let mut lever = simulator
                    .world()
                    .get(position.0, position.1, position.2)
                    .clone();
                assert_eq!(lever.kind, BlockKind::Lever);
                lever.lit = value;
                simulator
                    .world_mut()
                    .set(position.0, position.1, position.2, lever);
                inputs.insert(name.clone(), value);
            }
            simulator.run_until_stable(2000).unwrap();
            let values = evaluator.settle(&inputs).unwrap();
            for gate in &rewrite.implemented().gates {
                let p = compiled.gate_output_positions[&gate.output];
                let cell = simulator.world().get(p.0, p.1, p.2);
                let actual = if cell.kind == BlockKind::RedstoneWire {
                    cell.power > 0
                } else {
                    cell.lit
                };
                assert_eq!(
                    actual, values[&gate.output],
                    "budget={budget}, address={address}, gate={}",
                    gate.output
                );
            }
            let bit = address & 1 != 0;
            let expected = [false, true, bit, !bit, bit, false, true, !bit];
            for (position, value) in rewrite.source_output_positions().iter().zip(expected) {
                let lamp = simulator.world().get(position.0, position.1, position.2);
                assert_eq!(lamp.kind, BlockKind::Lamp);
                assert_eq!(lamp.lit, value, "budget={budget}, address={address}");
            }
        }
    }
}
