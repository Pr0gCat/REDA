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
    Plain,
    Counter,
    Mixed,
}
fn next(case: Case, q: u8, control: u8, previous: bool, clock: bool) -> u8 {
    if previous && !clock {
        match case {
            Case::Plain => (control & 1 != 0) as u8,
            Case::Counter if control & 1 != 0 => 0,
            Case::Counter if control & 2 != 0 => (q + 1) % 4,
            Case::Mixed => (q & 1) | ((q & 1) << 1),
            _ => q,
        }
    } else if !previous && clock && matches!(case, Case::Mixed) {
        (q & 2) | (control & 1)
    } else {
        q
    }
}
fn negative(case: Case, bit: usize) -> bool {
    !matches!(case, Case::Mixed) || bit == 1
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
    let names: Vec<_> = if matches!(case, Case::Plain) {
        vec!["q".to_owned()]
    } else {
        vec!["q[0]".to_owned(), "q[1]".to_owned()]
    };
    let pins = if matches!(case, Case::Counter) {
        vec!["reset", "en"]
    } else {
        vec!["d"]
    };
    for (bit, port) in names.iter().enumerate() {
        let gate = lowered
            .gates
            .iter()
            .find(|g| g.output == outputs[port])
            .unwrap();
        assert!(gate.kind.is_sequential());
        if negative(case, bit) {
            let inverter = lowered
                .gates
                .iter()
                .find(|g| g.output == gate.inputs[1])
                .unwrap();
            assert_eq!(inverter.kind, reda::compile::topology::GateKind::Nor(1));
            assert_eq!(inverter.inputs, vec!["clk"]);
        } else {
            assert_eq!(gate.inputs[1], "clk");
        }
    }
    let path = std::env::temp_dir().join(format!(
        "reda-negedge-{}-{name}.litematic",
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
        let controls = if !matches!(case, Case::Counter) {
            [0, 3, 2, 1, 0, 2, 3, 1]
        } else {
            [1, 2, 2, 2, 2, 0, 3, 2]
        };
        let control = controls[cycle % 8];
        for (control, clk) in [
            (control ^ 3, false),
            (control ^ 3, true),
            (control, true),
            (control, false),
        ] {
            let start = sim.current_tick();
            sim.reset_observer();
            q = next(case, q, control, clock, clk);
            clock = clk;
            for (port, value) in std::iter::once(("clk", clk)).chain(
                pins.iter()
                    .enumerate()
                    .map(|(bit, &port)| (port, control & (1 << bit) != 0)),
            ) {
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
                let bit = names
                    .iter()
                    .position(|port| outputs[port] == *output)
                    .unwrap();
                let cell_clock = if negative(case, bit) { !clk } else { clk };
                let expected = match *role {
                    "MData" => {
                        if cell_clock {
                            values[output]
                        } else {
                            values[&g.inputs[0]]
                        }
                    }
                    "SData" => values[output],
                    "MLock" => cell_clock,
                    "SLock" | "InvC" => !cell_clock,
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
                        cell_clock
                    );
                }
                if *role == "SData" {
                    assert_eq!(
                        reda::redstone::simulator::component::repeater_is_locked(
                            sim.world(),
                            Position::new(at.0, at.1, at.2)
                        ),
                        !cell_clock
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
        "NEGEDGE_TRACE {}",
        json!({"case":name,"power_on":power_on,"outputs":outputs,"gates":lowered.gates.iter().map(|g|json!({"output":g.output,"inputs":g.inputs,"kind":g.kind.wire_name()})).collect::<Vec<_>>(),"phases":phases})
    );
}
#[test]
fn falling_edges_and_mixed_edge_shift_match_loaded_physical_cells() {
    for (top, text, case) in [
        (
            "negedge_dff",
            include_str!("fixtures/negedge_dff.sv"),
            Case::Plain,
        ),
        (
            "negedge_counter",
            include_str!("fixtures/negedge_counter.sv"),
            Case::Counter,
        ),
        (
            "mixed_edge_shift",
            include_str!("fixtures/mixed_edge_shift.sv"),
            Case::Mixed,
        ),
        (
            "hierarchy_negedge_dff",
            include_str!("fixtures/hierarchy_negedge_dff.sv"),
            Case::Plain,
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
fn statement_reset_on_falling_edges_matches_loaded_physical_cells() {
    for (top, text, case) in [
        ("reset_counter", include_str!("fixtures/reset_counter.sv"), Case::Counter),
        ("hierarchy_reset_counter", include_str!("fixtures/hierarchy_reset_counter.sv"), Case::Counter),
    ] {
        let text = text.replace("posedge", "negedge");
        let text = text.as_str();
        let artifact = compile_systemverilog(
            &[SourceInput { name: top, text }],
            &CompileOptions::new(top),
        )
        .unwrap();
        check(
            artifact.netlist.clone(),
            artifact.output_map(),
            case,
            &format!("native-negedge_{top}"),
        );
        let (netlist, outputs) = synthesize_verilog(text, top).unwrap();
        check(netlist, outputs, case, &format!("yosys-negedge_{top}"));
    }
}
