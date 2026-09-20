//! Contract composition slices: independently certify one-input `NOR` children,
//! join adjacent handovers through parent-owned portals along one axis, then
//! prove the composed world computes an even number of inversions (`a`).

use std::thread;

use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
use reda::compile::planner::{Anchor, PortPlacements};
use reda::compile::{drive_caller_cell, probe_caller_cell, Gate, Netlist};
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Facing};
use reda::redstone::world::storage::World;

const ROOT_IN: (i32, i32, i32) = (10, 1, 40);
/// Fixed x-axis spacing between consecutive boundary cells of a pipeline.
const SPACING: i32 = 10;

/// Boundary cell `index` of a pipeline: cell 0 is the root input, cell `n`
/// the root output, everything between is a parent-owned portal.
fn boundary(index: usize) -> (i32, i32, i32) {
    (ROOT_IN.0 + SPACING * index as i32, ROOT_IN.1, ROOT_IN.2)
}

/// Stage names for an `n`-stage pipeline: `a`, `s1`, ..., `s{n-1}`, `y`.
fn stage_names(n: usize) -> Vec<String> {
    (0..=n)
        .map(|i| match i {
            0 => "a".to_owned(),
            i if i == n => "y".to_owned(),
            i => format!("s{i}"),
        })
        .collect()
}

struct Child {
    netlist: Netlist,
    pins: PortPlacements,
    input: String,
    output: String,
}

struct ChildResult {
    world: World,
    input: (i32, i32, i32),
    output: (i32, i32, i32),
    case: String,
    candidate: String,
    blocks: Vec<((i32, i32, i32), BlockState)>,
}

fn anchor((x, y, z): (i32, i32, i32)) -> Anchor {
    Anchor { x, y, z }
}

fn child(
    input: &str,
    output: &str,
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
        input: input.into(),
        output: output.into(),
    }
}

/// `n` chained children; child `i` reads boundary `i` and writes boundary
/// `i + 1`. At every internal portal the producer delivers on the north side
/// and the consumer reads on the south side, so the handovers sit on opposite
/// faces of the shared parent-owned cell. The root input instead reads north.
fn pipeline(n: usize) -> Vec<Child> {
    let names = stage_names(n);
    names
        .windows(2)
        .enumerate()
        .map(|(i, pair)| {
            let input_toward = if i == 0 { Facing::North } else { Facing::South };
            child(
                &pair[0],
                &pair[1],
                boundary(i),
                input_toward,
                boundary(i + 1),
                Facing::South,
            )
        })
        .collect()
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
    assert_eq!(
        result.evaluations_used, 0,
        "this test certifies seed layouts"
    );
    let case = result.case_fingerprint.as_str().to_owned();
    let candidate = result.candidate_fingerprint.as_str().to_owned();
    let compiled = result.compiled;
    ChildResult {
        input: compiled.input_positions[child.input.as_str()],
        output: compiled.output_positions[child.output.as_str()],
        case,
        blocks: cells(&compiled.world),
        candidate,
        world: compiled.world,
    }
}

