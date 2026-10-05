use reda::compile::{fragment_synth::benchmark::canonical_world_fingerprint, geometry::rotate};
use reda::frontend::{
    compile_systemverilog_physical, evaluate::Evaluator, CompileOptions, ResolvedPinout,
    SourceInput,
};
use reda::redstone::{
    simulator::{component::repeater_is_locked, position::Position, Simulator},
    world::{block::BlockKind, storage::World},
};
use std::collections::{BTreeMap, BTreeSet};

fn signal(world: &World, at: (i32, i32, i32)) -> bool {
    let cell = world.get(at.0, at.1, at.2);
    if cell.kind == BlockKind::RedstoneWire {
        cell.power > 0
    } else {
        cell.lit
    }
}
fn check_counter(subtract: bool) {
    let (top, text) = if subtract {
        (
            "arithmetic_counter_subtract",
            include_str!("/home/seith/REDA/tests/fixtures/arithmetic_counter_subtract.sv"),
        )
    } else {
        (
            "arithmetic_counter_add",
            include_str!("/home/seith/REDA/tests/fixtures/arithmetic_counter_add.sv"),
        )
    };
    let directory = std::path::PathBuf::from(std::env::args().nth(1).expect("artifact directory"));
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.join("observations.json")).unwrap()).unwrap();
    assert_eq!(data["top"],top); assert_eq!(data["source"],text);
    let artifact = reda::frontend::compile_systemverilog(&[SourceInput{name:top,text}],&CompileOptions::new(top)).unwrap();
    let implementation = reda::compile::lowering::lower_optimised(&artifact.netlist).unwrap();
    let expected_graph = serde_json::json!({"inputs":implementation.inputs,"outputs":implementation.outputs,"gates":implementation.gates.iter().map(|gate|serde_json::json!({"name":gate.name,"output":gate.output,"inputs":gate.inputs,"kind":gate.kind.wire_name()})).collect::<Vec<_>>()});
    assert_eq!(data["implementation"],expected_graph);
    let loaded = reda::formats::litematic::load(&directory.join("counter.litematic")).unwrap();
    assert_eq!(serde_json::to_value(canonical_world_fingerprint(&loaded)).unwrap(),data["world_fingerprint"]);
    let pinout: ResolvedPinout = serde_json::from_slice(&std::fs::read(directory.join("counter.pinout.json")).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&pinout).unwrap(),data["pinout"]);
    let gate_positions: BTreeMap<String,(i32,i32,i32)> = serde_json::from_value(data["gate_output_positions"].clone()).unwrap();
    assert_eq!(gate_positions.len(),implementation.gates.len());
    let mut simulator = Simulator::new(loaded);
    let mut evaluator = Evaluator::new(&implementation).unwrap();
    let mut inputs: BTreeMap<_, _> = implementation
        .inputs
        .iter()
        .map(|n| (n.clone(), false))
        .collect();
    simulator.run_until_stable(2000).unwrap();
    for position in pinout.outputs.values() {
        assert!(
            !signal(simulator.world(), (position[0], position[1], position[2])),
            "{top}: power-on Q"
        );
    }
    let mut watched = Vec::new();
    let mut hidden = Vec::new();
    for (index, gate) in implementation
        .gates
        .iter()
        .enumerate()
        .filter(|(_, g)| g.kind.is_sequential())
    {
        for (role, offset) in [
            ("MData", (0, 0, 1)),
            ("SData", (0, 0, 0)),
            ("MLock", (1, 0, 1)),
            ("SLock", (1, 0, 0)),
            ("InvC", (2, 0, 0)),
        ] {
            let site: [i32;3] = serde_json::from_value(data["dff_cells"][&gate.output][role].clone()).unwrap();
            let at = (site[0],site[1],site[2]);
            let label = format!("{}:{role}", gate.output);
            watched.push((Position::new(at.0, at.1, at.2), label.clone()));
            hidden.push((index, role, at, label));
        }
    }
    simulator.attach_observer(watched);
    let mut protocol = vec![
        (false, true, false),
        (true, true, false),
        (false, false, true),
    ];
    for _ in 0..16 {
        protocol.extend([(true, false, true), (false, false, true)]);
    }
    protocol.extend([
        (false, false, false), // enable setup before the hold edge
        (true, false, false),
        (false, false, false),
        (false, false, true), // enable setup before the next count edge
        (true, false, true),
        (false, false, true),
        (false, true, true),
        (true, true, true),
        (false, false, false),
    ]);
    if std::env::args().any(|arg|arg=="--no-setup") { protocol.remove(38); protocol.remove(35); }
    let mut q = 0u8;
    let mut previous_clock = false;
    let mut covered = BTreeSet::from([0u8]);
    let mut phases = Vec::new();
    for (ordinal, (clock, reset, enable)) in protocol.into_iter().enumerate() {
        let prior = evaluator.settle(&inputs).unwrap();
        simulator.reset_observer();
        if clock && !previous_clock {
            q = if reset {
                0
            } else if enable {
                if subtract {
                    q.wrapping_sub(1) & 15
                } else {
                    (q + 1) & 15
                }
            } else {
                q
            };
        }
        previous_clock = clock;
        covered.insert(q);
        for (name, value) in [("clk", clock), ("reset", reset), ("en", enable)] {
            inputs.insert(name.to_string(), value);
            let [x, y, z] = pinout.inputs[name];
            let mut lever = simulator.world().get(x, y, z).clone();
            assert_eq!(lever.kind, BlockKind::Lever);
            lever.lit = value;
            simulator.world_mut().set(x, y, z, lever);
        }
        simulator.run_until_stable(2000).unwrap();
        for _ in 0..16 {
            simulator.step();
        }
        evaluator.step(&inputs).unwrap();
        let values = evaluator.settle(&inputs).unwrap();
        for bit in 0..4 {
            let [x, y, z] = pinout.outputs[&format!("q[{bit}]")];
            assert_eq!(
                signal(simulator.world(), (x, y, z)),
                q & (1 << bit) != 0,
                "{top}: phase={ordinal}, Q[{bit}]"
            );
        }
        for gate in &implementation.gates {
            assert_eq!(
                signal(
                    simulator.world(),
                    gate_positions[&gate.output]
                ),
                values[&gate.output],
                "{top}: phase={ordinal}, gate={}",
                gate.output
            );
        }
        for (index, role, at, label) in &hidden {
            let gate = &implementation.gates[*index];
            let expected = match *role {
                "MData" => values[if clock { &gate.output } else { &gate.inputs[0] }],
                "SData" => values[&gate.output],
                "MLock" => clock,
                "SLock" | "InvC" => !clock,
                _ => unreachable!(),
            };
            assert_eq!(
                signal(simulator.world(), *at),
                expected,
                "{top}: phase={ordinal}, {label}"
            );
            if *role == "MData" || *role == "SData" {
                assert_eq!(
                    repeater_is_locked(simulator.world(), Position::new(at.0, at.1, at.2)),
                    if *role == "MData" { clock } else { !clock }
                );
            }
            if *role == "SData" {
                let events: Vec<_> = simulator
                    .observations()
                    .iter()
                    .filter(|event| event.label == *label)
                    .collect();
                assert_eq!(
                    events.len(),
                    usize::from(prior[&gate.output] != expected),
                    "{top}: phase={ordinal}, stored-state events {label}"
                );
                assert!(events.iter().all(|event| event.value == expected));
            }
        }
        phases.push(serde_json::json!({"phase":ordinal,"q":q,"inputs":inputs,"values":values,"events":simulator.observations().iter().map(|event|serde_json::json!({"label":event.label,"tick":event.tick,"value":event.value})).collect::<Vec<_>>()}));
    }
    assert_eq!(covered, (0u8..16).collect());
    std::fs::write(
        directory.join("replayed-phases.json"),
        serde_json::to_vec_pretty(&phases).unwrap(),
    )
    .unwrap();
    println!(
        "{top}: {} phases, 16 states, every implementation gate and hidden DFF cell passed",
        phases.len()
    );
}

fn main(){check_counter(false);}
