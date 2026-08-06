//! 驗收測試：網表進去，紅石世界出來，模擬器驗證跟真值表一致。
//!
//! 這是第一條端到端的編譯路徑 —— 前面所有測試都驗證手搭的電路；這裡驗證
//! `compile()` 自己排出來的電路。

use std::path::PathBuf;

use reda::compile::{compile, CompileError, Gate, Netlist};
use reda::formats::litematic;
use reda::redstone::simulator::Simulator;

const MAX_TICKS: u64 = 500;

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) {
    let mut state = simulator
        .world()
        .get(position.0, position.1, position.2)
        .clone();
    state.lit = on;
    simulator
        .world_mut()
        .set(position.0, position.1, position.2, state);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle after changing an input");
}

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator.world().get(position.0, position.1, position.2).lit
}

fn not_netlist() -> Netlist {
    Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![Gate {
            name: "g1".to_string(),
            inputs: vec!["a".to_string()],
            output: "y".to_string(),
        }],
    }
}

fn and_netlist() -> Netlist {
    // AND = NOR(NOT a, NOT b)
    Netlist {
        inputs: vec!["a".to_string(), "b".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![
            Gate { name: "not_a".to_string(), inputs: vec!["a".to_string()], output: "na".to_string() },
            Gate { name: "not_b".to_string(), inputs: vec!["b".to_string()], output: "nb".to_string() },
            Gate {
                name: "final_nor".to_string(),
                inputs: vec!["na".to_string(), "nb".to_string()],
                output: "y".to_string(),
            },
        ],
    }
}

#[test]
fn a_compiled_not_gate_matches_its_truth_table() {
    let netlist = not_netlist();
    let compiled = compile(&netlist).expect("a single NOR gate has no cycle and is fully driven");

    let mut simulator = Simulator::new(compiled.world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle before the first reading");

    let lever_a = *compiled.input_positions.get("a").unwrap();
    let output_y = *compiled.output_positions.get("y").unwrap();

    let rows: [(bool, bool); 2] = [(false, true), (true, false)];
    for (a, expected) in rows {
        set_lever(&mut simulator, lever_a, a);
        let output = read_output(&simulator, output_y);
        assert_eq!(output, expected, "NOT({a}) should be {expected}, got {output}");
    }
}

#[test]
fn a_compiled_and_gate_matches_its_truth_table() {
    let netlist = and_netlist();
    let compiled = compile(&netlist).expect("this netlist is acyclic and fully driven");

    let mut simulator = Simulator::new(compiled.world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle before the first reading");

    let lever_a = *compiled.input_positions.get("a").unwrap();
    let lever_b = *compiled.input_positions.get("b").unwrap();
    let output_y = *compiled.output_positions.get("y").unwrap();

    // 四列全測：00->0, 01->0, 10->0, 11->1
    let rows: [(bool, bool, bool); 4] = [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ];

    for (a, b, expected) in rows {
        set_lever(&mut simulator, lever_a, a);
        set_lever(&mut simulator, lever_b, b);
        let output = read_output(&simulator, output_y);
        assert_eq!(output, expected, "AND({a}, {b}) should be {expected}, got {output}");
    }
}

#[test]
fn a_compiled_circuit_saves_to_a_loadable_litematic() {
    let netlist = and_netlist();
    let compiled = compile(&netlist).expect("this netlist is acyclic and fully driven");

    let mut path = PathBuf::from(std::env::var("CARGO_TARGET_TMPDIR").unwrap_or_else(|_| {
        std::env::temp_dir().to_string_lossy().to_string()
    }));
    path.push("reda_compile_end_to_end_and_gate.litematic");

    litematic::save(&path, &compiled.world, "and_gate").expect("saving must succeed");
    let loaded = litematic::load(&path).expect("loading must succeed");

    assert_eq!(loaded.size(), compiled.world.size(), "loaded world must have the same dimensions");

    let (size_x, size_y, size_z) = compiled.world.size();
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                let original = compiled.world.get(x, y, z);
                let round_tripped = loaded.get(x, y, z);
                assert_eq!(
                    original.kind, round_tripped.kind,
                    "block kind mismatch at ({x},{y},{z})"
                );
                assert_eq!(
                    original.name, round_tripped.name,
                    "block name mismatch at ({x},{y},{z})"
                );
                assert_eq!(
                    original.facing, round_tripped.facing,
                    "facing mismatch at ({x},{y},{z})"
                );
            }
        }
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn compiling_the_same_netlist_twice_gives_the_same_world() {
    // Placement, track assignment and feed-through reservation are all greedy
    // searches over hash-map contents, which is exactly the shape of code that
    // silently starts depending on iteration order. If it ever does, a routing
    // bug becomes reproducible only every other run.
    let first = compile(&and_netlist()).expect("this netlist is acyclic and fully driven");
    let second = compile(&and_netlist()).expect("this netlist is acyclic and fully driven");

    assert_eq!(first.world.size(), second.world.size(), "world size must be stable");
    assert_eq!(first.input_positions, second.input_positions);
    assert_eq!(first.output_positions, second.output_positions);

    let (size_x, size_y, size_z) = first.world.size();
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                let a = first.world.get(x, y, z);
                let b = second.world.get(x, y, z);
                assert_eq!(a.kind, b.kind, "block kind differs at ({x},{y},{z})");
                assert_eq!(a.facing, b.facing, "facing differs at ({x},{y},{z})");
            }
        }
    }
}

#[test]
fn a_cyclic_netlist_is_rejected() {
    // g1's input is g2's output and g2's input is g1's output -- a two-gate
    // loop with no external input driving either of them.
    let netlist = Netlist {
        inputs: vec![],
        outputs: vec!["loop_b".to_string()],
        gates: vec![
            Gate { name: "g1".to_string(), inputs: vec!["loop_b".to_string()], output: "loop_a".to_string() },
            Gate { name: "g2".to_string(), inputs: vec!["loop_a".to_string()], output: "loop_b".to_string() },
        ],
    };

    let result = compile(&netlist);

    assert_eq!(result.err(), Some(CompileError::CyclicNetlist));
}