fn merge(worlds: &[&World], minimum_size: (i32, i32, i32)) -> World {
    let size = worlds.iter().fold(minimum_size, |acc, world| {
        let s = world.size();
        (acc.0.max(s.0), acc.1.max(s.1), acc.2.max(s.2))
    });
    let mut merged = World::new(size.0, size.1, size.2);
    for (child, world) in worlds.iter().enumerate() {
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
    assert!(
        world.index(at.0, at.1, at.2).is_some(),
        "handover {at:?} is out of bounds"
    );
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

fn assert_air(world: &World, at: (i32, i32, i32)) {
    assert!(
        world.index(at.0, at.1, at.2).is_some(),
        "air cell {at:?} is out of bounds"
    );
    assert_eq!(world.get(at.0, at.1, at.2).kind, BlockKind::Air);
}

fn assert_portal(portal: (i32, i32, i32), first: &World, second: &World, merged: &World) {
    let delivery = step(portal, Facing::North);
    let reader = step(portal, Facing::South);
    assert_repeater(first, delivery, Facing::North);
    assert_repeater(second, reader, Facing::North);
    for facing in [Facing::East, Facing::West, Facing::Up, Facing::Down] {
        assert_air(merged, step(portal, facing));
    }
}

fn assert_terminal(world: &World, at: (i32, i32, i32), handover: Facing) {
    assert_air(world, at);
    for facing in [
        Facing::North,
        Facing::South,
        Facing::East,
        Facing::West,
        Facing::Up,
        Facing::Down,
    ] {
        if facing != handover {
            assert_air(world, step(at, facing));
        }
    }
}

fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".into();
    state
}

/// Merge chained children in stable order, certify every portal, then seal
/// each portal with an inert solid block.
fn compose(children: &[ChildResult]) -> World {
    for (i, first) in children.iter().enumerate() {
        for second in &children[i + 1..] {
            assert_separated(first, second);
        }
    }
    let worlds: Vec<&World> = children.iter().map(|c| &c.world).collect();
    let root_out = boundary(children.len());
    let mut world = merge(&worlds, (root_out.0 + 2, root_out.1 + 2, root_out.2 + 2));
    for (i, pair) in children.windows(2).enumerate() {
        let portal = boundary(i + 1);
        assert_eq!((pair[0].output, pair[1].input), (portal, portal));
        assert_portal(portal, &pair[0].world, &pair[1].world, &world);
        assert!(world.index(portal.0, portal.1, portal.2).is_some());
        world.set(portal.0, portal.1, portal.2, stone());
        assert_eq!(world.get(portal.0, portal.1, portal.2), &stone());
    }
    world
}

/// Drive the root input low/high/low and assert the root output tracks it.
fn assert_identity(mut world: World, root_out: (i32, i32, i32), stages: usize) {
    let tick_limit = 500.max(64 * stages as u64);
    probe_caller_cell(&mut world, root_out);
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(tick_limit).unwrap();
    assert!(
        !simulator
            .world()
            .get(root_out.0, root_out.1, root_out.2)
            .lit
    );
    for value in [true, false] {
        drive_caller_cell(simulator.world_mut(), ROOT_IN, value);
        simulator.run_until_stable(tick_limit).unwrap();
        assert_eq!(
            simulator
                .world()
                .get(root_out.0, root_out.1, root_out.2)
                .lit,
            value
        );
    }
}

/// Compile `n` seed layouts concurrently and sequentially, certify their flat
/// composition, and prove the even number of inversions is the identity.
fn run_pipeline(n: usize) {
    assert_eq!(n % 2, 0, "an identity pipeline needs an even stage count");
    let children = pipeline(n);
    let root_out = boundary(n);

    let concurrent: Vec<ChildResult> = thread::scope(|scope| {
        let handles: Vec<_> = children
            .iter()
            .map(|child| scope.spawn(move || solve(child)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let sequential: Vec<ChildResult> = children.iter().map(solve).collect();

    let mut cases: Vec<&str> = concurrent.iter().map(|child| child.case.as_str()).collect();
    cases.sort_unstable();
    cases.dedup();
    assert_eq!(
        cases.len(),
        n,
        "each stage must be a distinct synthesis case"
    );

    for (i, (fast, slow)) in concurrent.iter().zip(&sequential).enumerate() {
        assert_eq!((fast.input, fast.output), (boundary(i), boundary(i + 1)));
        assert_eq!(
            (&fast.candidate, &fast.blocks),
            (&slow.candidate, &slow.blocks),
            "child {i} must certify identically under concurrency"
        );
        for cell in [boundary(i), boundary(i + 1)] {
            assert_eq!(fast.world.get(cell.0, cell.1, cell.2).kind, BlockKind::Air);
        }
    }

    let world = compose(&concurrent);
    assert_terminal(&world, ROOT_IN, Facing::North);
    assert_terminal(&world, root_out, Facing::North);
    assert_eq!(
        cells(&world),
        cells(&compose(&sequential)),
        "concurrent and sequential root compositions must be identical"
    );
    assert_identity(world, root_out, n);
}

#[test]
fn two_parallel_children_compose_through_one_parent_portal() {
    run_pipeline(2);
}

#[test]
fn sixteen_parallel_seed_children_compose_through_fifteen_parent_portals() {
    run_pipeline(16);
}
