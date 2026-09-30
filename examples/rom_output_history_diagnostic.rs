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
    let plan = reda::compile::planner::plan_from_netlist(
        &source,
        &reda::compile::planner::PortPlacements::default(),
    )
    .unwrap();
    let realised =
        reda::compile::planner::emit_candidate(&plan, &source, circuit.world.size()).unwrap();
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&realised.world),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&circuit.world),
        "the diagnostic route ledger must describe exactly the world stock compile shipped"
    );
    for route in plan.routes() {
        for (terminal, path) in route.terminals().iter().zip(route.branch_paths()) {
            println!(
                "terminal owner={:?},sink={:?},kind={:?},tail={:?}",
                route.owner(),
                terminal.sink,
                terminal.kind,
                &path[path.len().saturating_sub(3)..]
            );
        }
    }
    for route in plan.routes() {
        for (at, state) in route.anchors().iter().zip(route.realisation()) {
            if at.y == 1 && (40..=43).contains(&at.x) && (41..=43).contains(&at.z) {
                println!(
                    "route={},owner={:?},at={at:?},state={state:?}",
                    route.id(),
                    route.owner()
                );
            }
        }
    }
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
                        for route in plan
                            .routes()
                            .iter()
                            .filter(|r| r.owner() == Some(name.as_str()))
                        {
                            let mut energized: Vec<_> = route
                                .anchors()
                                .iter()
                                .filter_map(|a| {
                                    let b = sim.world().get(a.x, a.y, a.z);
                                    (b.kind == BlockKind::RedstoneWire && b.power > 0)
                                        .then_some((b.power, *a))
                                })
                                .collect();
                            energized.sort_by_key(|&(power, at)| (std::cmp::Reverse(power), at));
                            println!(
                                "highest affected route cells: {:?}",
                                &energized[..energized.len().min(5)]
                            );
                            if let Some((_, at)) = energized.first() {
                                for dy in -1..=1 {
                                    for dz in -1..=1 {
                                        for dx in -2..=1 {
                                            let b =
                                                sim.world().get(at.x + dx, at.y + dy, at.z + dz);
                                            if b.kind != BlockKind::Air {
                                                println!(
                                                    "source neighborhood ({},{},{}) {b:?}",
                                                    at.x + dx,
                                                    at.y + dy,
                                                    at.z + dz
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        for dy in 0..=2 {
                            for dz in -1..=1 {
                                for dx in -2..=1 {
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
