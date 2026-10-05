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
            include_str!("fixtures/arithmetic_counter_subtract.sv"),
        )
    } else {
        (
            "arithmetic_counter_add",
            include_str!("fixtures/arithmetic_counter_add.sv"),
        )
    };
    println!("{top}: physical_compile_begin");
    let circuit = compile_systemverilog_physical(
        &[SourceInput { name: top, text }],
        &CompileOptions::new(top),
    )
    .unwrap();
    println!("{top}: physical_compile_complete");
    let implementation = circuit.implementation_netlist();
    assert_eq!(
        implementation
            .gates
            .iter()
            .filter(|g| g.kind.is_sequential())
            .count(),
        4
    );
    assert_eq!(circuit.pinout.outputs.len(), 4);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/qualification/native-arithmetic-generation")
        .join(top);
    std::fs::create_dir_all(&directory).unwrap();
    let bundle = directory.join("counter.litematic");
    circuit.save_bundle(&bundle).unwrap();
    let (loaded, pinout) = ResolvedPinout::load_bundle(&bundle, &circuit).unwrap();
    assert_eq!(pinout, circuit.pinout);
    assert_eq!(
        canonical_world_fingerprint(&loaded),
        canonical_world_fingerprint(&circuit.physical.world)
    );
    // Bind replay observations to the actual emitted implementation.
    let observations = serde_json::json!({
        "top": top, "source": text, "pinout": circuit.pinout,
        "implementation": {
            "inputs": implementation.inputs, "outputs": implementation.outputs,
            "gates": implementation.gates.iter().map(|gate| serde_json::json!({
                "name": gate.name, "output": gate.output, "inputs": gate.inputs,
                "kind": gate.kind.wire_name(),
            })).collect::<Vec<_>>(),
        },
        "gate_output_positions": circuit.physical.gate_output_positions,
        "dff_cells": implementation.gates.iter().enumerate().filter(|(_,gate)|gate.kind.is_sequential())
            .map(|(index,gate)| {
                let anchor = circuit.physical.gate_output_positions[&gate.output];
                let sites: BTreeMap<_,_> = [("MData",(0,0,1)),("SData",(0,0,0)),("MLock",(1,0,1)),("SLock",(1,0,0)),("InvC",(2,0,0))].into_iter().map(|(role,offset)| {
                    let delta = rotate(offset,circuit.physical.gate_facings[index]);
                    (role,[anchor.0+delta.0,anchor.1+delta.1,anchor.2+delta.2])
                }).collect();
                (gate.output.clone(),sites)
            }).collect::<BTreeMap<_,_>>(),
        "world_fingerprint": canonical_world_fingerprint(&loaded),
    });
    std::fs::write(
        directory.join("observations.json"),
        serde_json::to_vec_pretty(&observations).unwrap(),
    )
    .unwrap();
    let mut simulator = Simulator::new(loaded);
    let mut evaluator = Evaluator::new(implementation).unwrap();
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
        let anchor = circuit.physical.gate_output_positions[&gate.output];
        for (role, offset) in [
            ("MData", (0, 0, 1)),
            ("SData", (0, 0, 0)),
            ("MLock", (1, 0, 1)),
            ("SLock", (1, 0, 0)),
            ("InvC", (2, 0, 0)),
        ] {
            let delta = rotate(offset, circuit.physical.gate_facings[index]);
            let at = (anchor.0 + delta.0, anchor.1 + delta.1, anchor.2 + delta.2);
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
                    circuit.physical.gate_output_positions[&gate.output]
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
        directory.join("phases.json"),
        serde_json::to_vec_pretty(&phases).unwrap(),
    )
    .unwrap();
    println!(
        "{top}: {} phases, 16 states, every implementation gate and hidden DFF cell passed",
        phases.len()
    );
}
#[test]
fn native_increment_generates_correct_loaded_feedback() {
    check_counter(false);
}
#[test]
fn native_decrement_generates_correct_loaded_feedback() {
    check_counter(true);
}

#[test]
fn native_and_auto_select_the_same_counter_graph_without_external_frontends() {
    const CHILD: &str = "REDA_ARITHMETIC_SELECTION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "native_and_auto_select_the_same_counter_graph_without_external_frontends",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("PATH", "")
            .env("REDA_PYTHON", "/nonexistent/reda-python")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    use reda::frontend::{compile_systemverilog, synthesize_selected, FrontendMode};
    for (top, text) in [
        (
            "arithmetic_counter_add",
            include_str!("fixtures/arithmetic_counter_add.sv"),
        ),
        (
            "arithmetic_counter_subtract",
            include_str!("fixtures/arithmetic_counter_subtract.sv"),
        ),
    ] {
        let source = SourceInput { name: top, text };
        let artifact = compile_systemverilog(&[source.clone()], &CompileOptions::new(top)).unwrap();
        let lowered = reda::compile::lowering::lower_optimised(&artifact.netlist).unwrap();
        for mode in [FrontendMode::Native, FrontendMode::Auto] {
            let selected = synthesize_selected(source.clone(), top, mode).unwrap();
            assert_eq!(selected.frontend, FrontendMode::Native);
            assert!(selected.fallback_reason.is_none());
            assert_eq!(selected.netlist, artifact.netlist);
            assert_eq!(selected.outputs, artifact.output_map());
            assert_eq!(
                selected.output_bindings,
                artifact
                    .ordered_output_ports
                    .iter()
                    .map(|port| (port.name.clone(), port.signal.clone()))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                reda::compile::lowering::lower_optimised(&selected.netlist).unwrap(),
                lowered
            );
        }
    }
}

