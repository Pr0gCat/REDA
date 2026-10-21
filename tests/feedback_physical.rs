//! Literal state-machine oracles over loaded artifacts and all implemented signals.
use reda::compile::{compile, lowering::lower_optimised, Netlist};
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
    ArithmeticCounter { subtract: bool },
    Fsm,
    StateEnable,
}
fn next(case: Case, q: u8, control: u8) -> u8 {
    let first = control & 1 != 0;
    let enabled = control & 2 != 0;
    match case {
        Case::Counter | Case::Fsm | Case::ArithmeticCounter { .. } if first => 0,
        Case::Counter if enabled => (q + 1) % 4,
        Case::ArithmeticCounter { subtract } if enabled => {
            if subtract {
                q.wrapping_sub(1) & 15
            } else {
                q.wrapping_add(1) & 15
            }
        }
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
        let width = if matches!(case, Case::ArithmeticCounter { .. }) {
            4
        } else {
            2
        };
        (0..width).map(|bit| format!("q[{bit}]")).collect()
    };
    let pins = if matches!(case, Case::Counter | Case::ArithmeticCounter { .. }) {
        ["reset", "en"]
    } else if matches!(case, Case::Fsm) {
        ["reset", "advance"]
    } else {
        ["d", "en"]
    };
    let path = std::env::temp_dir().join(format!(
        "reda-feedback-{}-{:?}-{name}.litematic",
        std::process::id(),
        std::thread::current().id()
    ));
    save(&path, &physical.bench, name).unwrap();
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
    let mut watched = Vec::new();
    for port in &names {
        let p = physical.output_positions[&outputs[port]];
        watched.push((Position::new(p.0, p.1, p.2), format!("lamp:{port}")));
    }
    let power_on: BTreeMap<_, _> = watched
        .iter()
        .map(|(at, label)| (label.clone(), signal(sim.world(), (at.x, at.y, at.z))))
        .collect();
    sim.attach_observer(watched);
    let mut phases = Vec::new();
    // Minimal cycle set: every semantic regime (reset, increment, decrement,
    // hold, wrap) plus full state coverage, with no repeated periods. A 4-bit
    // counter has 16 states, so 16 increments cover them all; the period-8
    // control word exposes all eight regimes in 8 cycles.
    let cycles = if matches!(case, Case::ArithmeticCounter { .. }) {
        19
    } else {
        8
    };
    for cycle in 0..cycles {
        let controls = if matches!(case, Case::StateEnable) {
            [3, 0, 1, 2, 3, 1, 0, 1]
        } else {
            [1, 2, 2, 2, 2, 0, 3, 2]
        };
        let control = if matches!(case, Case::ArithmeticCounter { .. }) {
            if cycle == 0 {
                1
            } else if cycle == 17 {
                3
            } else if cycle == 18 {
                0
            } else {
                2
            }
        } else {
            controls[cycle % 8]
        };
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
            for (bit, port) in names.iter().enumerate() {
                let expected = q & (1 << bit) != 0;
                assert_eq!(evaluated[&outputs[port]], expected, "{name} logical {port}");
                assert_eq!(
                    signal(sim.world(), physical.output_positions[&outputs[port]]),
                    expected,
                    "{name} physical {port}, cycle={cycle}, control={control}, clk={clk}"
                );
            }
            phases.push(json!({"control":control,"clock":clk,"expected":q,"inputs":inputs,"ticks":sim.current_tick()-start,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
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

#[test]
fn arithmetic_counters_match_loaded_physical_cells() {
    for (top, text, case) in [
        (
            "arithmetic_counter_add",
            include_str!("fixtures/arithmetic_counter_add.sv"),
            Case::ArithmeticCounter { subtract: false },
        ),
        (
            "arithmetic_counter_subtract",
            include_str!("fixtures/arithmetic_counter_subtract.sv"),
            Case::ArithmeticCounter { subtract: true },
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

// Individual native controls keep the original full Native/Yosys suite intact.
fn check_native_arithmetic_counter(subtract: bool) {
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

    let artifact = compile_systemverilog(
        &[SourceInput { name: top, text }],
        &CompileOptions::new(top),
    )
    .unwrap();
    check(
        artifact.netlist.clone(),
        artifact.output_map(),
        Case::ArithmeticCounter { subtract },
        &format!("native-{top}"),
    );
}

#[test]
fn native_arithmetic_increment_matches_loaded_physical_cells() {
    check_native_arithmetic_counter(false);
}

#[test]
fn native_arithmetic_decrement_matches_loaded_physical_cells() {
    check_native_arithmetic_counter(true);
}

// Each fixture is its own test so the libtest harness parallelizes the slow
// per-fixture compilation across cores.
fn check_native_feedback(top: &str, text: &str, case: Case) {
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
}

#[test]
fn native_feedback_counter_matches_loaded_physical_cells() {
    check_native_feedback(
        "feedback_counter",
        include_str!("fixtures/feedback_counter.sv"),
        Case::Counter,
    );
}
#[test]
fn native_feedback_fsm_matches_loaded_physical_cells() {
    check_native_feedback(
        "feedback_fsm",
        include_str!("fixtures/feedback_fsm.sv"),
        Case::Fsm,
    );
}
#[test]
fn native_feedback_state_enable_matches_loaded_physical_cells() {
    check_native_feedback(
        "feedback_state_enable",
        include_str!("fixtures/feedback_state_enable.sv"),
        Case::StateEnable,
    );
}
#[test]
fn native_reset_counter_matches_loaded_physical_cells() {
    check_native_feedback(
        "reset_counter",
        include_str!("fixtures/reset_counter.sv"),
        Case::Counter,
    );
}
#[test]
fn native_hierarchy_reset_counter_matches_loaded_physical_cells() {
    check_native_feedback(
        "hierarchy_reset_counter",
        include_str!("fixtures/hierarchy_reset_counter.sv"),
        Case::Counter,
    );
}
#[test]
fn native_reset_fsm_matches_loaded_physical_cells() {
    check_native_feedback(
        "reset_fsm",
        include_str!("fixtures/reset_fsm.sv"),
        Case::Fsm,
    );
}
#[test]
fn native_conditional_state_enable_matches_loaded_physical_cells() {
    check_native_feedback(
        "conditional_state_enable",
        include_str!("fixtures/conditional_state_enable.sv"),
        Case::StateEnable,
    );
}
