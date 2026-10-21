//! Independent synchronous-reset/feedback oracle across opposite clock edges.
use reda::compile::{
    compile, lowering::lower_optimised, metrics::canonical_fingerprint,
};
use reda::formats::litematic::{load, save};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::{block::BlockKind, storage::World},
};
use std::collections::BTreeMap;

fn signal(world: &World, at: (i32, i32, i32)) -> bool {
    let block = world.get(at.0, at.1, at.2);
    if block.kind == BlockKind::RedstoneWire {
        block.power > 0
    } else {
        block.lit
    }
}

#[test]
fn native_mixed_edges_reset_feedback_matches_loaded_state_trajectory() {
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "mixed_clock_reset_feedback.sv",
            text: include_str!("fixtures/mixed_clock_reset_feedback.sv"),
        }],
        &CompileOptions::new("mixed_clock_reset_feedback"),
    )
    .unwrap();
    let outputs = artifact.output_map();
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let physical = compile(&lowered).unwrap();
    let exact_cells: Vec<_> = physical
        .world
        .cells()
        .iter()
        .enumerate()
        .map(|(flat, &index)| {
            (
                physical.world.decode(flat),
                physical.world.palette().get(index).unwrap(),
            )
        })
        .collect();
    let world_fingerprint =
        canonical_fingerprint(&serde_json::to_vec(&(physical.world.size(), exact_cells)).unwrap());
    let path = std::env::temp_dir().join(format!(
        "reda-mixed-clock-reset-{}.litematic",
        std::process::id()
    ));
    save(&path, &physical.world, "mixed-clock-reset-feedback").unwrap();
    let world = load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(physical.world.size(), world.size());
    for (flat, &index) in physical.world.cells().iter().enumerate() {
        let at = physical.world.decode(flat);
        assert_eq!(
            physical.world.palette().get(index).unwrap(),
            world.get(at.0, at.1, at.2)
        );
    }
    let mut simulator = Simulator::new(world);
    let mut evaluator = Evaluator::new(&artifact.netlist).unwrap();
    let mut inputs: BTreeMap<_, _> = artifact
        .netlist
        .inputs
        .iter()
        .map(|name| (name.clone(), false))
        .collect();
    let mut clocks = [false; 2];
    let mut state = [false; 2];
    simulator.run_until_stable(2000).unwrap();
    let mut watched = Vec::new();
    for port in ["q_a", "q_b"] {
        let at = physical.output_positions[&outputs[port]];
        assert!(!signal(simulator.world(), at));
        watched.push((Position::new(at.0, at.1, at.2), format!("lamp:{port}")));
    }
    let power_on: BTreeMap<_, _> = watched
        .iter()
        .map(|(at, label)| (label.clone(), signal(simulator.world(), (at.x, at.y, at.z))))
        .collect();
    simulator.attach_observer(watched);
    let mut replay = power_on.clone();
    let mut phases = Vec::new();
    // Exhaust reset/enable settings under separate and simultaneous edges.
    // Changes while no active edge occurs must hold state, even during reset.
    for control in 0u8..8 {
        for next_clocks in [0u8, 1, 3, 2, 0, 3, 0, 2, 3, 1, 0] {
            for control in [control, control ^ 7, control] {
                let reset_a = control & 1 != 0;
                let reset_b = control & 2 != 0;
                let en = control & 4 != 0;
                let next = [next_clocks & 1 != 0, next_clocks & 2 != 0];
                let old = state;
                if !clocks[0] && next[0] {
                    state[0] = if reset_a {
                        false
                    } else if en {
                        !old[0]
                    } else {
                        old[0]
                    };
                }
                if clocks[1] && !next[1] {
                    state[1] = if reset_b { false } else { old[0] };
                }
                clocks = next;
                let start = simulator.current_tick();
                simulator.reset_observer();
                for (port, value) in [
                    ("clk_a", next[0]),
                    ("clk_b", next[1]),
                    ("reset_a", reset_a),
                    ("reset_b", reset_b),
                    ("en", en),
                ] {
                    inputs.insert(port.to_owned(), value);
                    let at = physical.input_positions[port];
                    let mut block = simulator.world().get(at.0, at.1, at.2).clone();
                    block.lit = value;
                    simulator.world_mut().set(at.0, at.1, at.2, block);
                }
                simulator.run_until_stable(2000).unwrap();
                // Same existing Design H dwell; no larger settling cap.
                for _ in 0..16 {
                    simulator.step();
                }
                let logical = evaluator.step(&inputs).unwrap();
                for (bit, port) in ["q_a", "q_b"].into_iter().enumerate() {
                    assert_eq!(
                        logical[&outputs[port]], state[bit],
                        "logical {port}, control={control}, clocks={next_clocks}"
                    );
                    assert_eq!(
                        signal(simulator.world(), physical.output_positions[&outputs[port]]),
                        state[bit],
                        "physical {port}, control={control}, clocks={next_clocks}"
                    );
                }
                for event in simulator.observations() {
                    replay.insert(event.label.clone(), event.value);
                }
                for (bit, port) in ["q_a", "q_b"].into_iter().enumerate() {
                    assert_eq!(
                        replay[&format!("lamp:{port}")],
                        state[bit],
                        "lamp event replay {port}"
                    );
                }
                phases.push(serde_json::json!({"control":control,"clocks":next_clocks,"state":state,"inputs":inputs,"ticks":simulator.current_tick()-start,"events":simulator.observations().iter().map(|event| serde_json::json!({"label":event.label,"tick":event.tick-start,"value":event.value})).collect::<Vec<_>>() }));
            }
        }
    }
    println!(
        "MIXED_CLOCK_RESET_TRACE {}",
        serde_json::json!({"world_fingerprint":world_fingerprint,"outputs":outputs,"power_on":power_on,"gates":lowered.gates.iter().map(|gate| serde_json::json!({"output":gate.output,"inputs":gate.inputs,"kind":gate.kind.wire_name()})).collect::<Vec<_>>(),"phases":phases})
    );
}
