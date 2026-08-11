//! 驗收測試：網表進去，紅石世界出來，模擬器驗證跟真值表一致。
//!
//! 這是第一條端到端的編譯路徑 —— 前面所有測試都驗證手搭的電路；這裡驗證
//! `compile()` 自己排出來的電路。

use reda::compile::topology::GateKind;
use std::path::PathBuf;

use reda::circuits::and4::build_and4_netlist;
use reda::compile::physical;
use reda::compile::planner::{
    emit_candidate, emit_primitives, seed_from_legacy, verify_candidate, Anchor, NodeRealisation,
    NormalisedScore, PlannerWeights, RouteTerminalKind,
};
use reda::compile::topology::Primitive;
use reda::redstone::world::block::BlockKind;
use reda::redstone::world::storage::World;
use reda::compile::{compile, CompileError, CompiledCircuit, Gate, Netlist};
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
    simulator
        .world()
        .get(position.0, position.1, position.2)
        .lit
}

fn not_netlist() -> Netlist {
    Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![Gate {
            name: "g1".to_string(),
            inputs: vec!["a".to_string()],
            output: "y".to_string(),
            kind: GateKind::Nor(1),
        }],
    }
}

fn and_netlist() -> Netlist {
    // AND = NOR(NOT a, NOT b)
    Netlist {
        inputs: vec!["a".to_string(), "b".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![
            Gate {
                name: "not_a".to_string(),
                inputs: vec!["a".to_string()],
                output: "na".to_string(),
                kind: GateKind::Nor(1),
            },
            Gate {
                name: "not_b".to_string(),
                inputs: vec!["b".to_string()],
                output: "nb".to_string(),
                kind: GateKind::Nor(1),
            },
            Gate {
                name: "final_nor".to_string(),
                inputs: vec!["na".to_string(), "nb".to_string()],
                output: "y".to_string(),
                kind: GateKind::Nor(2),
            },
        ],
    }
}

fn bare_merge_netlist() -> Netlist {
    Netlist {
        inputs: vec!["a".to_string(), "b".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![Gate {
            name: "merge".to_string(),
            inputs: vec!["a".to_string(), "b".to_string()],
            output: "y".to_string(),
            kind: GateKind::Or(2),
        }],
    }
}

fn fanout_netlist() -> Netlist {
    Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["left".to_string(), "right".to_string()],
        gates: vec![Gate::nor("left", &["a"]), Gate::nor("right", &["a"])],
    }
}

fn compiled_and4() -> (Netlist, CompiledCircuit) {
    let (netlist, _) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
    (netlist, compiled)
}

/// A candidate carries anchors, but an anchor alone cannot be turned back
/// into blocks: emission needs to know whether the thing standing there is a
/// torch, a lever, or nothing at all because the gate is a wire merge. Until
/// the seed names that, `physical::variants` has no caller and no candidate
/// can be realised.
#[test]
fn a_seed_names_the_physical_realisation_behind_every_placed_node() {
    let (netlist, compiled) = compiled_and4();

    let seed = seed_from_legacy(&netlist, &compiled).expect("legacy output must be extractable");

    for (gate, node) in netlist.gates.iter().zip(seed.primitive_nodes()) {
        let expected = match gate.kind {
            GateKind::Nor(_) => NodeRealisation::Primitive(Primitive::Torch),
            GateKind::Or(_) => NodeRealisation::WireMerge,
            other => panic!("a compiled netlist cannot contain {other:?}"),
        };
        assert_eq!(node.realisation, expected, "gate {}", gate.output);
    }

    for node in seed.primitive_nodes().iter().skip(netlist.gates.len()) {
        assert_eq!(
            node.realisation,
            NodeRealisation::Primitive(Primitive::Lever),
            "primary input {}",
            node.id
        );
    }

    for node in seed.primitive_nodes() {
        if let NodeRealisation::Primitive(primitive) = node.realisation {
            assert!(
                !physical::variants(primitive).is_empty(),
                "{} realises as {primitive:?}, which has no physical variant to emit",
                node.id
            );
        }
    }
}

/// Report every cell where two worlds disagree, capped so a wholesale
/// mismatch stays readable.
fn assert_worlds_identical(realised: &World, expected: &World, what: &str) {
    assert_eq!(realised.size(), expected.size(), "{what}: world size");

    let mut total = 0usize;
    let mut shown = Vec::new();
    for flat in 0..realised.cells().len() {
        let (x, y, z) = realised.decode(flat);
        let (got, want) = (realised.get(x, y, z), expected.get(x, y, z));
        if got != want {
            total += 1;
            if shown.len() < 12 {
                shown.push(format!("  ({x}, {y}, {z}): got {:?}, want {:?}", got.kind, want.kind));
            }
        }
    }

    assert!(
        total == 0,
        "{what}: {total} cell(s) differ; first {}:\n{}",
        shown.len(),
        shown.join("\n")
    );
}

/// The acceptance test for making the planner load-bearing: a candidate has
/// to be a complete physical realisation, not a sketch only the legacy
/// emitter can finish. A seed is the one candidate whose correct realisation
/// is already known, so it is the only honest oracle available before any
/// candidate has been moved.
///
/// Byte equality, not "close enough": every block, in every cell, including
/// the floors the emitter lays under cells it then leaves empty. Those are
/// not derivable from a finished world, which is why they are recorded
/// rather than inferred.
///
/// Run over several shapes deliberately -- a lone NOR, a bare merge, a
/// fanout, and a real multi-row circuit -- because a candidate that only
/// round-trips and4 has proved nothing about merges or fanout.
#[test]
fn a_legacy_seed_re_emits_the_exact_world_the_legacy_compiler_built() {
    let (and4, _) = build_and4_netlist();
    let circuits: [(&str, Netlist); 5] = [
        ("not", not_netlist()),
        ("and", and_netlist()),
        ("bare merge", bare_merge_netlist()),
        ("fanout", fanout_netlist()),
        ("and4", and4),
    ];

    for (name, netlist) in circuits {
        let compiled = compile(&netlist).expect("every fixture compiles");
        let seed =
            seed_from_legacy(&netlist, &compiled).expect("legacy output must be extractable");
        let realised = emit_candidate(&seed, &netlist, compiled.world.size())
            .expect("a legacy seed must be fully realisable");

        assert_worlds_identical(&realised.world, &compiled.world, name);
    }
}

/// The first half of realisation: a candidate's anchors, on their own, must
/// be able to put the primitives back into a world. Every block this writes
/// has to be the block the legacy emitter wrote at the same coordinate --
/// anything else means the candidate lost information the emitter had.
///
/// Routes are deliberately not emitted yet, so this checks containment, not
/// equality: what the primitive pass writes is a subset of the legacy world.
#[test]
fn a_seed_re_emits_every_primitive_exactly_where_the_legacy_emitter_put_it() {
    let (netlist, compiled) = compiled_and4();

    let seed = seed_from_legacy(&netlist, &compiled).expect("legacy output must be extractable");
    let realised = emit_primitives(&seed, &netlist, compiled.world.size())
        .expect("a legacy seed must be realisable")
        .world;

    let mut written = 0usize;
    for flat in 0..realised.cells().len() {
        let (x, y, z) = realised.decode(flat);
        let block = realised.get(x, y, z);
        if block.kind == BlockKind::Air {
            continue;
        }
        written += 1;
        assert_eq!(
            block,
            compiled.world.get(x, y, z),
            "primitive realisation disagrees with the legacy world at ({x}, {y}, {z})"
        );
    }

    assert!(
        written > netlist.gates.len() + netlist.inputs.len(),
        "every gate and lever must contribute at least its own block, got {written}"
    );
}

#[test]
fn legacy_and4_extracts_to_a_legal_candidate_with_unit_seed_score() {
    let (netlist, compiled) = compiled_and4();

    let seed = seed_from_legacy(&netlist, &compiled).expect("legacy output must be extractable");

    verify_candidate(&seed, &netlist).expect("the extracted candidate must retain legacy legality");
    assert_eq!(
        seed.score(&PlannerWeights::default())
            .expect("and4 seed score must fit the exact representation"),
        NormalisedScore::ONE
    );
}

#[test]
fn extracted_candidate_preserves_each_primitive_anchor_and_route_owner() {
    let (netlist, compiled) = compiled_and4();

    let seed = seed_from_legacy(&netlist, &compiled).expect("legacy output must be extractable");

    assert_eq!(
        seed.anchors(),
        &[
            Anchor { x: 14, y: 1, z: 38 },
            Anchor { x: 28, y: 1, z: 38 },
            Anchor { x: 42, y: 1, z: 38 },
            Anchor { x: 26, y: 1, z: 27 },
            Anchor { x: 28, y: 1, z: 16 },
            Anchor { x: 56, y: 1, z: 38 },
            Anchor { x: 26, y: 1, z: 5 },
            Anchor { x: 12, y: 1, z: 49 },
            Anchor { x: 26, y: 1, z: 49 },
            Anchor { x: 40, y: 1, z: 49 },
            Anchor { x: 54, y: 1, z: 49 },
        ],
        "and4's legacy seed must preserve each emitter-selected primitive origin"
    );

    let observed = seed
        .routes()
        .iter()
        .map(|route| {
            (
                route.id(),
                route.owner(),
                route.anchors().first().copied(),
                route.anchors().last().copied(),
                route.terminal_kinds(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                "a",
                Some("a"),
                Some(Anchor { x: 12, y: 1, z: 48 }),
                Some(Anchor { x: 13, y: 1, z: 38 }),
                vec![RouteTerminalKind::RepeaterIntoSupport]
            ),
            (
                "b",
                Some("b"),
                Some(Anchor { x: 26, y: 1, z: 48 }),
                Some(Anchor { x: 27, y: 1, z: 38 }),
                vec![RouteTerminalKind::RepeaterIntoSupport]
            ),
            (
                "c",
                Some("c"),
                Some(Anchor { x: 40, y: 1, z: 48 }),
                Some(Anchor { x: 41, y: 1, z: 38 }),
                vec![RouteTerminalKind::RepeaterIntoSupport]
            ),
            (
                "d",
                Some("d"),
                Some(Anchor { x: 54, y: 1, z: 48 }),
                Some(Anchor { x: 55, y: 1, z: 38 }),
                vec![RouteTerminalKind::RepeaterIntoSupport]
            ),
            (
                "g0",
                Some("g0"),
                Some(Anchor { x: 14, y: 1, z: 36 }),
                Some(Anchor { x: 25, y: 1, z: 27 }),
                vec![RouteTerminalKind::DirectedDustIntoSupport]
            ),
            (
                "g1",
                Some("g1"),
                Some(Anchor { x: 28, y: 1, z: 36 }),
                Some(Anchor { x: 27, y: 1, z: 27 }),
                vec![RouteTerminalKind::DirectedDustIntoSupport]
            ),
            (
                "g2",
                Some("g2"),
                Some(Anchor { x: 42, y: 1, z: 36 }),
                Some(Anchor { x: 26, y: 3, z: 33 }),
                vec![RouteTerminalKind::DirectedDustIntoSupport]
            ),
            (
                "g3",
                Some("g3"),
                Some(Anchor { x: 26, y: 1, z: 25 }),
                Some(Anchor { x: 27, y: 1, z: 16 }),
                vec![RouteTerminalKind::DirectedDustIntoSupport]
            ),
            (
                "g4",
                Some("g4"),
                Some(Anchor { x: 28, y: 1, z: 14 }),
                Some(Anchor { x: 25, y: 1, z: 5 }),
                vec![RouteTerminalKind::DirectedDustIntoSupport]
            ),
            (
                "g5",
                Some("g5"),
                Some(Anchor { x: 56, y: 1, z: 36 }),
                Some(Anchor { x: 30, y: 3, z: 11 }),
                vec![RouteTerminalKind::DirectedDustIntoSupport]
            ),
        ],
        "route owners, coverage endpoints, and terminal choices are explicit legacy facts"
    );
}

#[test]
fn extracted_bare_merge_routes_identify_their_merge_sink_and_terminal_style() {
    let netlist = bare_merge_netlist();
    let compiled = compile(&netlist).expect("private merge branches must compile");
    let seed = seed_from_legacy(&netlist, &compiled).expect("compiled merge must seed");

    assert_eq!(
        seed.routes()
            .iter()
            .map(|route| (route.id(), route.owner(), route.terminal_kinds()))
            .collect::<Vec<_>>(),
        vec![
            ("a", Some("a"), vec![RouteTerminalKind::BareMergeDust]),
            ("b", Some("b"), vec![RouteTerminalKind::BareMergeDust]),
        ],
        "a private merge branch terminates at the merge dust, not a NOR-support repeater"
    );
}

#[test]
fn extracted_fanout_terminal_metadata_keeps_each_sink_identity() {
    let netlist = fanout_netlist();
    let compiled = compile(&netlist).expect("fanout fixture must compile");
    let seed = seed_from_legacy(&netlist, &compiled).expect("compiled fanout must seed");
    let route = seed
        .routes()
        .iter()
        .find(|route| route.id() == "a")
        .expect("input edge must exist");

    let mut sinks = route
        .terminals()
        .iter()
        .map(|terminal| {
            (
                terminal.sink.gate.as_str(),
                terminal.sink.input_index,
                terminal.sink.anchor,
            )
        })
        .collect::<Vec<_>>();
    sinks.sort_unstable_by_key(|(gate, input_index, _)| (*gate, *input_index));
    assert_eq!(
        sinks.iter().map(|(gate, input_index, _)| (*gate, *input_index)).collect::<Vec<_>>(),
        vec![("left", 0), ("right", 0)],
        "fanout terminals carry declared sink identities instead of relying on an internal flattening order"
    );
    assert!(
        sinks
            .iter()
            .all(|(_, _, anchor)| route.anchors().contains(anchor)),
        "each identified fanout sink terminates at one of its own edge cells"
    );
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
        assert_eq!(
            output, expected,
            "NOT({a}) should be {expected}, got {output}"
        );
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
        assert_eq!(
            output, expected,
            "AND({a}, {b}) should be {expected}, got {output}"
        );
    }
}

#[test]
fn a_compiled_circuit_saves_to_a_loadable_litematic() {
    let netlist = and_netlist();
    let compiled = compile(&netlist).expect("this netlist is acyclic and fully driven");

    let mut path = PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string()),
    );
    path.push("reda_compile_end_to_end_and_gate.litematic");

    litematic::save(&path, &compiled.world, "and_gate").expect("saving must succeed");
    let loaded = litematic::load(&path).expect("loading must succeed");

    assert_eq!(
        loaded.size(),
        compiled.world.size(),
        "loaded world must have the same dimensions"
    );

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

    assert_eq!(
        first.world.size(),
        second.world.size(),
        "world size must be stable"
    );
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
            Gate {
                name: "g1".to_string(),
                inputs: vec!["loop_b".to_string()],
                output: "loop_a".to_string(),
                kind: GateKind::Nor(1),
            },
            Gate {
                name: "g2".to_string(),
                inputs: vec!["loop_a".to_string()],
                output: "loop_b".to_string(),
                kind: GateKind::Nor(1),
            },
        ],
    };

    let result = compile(&netlist);

    assert_eq!(result.err(), Some(CompileError::CyclicNetlist));
}