#[test]
fn raw_and_inverted_clocks_keep_zero_state_and_capture_only_the_declared_edge() {
    for falling in [false, true] {
        let edge = if falling { "negedge" } else { "posedge" };
        let text =
            format!("module m(input clk,d,output reg q);always @({edge} clk) q<=~d;endmodule");
        let circuit = compile_systemverilog_physical(
            &[SourceInput {
                name: "clock.sv",
                text: &text,
            }],
            &CompileOptions::new("m"),
        )
        .unwrap();
        let implementation = circuit.implementation_netlist();
        let mut simulator = Simulator::new(circuit.physical.world.clone());
        let mut evaluator = Evaluator::new(implementation).unwrap();
        simulator.run_until_stable(2000).unwrap();
        let [x, y, z] = circuit.pinout.outputs["q"];
        assert!(
            !signal(simulator.world(), (x, y, z)),
            "{edge}: no power-on capture even with D=1"
        );
        let active = !falling;
        let mut previous = false;
        let mut q = false;
        for (clock, d) in [
            (!active, false),
            (active, false),
            (active, true),
            (!active, true),
            (active, true),
            (!active, true),
        ] {
            if if falling {
                previous && !clock
            } else {
                !previous && clock
            } {
                q = !d;
            }
            previous = clock;
            let inputs = BTreeMap::from([("clk".to_string(), clock), ("d".to_string(), d)]);
            for (name, value) in &inputs {
                let [x, y, z] = circuit.pinout.inputs[name];
                let mut lever = simulator.world().get(x, y, z).clone();
                assert_eq!(lever.kind, BlockKind::Lever);
                lever.lit = *value;
                simulator.world_mut().set(x, y, z, lever);
            }
            simulator.run_until_stable(2000).unwrap();
            for _ in 0..16 {
                simulator.step();
            }
            evaluator.step(&inputs).unwrap();
            let values = evaluator.settle(&inputs).unwrap();
            assert_eq!(
                signal(simulator.world(), (x, y, z)),
                q,
                "{edge}: clk={clock}, d={d}"
            );
            for gate in &implementation.gates {
                assert_eq!(
                    signal(
                        simulator.world(),
                        circuit.physical.gate_output_positions[&gate.output]
                    ),
                    values[&gate.output]
                );
            }
        }
    }
}

#[test]
fn independent_raw_and_inverted_clocks_retain_separate_storage() {
    let text="module m(input ca,cb,d,output reg qa,qb);always @(posedge ca) qa<=~d;always @(negedge cb) qb<=d;endmodule";
    let circuit = compile_systemverilog_physical(
        &[SourceInput {
            name: "clocks.sv",
            text,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let mut simulator = Simulator::new(circuit.physical.world.clone());
    let mut evaluator = Evaluator::new(circuit.implementation_netlist()).unwrap();
    simulator.run_until_stable(2000).unwrap();
    for position in circuit.pinout.outputs.values() {
        assert!(!signal(
            simulator.world(),
            (position[0], position[1], position[2])
        ));
    }
    let (mut previous_a, mut previous_b, mut qa, mut qb) = (false, false, false, false);
    for (ca, cb, d) in [
        (false, true, false),
        (true, true, false),
        (true, true, true),
        (false, true, true),
        (false, false, true),
        (true, false, true),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (true, false, false),
    ] {
        if !previous_a && ca {
            qa = !d;
        }
        if previous_b && !cb {
            qb = d;
        }
        previous_a = ca;
        previous_b = cb;
        let inputs = BTreeMap::from([
            ("ca".to_string(), ca),
            ("cb".to_string(), cb),
            ("d".to_string(), d),
        ]);
        for (name, value) in &inputs {
            let [x, y, z] = circuit.pinout.inputs[name];
            let mut lever = simulator.world().get(x, y, z).clone();
            assert_eq!(lever.kind, BlockKind::Lever);
            lever.lit = *value;
            simulator.world_mut().set(x, y, z, lever);
        }
        simulator.run_until_stable(2000).unwrap();
        for _ in 0..16 {
            simulator.step();
        }
        evaluator.step(&inputs).unwrap();
        let values = evaluator.settle(&inputs).unwrap();
        for (name, expected) in [("qa", qa), ("qb", qb)] {
            let [x, y, z] = circuit.pinout.outputs[name];
            assert_eq!(
                signal(simulator.world(), (x, y, z)),
                expected,
                "{name}: ca={ca}, cb={cb}, d={d}"
            );
        }
        for gate in &circuit.implementation_netlist().gates {
            assert_eq!(
                signal(
                    simulator.world(),
                    circuit.physical.gate_output_positions[&gate.output]
                ),
                values[&gate.output]
            );
        }
    }
}
