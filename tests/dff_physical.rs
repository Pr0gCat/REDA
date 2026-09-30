//! End-to-end sequential contract, without priming away power-on defects.
use reda::compile::{compile, lowering::lower_optimised, Netlist};
use reda::formats::litematic::{load, save};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use std::collections::{BTreeMap, HashMap};

fn check_trace(netlist: Netlist, outputs: HashMap<String, String>, name: &str) {
    let mut logical = Evaluator::new(&netlist).unwrap();
    let physical = compile(&lower_optimised(&netlist).unwrap()).unwrap();
    let positions = physical.input_positions;
    let q = physical.output_positions[&outputs["q"]];
    // Test the artifact users load, including powered block states, rather
    // than applying any simulator-only initialization or reset pulse.
    let path =
        std::env::temp_dir().join(format!("reda-dff-{}-{name}.litematic", std::process::id()));
    save(&path, &physical.world, name).unwrap();
    let world = load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let mut sim = Simulator::new(world);
    sim.run_until_stable(2000).unwrap();
    let mut inputs: BTreeMap<String, bool> =
        netlist.inputs.iter().map(|n| (n.clone(), false)).collect();
    assert_eq!(
        sim.world().get(q.0, q.1, q.2).lit,
        logical.evaluate(&inputs).unwrap()[&outputs["q"]],
        "{name}: power-on before first edge"
    );

    // Establish setup/hold by settling data and enable before each rising
    // edge; also vary both while high to check that Q does not feed through.
    for previous in [false, true] {
        for data in [false, true] {
            for enabled in [false, true] {
                for (d, clk, en) in [
                    (previous, false, true),
                    (previous, true, true), // establish prior Q
                    (previous, false, true),
                    (data, false, enabled),
                    (data, true, enabled),
                    (!data, true, !enabled),
                    (!data, false, !enabled),
                    (!data, true, !enabled),
                ] {
                    for (port, value) in [("d", d), ("clk", clk), ("en", en)] {
                        if let Some(at) = positions.get(port) {
                            inputs.insert(port.into(), value);
                            let mut state = sim.world().get(at.0, at.1, at.2).clone();
                            state.lit = value;
                            sim.world_mut().set(at.0, at.1, at.2, state);
                        }
                    }
                    sim.run_until_stable(2000).unwrap();
                    // Design H uses a clock-inverting torch. Leave a real
                    // dwell between changes: fast repeated edges burn it out.
                    for _ in 0..16 {
                        sim.step();
                    }
                    assert_eq!(
                        sim.world().get(q.0, q.1, q.2).lit,
                        logical.step(&inputs).unwrap()[&outputs["q"]],
                        "{name}: previous={previous}, d={d}, clk={clk}, en={en}"
                    );
                }
            }
        }
    }
}

#[test]
fn native_and_yosys_dffs_match_physical_state_from_power_on() {
    for (top, text) in [
        ("dff", include_str!("fixtures/dff.sv")),
        ("dff_enable", include_str!("fixtures/dff_enable.sv")),
    ] {
        let artifact = compile_systemverilog(
            &[SourceInput { name: top, text }],
            &CompileOptions::new(top),
        )
        .unwrap();
        let outputs = artifact.output_map();
        check_trace(artifact.netlist, outputs, &format!("native-{top}"));
        let (netlist, outputs) = synthesize_verilog(text, top).unwrap();
        check_trace(netlist, outputs, &format!("yosys-{top}"));
    }
}
