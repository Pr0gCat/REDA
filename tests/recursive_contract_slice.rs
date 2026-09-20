//! Minimal two-child contract slice: independently certify `NOT` children,
//! join their opposite handovers through one parent-owned portal, then prove
//! the composed world computes `NOT(NOT(a)) = a`.

use std::thread;

use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
use reda::compile::planner::{Anchor, PortPlacements};
use reda::compile::{drive_caller_cell, probe_caller_cell, Gate, Netlist};
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Facing};
use reda::redstone::world::storage::World;

const ROOT_IN: (i32, i32, i32) = (10, 1, 40);
const PORTAL: (i32, i32, i32) = (20, 1, 40);
const ROOT_OUT: (i32, i32, i32) = (30, 1, 40);

struct Child {
    netlist: Netlist,
    pins: PortPlacements,
    input: &'static str,
    output: &'static str,
}

struct ChildResult {
    world: World,
    input: (i32, i32, i32),
    output: (i32, i32, i32),
    candidate: String,
    blocks: Vec<((i32, i32, i32), BlockState)>,
}

fn anchor((x, y, z): (i32, i32, i32)) -> Anchor {
    Anchor { x, y, z }
}

fn child(
    input: &'static str,
    output: &'static str,
    input_at: (i32, i32, i32),
    input_toward: Facing,
    output_at: (i32, i32, i32),
    output_toward: Facing,
) -> Child {
    let mut pins = PortPlacements::default();
    pins.pin(input, anchor(input_at), input_toward);
    pins.pin(output, anchor(output_at), output_toward);
    Child {
        netlist: Netlist {
            inputs: vec![input.into()],
            outputs: vec![output.into()],
            gates: vec![Gate::nor(output, &[input])],
        },
        pins,
        input,
        output,
    }
}

fn cells(world: &World) -> Vec<((i32, i32, i32), BlockState)> {
    let (sx, sy, sz) = world.size();
    let mut cells = Vec::new();
    for z in 0..sz {
        for y in 0..sy {
            for x in 0..sx {
                let state = world.get(x, y, z);
                if state.kind != BlockKind::Air {
                    cells.push(((x, y, z), state.clone()));
                }
            }
        }
    }
    cells
}

fn solve(child: &Child) -> ChildResult {
    let result = compile_fragment_synth(
        SynthesisInput {
            lowered: &child.netlist,
            source_provenance: None,
            pins: Some(&child.pins),
        },
        SynthesisBudget::Evaluations(0),
    )
    .expect("a pinned one-input NOR must certify");
    let candidate = result.candidate_fingerprint.as_str().to_owned();
    let compiled = result.compiled;
    ChildResult {
        input: compiled.input_positions[child.input],
        output: compiled.output_positions[child.output],
        blocks: cells(&compiled.world),
        candidate,
        world: compiled.world,
    }
}

fn merge(first: &World, second: &World) -> World {
    let a = first.size();
    let b = second.size();
    let mut merged = World::new(a.0.max(b.0), a.1.max(b.1), a.2.max(b.2));
    for (child, world) in [first, second].into_iter().enumerate() {
        for ((x, y, z), incoming) in cells(world) {
            assert_eq!(
                merged.get(x, y, z).kind,
                BlockKind::Air,
                "child {child} overlaps another child at ({x}, {y}, {z})"
            );
            merged.set(x, y, z, incoming);
        }
    }
    merged
}

fn assert_separated(first: &ChildResult, second: &ChildResult) {
    for (mine, _) in &first.blocks {
        for (other, _) in &second.blocks {
            let gap = (mine.0 - other.0)
                .abs()
                .max((mine.1 - other.1).abs())
                .max((mine.2 - other.2).abs());
            assert!(
                gap >= 2,
                "child cells {mine:?} and {other:?} violate the empty halo"
            );
        }
    }
}

