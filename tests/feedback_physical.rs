//! Literal state-machine oracles over loaded artifacts and all implemented signals.
use reda::compile::{compile, geometry::rotate, lowering::lower_optimised, Netlist};
use reda::formats::litematic::{load, save};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, synthesize_verilog, CompileOptions, SourceInput};
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::{block::BlockKind, storage::World},
};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy)]
enum Case {
    Counter,
    Fsm,
    StateEnable,
}
fn next(case: Case, q: u8, control: u8) -> u8 {
    let first = control & 1 != 0;
    let enabled = control & 2 != 0;
    match case {
        Case::Counter | Case::Fsm if first => 0,
        Case::Counter if enabled => (q + 1) % 4,
        Case::Fsm if enabled => (q + 1) % 3,
        Case::StateEnable if q != 0 || enabled => first as u8,
        _ => q,
    }
}
fn signal(world: &World, at: (i32, i32, i32)) -> bool {
    let b = world.get(at.0, at.1, at.2);
    if b.kind == BlockKind::RedstoneWire {
        b.power > 0
    } else {
        b.lit
    }
}
fn check(netlist: Netlist, outputs: HashMap<String, String>, case: Case, name: &str) {
    let lowered = lower_optimised(&netlist).unwrap();
    let physical = compile(&lowered).unwrap();
    let names: Vec<_> = if matches!(case, Case::StateEnable) {
        vec!["q".to_owned()]
    } else {
        vec!["q[0]".to_owned(), "q[1]".to_owned()]
    };
    let pins = if matches!(case, Case::Counter) {
        ["reset", "en"]
    } else if matches!(case, Case::Fsm) {
        ["reset", "advance"]
    } else {
        ["d", "en"]
    };
    let path = std::env::temp_dir().join(format!(
        "reda-feedback-{}-{name}.litematic",
        std::process::id()
    ));
    save(&path, &physical.world, name).unwrap();
    let world = load(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    let mut sim = Simulator::new(world);
    let mut evaluator = Evaluator::new(&netlist).unwrap();
    let mut inputs: BTreeMap<_, _> = netlist.inputs.iter().map(|s| (s.clone(), false)).collect();
    let mut q = 0u8;
    let mut clock = false;
    sim.run_until_stable(2000).unwrap();
    for port in &names {
        assert!(
            !signal(sim.world(), physical.output_positions[&outputs[port]]),
            "power-on {port}"
        );
    }
    let mut watched: Vec<_> = lowered
        .gates
        .iter()
        .map(|g| {
            let p = physical.gate_output_positions[&g.output];
            (Position::new(p.0, p.1, p.2), format!("gate:{}", g.output))
        })
        .collect();
    for port in &names {
        let p = physical.output_positions[&outputs[port]];
        watched.push((Position::new(p.0, p.1, p.2), format!("lamp:{port}")));
    }
    let mut sites = BTreeMap::new();
    for (index, gate) in lowered
        .gates
        .iter()
        .enumerate()
        .filter(|(_, g)| g.kind.is_sequential())
    {
        let p = physical.gate_output_positions[&gate.output];
        for (role, offset) in [
            ("MData", (0, 0, 1)),
            ("SData", (0, 0, 0)),
            ("MLock", (1, 0, 1)),
            ("SLock", (1, 0, 0)),
            ("InvC", (2, 0, 0)),
        ] {
            let d = rotate(offset, physical.gate_facings[index]);
            let at = (p.0 + d.0, p.1 + d.1, p.2 + d.2);
            let label = format!("cell:{}:{role}", gate.output);
            watched.push((Position::new(at.0, at.1, at.2), label.clone()));
            sites.insert(label, (gate.output.clone(), role, at));
        }
    }
    let power_on: BTreeMap<_, _> = watched
        .iter()
        .map(|(at, label)| (label.clone(), signal(sim.world(), (at.x, at.y, at.z))))
        .collect();
    sim.attach_observer(watched);
    let mut phases = Vec::new();
    for cycle in 0..64 {
        let controls = if matches!(case, Case::StateEnable) {
            [3, 0, 1, 2, 3, 1, 0, 1]
        } else {
            [1, 2, 2, 2, 2, 0, 3, 2]
        };
        let control = controls[cycle % 8];
        for (control, clk) in [
            (control, false),
            (control, true),
            (control ^ 3, true),
            (control ^ 3, false),
        ] {
            let start = sim.current_tick();
            sim.reset_observer();
            if clk && !clock {
                q = next(case, q, control);
            }
            clock = clk;
            for (port, value) in [
                ("clk", clk),
                (pins[0], control & 1 != 0),
                (pins[1], control & 2 != 0),
            ] {
                inputs.insert(port.to_owned(), value);
                let p = physical.input_positions[port];
                let mut b = sim.world().get(p.0, p.1, p.2).clone();
                b.lit = value;
                sim.world_mut().set(p.0, p.1, p.2, b);
            }
            sim.run_until_stable(2000).unwrap();
            for _ in 0..16 {
                sim.step();
            }
            let evaluated = evaluator.step(&inputs).unwrap();
            let mut values: HashMap<_, _> = inputs.clone().into_iter().collect();
            for (bit, port) in names.iter().enumerate() {
                let expected = q & (1 << bit) != 0;
                values.insert(outputs[port].clone(), expected);
                assert_eq!(evaluated[&outputs[port]], expected, "{name} logical {port}");
                assert_eq!(
                    signal(sim.world(), physical.output_positions[&outputs[port]]),
                    expected,
                    "{name} physical {port}, cycle={cycle}, control={control}, clk={clk}"
                );
            }
            for index in lowered.combinational_order().unwrap() {
                let g = &lowered.gates[index];
                if !g.kind.is_sequential() {
                    let operands = g.inputs.iter().map(|s| values[s]).collect::<Vec<_>>();
                    values.insert(g.output.clone(), g.kind.evaluate(&operands));
                }
                assert_eq!(
                    signal(sim.world(), physical.gate_output_positions[&g.output]),
                    values[&g.output],
                    "{name} internal {}, cycle={cycle}, control={control}, clk={clk}",
                    g.output
                );
            }
            let mut cells = BTreeMap::new();
            for (label, (output, role, at)) in &sites {
                let g = lowered.gates.iter().find(|g| &g.output == output).unwrap();
                let b = sim.world().get(at.0, at.1, at.2);
                let expected = match *role {
                    "MData" => {
                        if clk {
                            values[output]
                        } else {
                            values[&g.inputs[0]]
                        }
                    }
                    "SData" => values[output],
                    "MLock" => clk,
                    "SLock" | "InvC" => !clk,
                    _ => unreachable!(),
                };
                assert_eq!(
                    b.lit, expected,
                    "{name} {label}, cycle={cycle}, control={control}, clk={clk}"
                );
                if *role == "MData" {
                    assert_eq!(
                        reda::redstone::simulator::component::repeater_is_locked(
                            sim.world(),
                            Position::new(at.0, at.1, at.2)
                        ),
                        clk
                    );
                }
                if *role == "SData" {
                    assert_eq!(
                        reda::redstone::simulator::component::repeater_is_locked(
                            sim.world(),
                            Position::new(at.0, at.1, at.2)
                        ),
                        !clk
                    );
                }
                cells.insert(
                    label,
                    json!({"lit":b.lit,"locked":reda::redstone::simulator::component::repeater_is_locked(sim.world(), Position::new(at.0, at.1, at.2)),"kind":format!("{:?}",b.kind)}),
                );
            }
            phases.push(json!({"control":control,"clock":clk,"expected":q,"inputs":inputs,"internal":values,"cells":cells,"ticks":sim.current_tick()-start,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
        }
    }
    println!(
        "FEEDBACK_TRACE {}",
        json!({"case":name,"power_on":power_on,"outputs":outputs,"gates":lowered.gates.iter().map(|g|json!({"output":g.output,"inputs":g.inputs,"kind":g.kind.wire_name()})).collect::<Vec<_>>(),"phases":phases})
    );
}
#[test]
fn feedback_reset_counter_fsm_and_state_enable_match_loaded_physical_cells() {
    for (top, text, case) in [
        (
            "feedback_counter",
            include_str!("fixtures/feedback_counter.sv"),
            Case::Counter,
        ),
        (
            "feedback_fsm",
            include_str!("fixtures/feedback_fsm.sv"),
            Case::Fsm,
        ),
        (
            "feedback_state_enable",
            include_str!("fixtures/feedback_state_enable.sv"),
            Case::StateEnable,
        ),
    ] {
        let artifact = compile_systemverilog(
            &[SourceInput { name: top, text }],
            &CompileOptions::new(top),
        )
        .unwrap();
        check(
            artifact.netlist.clone(),
            artifact.output_map(),
            case,
            &format!("native-{top}"),
        );
        let (netlist, outputs) = synthesize_verilog(text, top).unwrap();
        check(netlist, outputs, case, &format!("yosys-{top}"));
    }
}

#[test]
fn statement_reset_and_nested_enable_match_loaded_physical_cells() {
    for (top, text, case) in [
        (
            "reset_counter",
            include_str!("fixtures/reset_counter.sv"),
            Case::Counter,
        ),
        (
            "hierarchy_reset_counter",
            include_str!("fixtures/hierarchy_reset_counter.sv"),
            Case::Counter,
        ),
        (
            "reset_fsm",
            include_str!("fixtures/reset_fsm.sv"),
            Case::Fsm,
        ),
        (
            "conditional_state_enable",
            include_str!("fixtures/conditional_state_enable.sv"),
            Case::StateEnable,
        ),
    ] {
        let artifact = compile_systemverilog(
            &[SourceInput { name: top, text }],
            &CompileOptions::new(top),
        )
        .unwrap();
        check(
            artifact.netlist.clone(),
            artifact.output_map(),
            case,
            &format!("native-{top}"),
        );
        let (netlist, outputs) = synthesize_verilog(text, top).unwrap();
        check(netlist, outputs, case, &format!("yosys-{top}"));
    }
}
