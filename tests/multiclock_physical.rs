//! Independent-domain capture/hold checks on the serialized physical world.
use reda::compile::metrics::canonical_fingerprint;
use reda::compile::{compile, lowering::lower_optimised, Netlist};
use reda::formats::litematic::{load, save};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::simulator::{position::Position, Simulator};
use reda::redstone::world::block::BlockKind;
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

fn check(netlist: Netlist, outputs: HashMap<String, String>, name: &str, crossing: bool) {
    let lowered = lower_optimised(&netlist).unwrap();
    let physical = compile(&lowered).unwrap();
    let repeated = compile(&lowered).unwrap();
    assert_eq!(physical.world.cells(), repeated.world.cells());
    assert_eq!(physical.world.size(), repeated.world.size());
    assert!(physical
        .world
        .cells()
        .iter()
        .zip(repeated.world.cells())
        .all(|(&a, &b)| physical.world.palette().get(a) == repeated.world.palette().get(b)));
    let cells: Vec<_> = physical
        .world
        .cells()
        .iter()
        .enumerate()
        .filter_map(|(flat, &index)| {
            let block = physical.world.palette().get(index).unwrap();
            (block.kind != BlockKind::Air).then(|| (physical.world.decode(flat), block))
        })
        .collect();
    let world_fingerprint =
        canonical_fingerprint(&serde_json::to_vec(&(physical.world.size(), cells)).unwrap());
    assert_eq!(physical.input_positions, repeated.input_positions);
    assert_eq!(physical.output_positions, repeated.output_positions);
    let path = std::env::temp_dir().join(format!(
        "reda-multiclock-{}-{name}.litematic",
        std::process::id()
    ));
    save(&path, &physical.world, name).unwrap();
    let world = load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let mut sim = Simulator::new(world);
    let mut evaluator = Evaluator::new(&netlist).unwrap();
    let mut state = [false; 2];
    let mut clocks = [false; 2];
    let mut inputs: BTreeMap<_, _> = netlist.inputs.iter().map(|s| (s.clone(), false)).collect();
    sim.run_until_stable(2000).unwrap();
    for (i, name) in ["q_a", "q_b"].into_iter().enumerate() {
        let at = physical.output_positions[&outputs[name]];
        assert_eq!(
            sim.world().get(at.0, at.1, at.2).lit,
            state[i],
            "power-on {name}"
        );
        assert_eq!(
            evaluator.evaluate(&inputs).unwrap()[&outputs[name]],
            state[i]
        );
    }
    let power_on = ["q_a", "q_b"].map(|name| {
        let at = physical.output_positions[&outputs[name]];
        sim.world().get(at.0, at.1, at.2).lit
    });
    let mut phases = Vec::new();
    sim.attach_observer(["q_a", "q_b"].into_iter().flat_map(|name| {
        let signal = &outputs[name];
        let lamp = physical.output_positions[signal];
        let driver = physical.gate_output_positions[signal];
        [
            (
                Position::new(lamp.0, lamp.1, lamp.2),
                format!("lamp_{name}"),
            ),
            (
                Position::new(driver.0, driver.1, driver.2),
                format!("driver_{name}"),
            ),
        ]
    }));
    // Every prior/data pair, separate and simultaneous edges, and data changes
    // with both clocks high. History is retained throughout all 240 phases.
    for prior in 0u8..4 {
        for data in 0u8..4 {
            for (word, next) in [
                (prior, 0),
                (prior, 1),
                (prior, 3),
                (prior ^ 3, 3),
                (prior ^ 3, 2),
                (prior ^ 3, 0),
                (data, 0),
                (data, 2),
                (data, 3),
                (data ^ 3, 3),
                (data ^ 3, 1),
                (data ^ 3, 0),
                (data, 0),
                (data, 3),
                (data, 0),
            ] {
                let start = sim.current_tick();
                sim.reset_observer();
                let previous_q = state;
                for i in 0..2 {
                    let d = word & (1 << i) != 0;
                    let clk = next & (1 << i) != 0;
                    if clk && !clocks[i] {
                        state[i] = if crossing && i == 1 { previous_q[0] } else { d };
                    }
                    clocks[i] = clk;
                    for (port, value) in [(["d_a", "d_b"][i], d), (["clk_a", "clk_b"][i], clk)] {
                        inputs.insert(port.into(), value);
                        let at = physical.input_positions[port];
                        let mut block = sim.world().get(at.0, at.1, at.2).clone();
                        block.lit = value;
                        sim.world_mut().set(at.0, at.1, at.2, block);
                    }
                }
                sim.run_until_stable(2000).unwrap();
                // The existing Design H physical protocol uses this dwell to
                // avoid clock-torch burnout, identically to dff_physical.rs.
                for _ in 0..16 {
                    sim.step();
                }
                let logical = evaluator.step(&inputs).unwrap();
                for (i, port) in ["q_a", "q_b"].into_iter().enumerate() {
                    let driver = physical.gate_output_positions[&outputs[port]];
                    assert_eq!(
                        sim.world().get(driver.0, driver.1, driver.2).lit,
                        state[i],
                        "{name} driver {port}"
                    );
                    let at = physical.output_positions[&outputs[port]];
                    assert_eq!(
                        logical[&outputs[port]], state[i],
                        "{name} logical {port}, word={word}, clocks={next}"
                    );
                    assert_eq!(
                        sim.world().get(at.0, at.1, at.2).lit,
                        state[i],
                        "{name} physical {port}, word={word}, clocks={next}"
                    );
                }
                let driver = ["q_a", "q_b"].map(|name| {
                    let p = physical.gate_output_positions[&outputs[name]];
                    sim.world().get(p.0, p.1, p.2).lit
                });
                let lamp = ["q_a", "q_b"].map(|name| {
                    let p = physical.output_positions[&outputs[name]];
                    sim.world().get(p.0, p.1, p.2).lit
                });
                phases.push(json!({"data":word,"clocks":next,"expected":state,"driver":driver,"lamp":lamp,
                    "ticks":sim.current_tick()-start,"events":sim.observations().iter().map(|e|
                        json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
            }
        }
    }
    println!(
        "MULTICLOCK_TRACE {}",
        json!({"case":name,"world_fingerprint":world_fingerprint,"power_on":power_on,
        "inputs":netlist.inputs,"gates":netlist.gates.iter().map(|g|
            json!({"output":g.output,"inputs":g.inputs,"kind":g.kind.wire_name()})).collect::<Vec<_>>(),
        "outputs":outputs,"phases":phases})
    );
}

#[test]
fn native_and_yosys_independent_clock_domains_match_the_physical_state_oracle() {
    for (top, text, crossing) in [
        (
            "multiclock_dff",
            include_str!("fixtures/multiclock_dff.sv"),
            false,
        ),
        (
            "hierarchy_multiclock_dff",
            include_str!("fixtures/hierarchy_multiclock_dff.sv"),
            false,
        ),
        (
            "multiclock_shift",
            include_str!("fixtures/multiclock_shift.sv"),
            true,
        ),
    ] {
        let artifact = compile_systemverilog(
            &[SourceInput { name: top, text }],
            &CompileOptions::new(top),
        )
        .unwrap();
        let repeated = compile_systemverilog(
            &[SourceInput { name: top, text }],
            &CompileOptions::new(top),
        )
        .unwrap();
        assert_eq!(artifact.netlist, repeated.netlist);
        assert_eq!(artifact.debug.to_json(), repeated.debug.to_json());
        check(
            artifact.netlist.clone(),
            artifact.output_map(),
            &format!("native-{top}"),
            crossing,
        );
        let (netlist, outputs) = synthesize_verilog(text, top).unwrap();
        check(netlist, outputs, &format!("yosys-{top}"), crossing);
    }
}