fn step(at: (i32, i32, i32), facing: Facing) -> (i32, i32, i32) {
    let at = Position::new(at.0, at.1, at.2).offset(facing);
    (at.x, at.y, at.z)
}

fn assert_repeater(world: &World, at: (i32, i32, i32), facing: Facing) {
    let state = world.get(at.0, at.1, at.2);
    assert_eq!(
        state.kind,
        BlockKind::Repeater,
        "missing handover at {at:?}"
    );
    assert_eq!(
        state.facing,
        Some(facing),
        "wrong handover facing at {at:?}"
    );
}

fn assert_portal(first: &World, second: &World, merged: &World) {
    let delivery = step(PORTAL, Facing::North);
    let reader = step(PORTAL, Facing::South);
    assert_repeater(first, delivery, Facing::North);
    assert_repeater(second, reader, Facing::North);
    assert_eq!(
        second.get(delivery.0, delivery.1, delivery.2).kind,
        BlockKind::Air
    );
    assert_eq!(first.get(reader.0, reader.1, reader.2).kind, BlockKind::Air);
    for facing in [Facing::East, Facing::West, Facing::Up, Facing::Down] {
        let at = step(PORTAL, facing);
        assert_eq!(merged.get(at.0, at.1, at.2).kind, BlockKind::Air);
    }
}

fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".into();
    state
}

fn compose(first: &ChildResult, second: &ChildResult) -> World {
    assert_separated(first, second);
    let mut world = merge(&first.world, &second.world);
    assert_portal(&first.world, &second.world, &world);
    assert!(world.index(PORTAL.0, PORTAL.1, PORTAL.2).is_some());
    world.set(PORTAL.0, PORTAL.1, PORTAL.2, stone());
    assert_eq!(world.get(PORTAL.0, PORTAL.1, PORTAL.2), &stone());
    world
}

#[test]
fn two_parallel_children_compose_through_one_parent_portal() {
    let one = child("a", "m", ROOT_IN, Facing::North, PORTAL, Facing::South);
    let two = child("m", "y", PORTAL, Facing::South, ROOT_OUT, Facing::North);
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(|| solve(&one));
        let second = scope.spawn(|| solve(&two));
        (first.join().unwrap(), second.join().unwrap())
    });

    let sequential = [solve(&one), solve(&two)];
    assert_eq!(
        (&first.candidate, &first.blocks),
        (&sequential[0].candidate, &sequential[0].blocks)
    );
    assert_eq!(
        (&second.candidate, &second.blocks),
        (&sequential[1].candidate, &sequential[1].blocks)
    );
    assert_eq!((first.input, first.output), (ROOT_IN, PORTAL));
    assert_eq!((second.input, second.output), (PORTAL, ROOT_OUT));
    assert_eq!(
        first.world.get(ROOT_IN.0, ROOT_IN.1, ROOT_IN.2).kind,
        BlockKind::Air
    );
    assert_eq!(
        second.world.get(ROOT_OUT.0, ROOT_OUT.1, ROOT_OUT.2).kind,
        BlockKind::Air
    );
    assert_eq!(
        first.world.get(PORTAL.0, PORTAL.1, PORTAL.2).kind,
        BlockKind::Air
    );
    assert_eq!(
        second.world.get(PORTAL.0, PORTAL.1, PORTAL.2).kind,
        BlockKind::Air
    );
    let mut world = compose(&first, &second);
    assert_eq!(
        cells(&world),
        cells(&compose(&sequential[0], &sequential[1])),
        "one-worker and two-worker root compositions must be identical"
    );
    probe_caller_cell(&mut world, ROOT_OUT);

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(500).unwrap();
    for value in [false, true, false] {
        drive_caller_cell(simulator.world_mut(), ROOT_IN, value);
        simulator.run_until_stable(500).unwrap();
        assert_eq!(
            simulator
                .world()
                .get(ROOT_OUT.0, ROOT_OUT.1, ROOT_OUT.2)
                .lit,
            value
        );
    }
}
