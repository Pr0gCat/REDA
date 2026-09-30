//! Manual route/power diagnostic: four comma-separated ROM words, or `xor2`.
use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;
use std::collections::HashMap;
fn main() {
    let argument = std::env::args().nth(1);
    let xor = argument.as_deref() == Some("xor2");
    let words: [u8; 4] = match argument {
        Some(_) if xor => [0, 1, 1, 0],
        Some(arg) => {
            let values: Vec<u8> = arg
                .split(',')
                .map(|n| n.parse().expect("four 3-bit words"))
                .collect();
            let words: [u8; 4] = values.try_into().expect("four words required");
            assert!(words.iter().all(|w| *w < 8));
            words
        }
        None => [1, 3, 2, 0],
    };
    let mut text = String::from("module source_rom(input logic a,b,output logic y0,y1,y2);");
    for bit in 0..3 {
        let v = words.map(|w| (w >> bit) & 1);
        text.push_str(&format!(
            "assign y{bit}=b ? (a ? 1'b{} : 1'b{}) : (a ? 1'b{} : 1'b{});",
            v[3], v[2], v[1], v[0]
        ));
    }
    text.push_str("endmodule");
    let module = if xor { "xor2" } else { "source_rom" };
    if xor {
        text = "module xor2(input logic a,b, output logic y); assign y=a^b; endmodule".to_owned();
    }
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "diagnostic.sv",
            text: &text,
        }],
        &CompileOptions::new(module),
    )
    .unwrap();
    let ports = artifact.output_map();
    let mut source = lower_optimised(&artifact.netlist).unwrap();
    source.outputs = if xor {
        vec![ports["y"].clone()]
    } else {
        (0..3)
            .map(|bit| ports[&format!("y{bit}")].clone())
            .collect()
    };
    // Stock compile declares one physical observation per driven net.
    // Public aliases share that observation rather than duplicate declarations.
    let mut seen = std::collections::BTreeSet::new();
    source.outputs.retain(|name| seen.insert(name.clone()));
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
                    let actual = if state.kind == BlockKind::RedstoneWire {
                        state.power > 0
                    } else {
                        state.lit
                    };
                    if actual != expected {
                        println!("incorrect gate {} from={from},to={to},address={address},at=({x},{y},{z}),expected={expected},state={state:?}", gate.output);
                        for dy in -1..=1 {
                            for dz in -2..=2 {
                                for dx in -2..=2 {
                                    let cell = sim.world().get(x + dx, y + dy, z + dz);
                                    if cell.kind != BlockKind::Air {
                                        println!(
                                            "gate neighborhood ({},{},{}) {cell:?}",
                                            x + dx,
                                            y + dy,
                                            z + dz
                                        );
                                    }
                                }
                            }
                        }
                    }
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
                        panic!("output history contains an incorrect lamp");
                    }
                }
            }
        }
    }
    println!("all 32 setup/target history phases have correct gate drivers and output lamps");
}
