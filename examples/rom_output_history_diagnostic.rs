//! Manual diagnostic, not an acceptance test. Stock output lamps misread this history.
use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;
use std::collections::HashMap;
fn main() {
    let text = "module source_rom(input logic a,b,output logic y0,y1,y2); assign y0=b ? (a ? 1'b0 : 1'b0) : (a ? 1'b1 : 1'b1); assign y1=b ? (a ? 1'b0 : 1'b1) : (a ? 1'b1 : 1'b0); assign y2=b ? (a ? 1'b0 : 1'b0) : (a ? 1'b0 : 1'b0); endmodule";
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "diagnostic.sv",
            text,
        }],
        &CompileOptions::new("source_rom"),
    )
    .unwrap();
    let ports = artifact.output_map();
    let mut source = lower_optimised(&artifact.netlist).unwrap();
    source.outputs = (0..3)
        .map(|bit| ports[&format!("y{bit}")].clone())
        .collect();
    let circuit = compile(&source).unwrap();
    println!("planner={:?},netlist={source:?}", circuit.planner_kind());
    let mut sim = Simulator::new(circuit.world.clone());
    sim.run_until_stable(2000).unwrap();
    let order = source.combinational_order().unwrap();
    for from in 0..4usize {
        for to in 0..4usize {
            for address in [from, to] {
                for (bit, name) in source.inputs.iter().enumerate() {
                    let (x, y, z) = circuit.input_positions[name];
                    let mut state = sim.world().get(x, y, z).clone();
                    state.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(x, y, z, state);
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                let mut values: HashMap<_, _> = source
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| (name.clone(), address & (1 << bit) != 0))
                    .collect();
                for &index in &order {
                    let gate = &source.gates[index];
                    let operands: Vec<_> = gate.inputs.iter().map(|n| values[n]).collect();
                    let expected = gate.kind.evaluate(&operands);
                    values.insert(gate.output.clone(), expected);
                    let (x, y, z) = circuit.gate_output_positions[&gate.output];
                    let state = sim.world().get(x, y, z);
                    assert_eq!(
                        if state.kind == BlockKind::RedstoneWire {
                            state.power > 0
                        } else {
                            state.lit
                        },
                        expected,
                        "internal {} address={address}",
                        gate.output
                    );
                }
                for name in &source.outputs {
                    let (x, y, z) = circuit.output_positions[name];
                    let state = sim.world().get(x, y, z);
                    if state.lit != values[name] {
                        println!("REPRODUCED: from={from},to={to},address={address},ticks={ticks},output={name},expected={},at=({x},{y},{z}),state={state:?}",values[name]);
                        for dy in 0..=2 {
                            for dz in -1..=1 {
                                for dx in -1..=1 {
                                    let state = sim.world().get(x + dx, y + dy, z + dz);
                                    if state.kind != BlockKind::Air {
                                        println!(
                                            "neighbor ({},{},{}) {state:?}",
                                            x + dx,
                                            y + dy,
                                            z + dz
                                        );
                                    }
                                }
                            }
                        }
                        return;
                    }
                }
            }
        }
    }
    panic!("recorded failure did not reproduce; verify the fix against the full history");
}
