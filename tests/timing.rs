//! Tests for `reda::timing`: dynamic timing analysis built on the
//! simulator's optional observer.
//!
//! Every measurement here has an expected value derived independently of the
//! implementation -- worked out by hand from the redstone rules themselves
//! (torch delay = 2 game ticks, repeater delay = 2 game ticks per redstone
//! tick of its setting, dust adds none), not read off what the code produces.
//! See `docs/superpowers/specs/2026-08-07-timing-analysis.md`.

use std::collections::BTreeMap;

use reda::circuits::and4::build_and4_netlist;
use reda::circuits::and4::INPUT_NAMES as AND4_INPUT_NAMES;
use reda::compile::compile;
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Facing};
use reda::redstone::world::storage::World;
use reda::timing::{measure_transition, watch_all_nets};

const MAX_TICKS: u64 = 200;

fn named(name: &str, kind: BlockKind) -> BlockState {
    let mut state = BlockState::air();
    state.kind = kind;
    state.name = name.to_string();
    state
}

fn lever(lit: bool) -> BlockState {
    let mut state = named("minecraft:lever", BlockKind::Lever);
    state.lit = lit;
    state
}

fn stone() -> BlockState {
    named("minecraft:stone", BlockKind::Solid)
}

fn dust() -> BlockState {
    named("minecraft:redstone_wire", BlockKind::RedstoneWire)
}

fn standing_torch() -> BlockState {
    named("minecraft:redstone_torch", BlockKind::Torch)
}

fn wall_torch(facing: Facing) -> BlockState {
    let mut state = named("minecraft:redstone_wall_torch", BlockKind::WallTorch);
    state.facing = Some(facing);
    state
}

fn repeater(facing: Facing, delay: u8) -> BlockState {
    let mut state = named("minecraft:repeater", BlockKind::Repeater);
    state.facing = Some(facing);
    state.delay = delay;
    state
}

fn flip_lever(simulator: &mut Simulator, pos: Position, on: bool) {
    simulator.world_mut().set(pos.x, pos.y, pos.z, lever(on));
}

// ---------------------------------------------------------------------
// Test 1: a straight chain of N inverters has a hand-computed settle time
// and per-net arrival schedule.
// ---------------------------------------------------------------------

/// Build a vertical chain of `n` standing torches, each one inverting the
/// one below it: `lever -> support0 -> torch0 -> support1 (torch0's own
/// strongly-powered "up" neighbour) -> torch1 -> support2 -> torch2 -> ...`.
///
/// Every hop is a direct block adjacency -- no wire run, no repeater -- so
/// the delay is exactly `TORCH_DELAY_GAME_TICKS` (2 game ticks) per torch,
/// applied strictly in series: torch `k` cannot react until torch `k - 1`
/// has already flipped. A chain of `n` torches therefore takes exactly `2n`
/// game ticks to settle after the lever flips, and torch `k` (0-indexed)
/// arrives at tick `2 * (k + 1)`.
///
/// Returns the lever's position and each torch's position, in chain order.
fn build_inverter_chain(n: usize) -> (World, Position, Vec<Position>) {
    let mut world = World::new(3, (2 * n + 2) as i32, 3);
    let lever_pos = Position::new(1, 0, 1);
    world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

    let mut support = lever_pos; // lever(false) also strongly powers its neighbours, same as any
                                  // active component -- the first support sits directly on top of it.
    let mut torches: Vec<Position> = Vec::with_capacity(n);
    for _ in 0..n {
        support = support.up();
        world.set(support.x, support.y, support.z, stone());
        let torch = support.up();
        // Self-consistent initial state: `lit` must already match what
        // `torch_should_be_lit` would compute, or `Simulator::new` would
        // start with pending work before the lever ever moves. Alternate
        // starting from `true` (lever off -> unpowered support -> lit) so
        // each successive support (the previous torch's own "up" neighbour)
        // is consistent with the one below it.
        let is_first = torches.is_empty();
        let initially_lit = if is_first {
            true // support (on top of the off lever) starts unpowered
        } else {
            !world.get(torches[torches.len() - 1].x, torches[torches.len() - 1].y, torches[torches.len() - 1].z).lit
        };
        world.set(torch.x, torch.y, torch.z, {
            let mut t = standing_torch();
            t.lit = initially_lit;
            t
        });
        torches.push(torch);
        support = torch;
    }

    (world, lever_pos, torches)
}

#[test]
fn a_chain_of_inverters_settles_in_exactly_two_ticks_per_torch() {
    const N: usize = 5;
    let (world, lever_pos, torches) = build_inverter_chain(N);

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("the chain must already be self-consistent");

    let watched: Vec<(Position, String)> =
        torches.iter().enumerate().map(|(i, &p)| (p, format!("torch{i}"))).collect();
    simulator.attach_observer(watched);

    let result = measure_transition(&mut simulator, MAX_TICKS, |sim| {
        flip_lever(sim, lever_pos, true);
    })
    .expect("an acyclic chain of inverters must settle");

    assert_eq!(
        result.settle_game_ticks,
        2 * N as u64,
        "N torches in series, 2 game ticks each, driven strictly in order"
    );

    for (i, _) in torches.iter().enumerate() {
        let net = result.nets.get(&format!("torch{i}")).unwrap_or_else(|| {
            panic!("torch{i} must have changed -- every torch in the chain flips exactly once")
        });
        assert_eq!(
            net.arrival_tick(),
            Some(2 * (i as u64 + 1)),
            "torch{i} is the ({}th) hop, so it must arrive at 2*(i+1) game ticks",
            i + 1
        );
        assert!(!net.glitched(), "a pure ripple chain has no reconvergent paths, so no torch should glitch");
        assert_eq!(net.change_count(), 1);
    }
}

// ---------------------------------------------------------------------
// Test 2: two paths of different length into the same NOR gate produce a
// hand-computed glitch, with a hand-computed settle time.
// ---------------------------------------------------------------------

/// Builds `out = NOR(a, b)` where `a = NOT(lever)` (one inversion, one
/// repeater) and `b = NOT(NOT(lever))` (two inversions, one repeater with a
/// longer setting) -- the textbook static-hazard shape: the *steady-state*
/// value of `out` never changes (`NOR(NOT(x), x)` is always false), but a
/// transition briefly shows the wrong value while the short path has already
/// dropped and the long path has not yet arrived.
///
/// Every component here is placed so it reads its input via direct
/// adjacency (a torch's own weak horizontal power, or a repeater's rear
/// reading a dust cell) or a short dust run -- dust adds no delay in this
/// simulator, only torches (2 game ticks) and repeaters (2 game ticks per
/// redstone tick of delay) do. That is what makes every tick below
/// derivable by hand instead of read off a run:
///
/// ```text
/// t=0  lever flips 0 -> 1
/// t=2  torch_a flips (a's first inversion); torch_b1 flips (b's first inversion)
/// t=4  repeater_a flips off  => a: 1 -> 0        (delay = 1 redstone tick = 2gt, scheduled at t=2)
///      torch_b2 flips        => b's 2nd inversion (torch delay, scheduled at t=2)
/// t=6  out flips ON  (glitch: a=0 and b=0 -- both low -- so NOR(a,b)=1)
///                                                (out scheduled from settle at t=4)
/// t=8  repeater_b flips on  => b: 0 -> 1        (delay = 2 redstone ticks = 4gt, scheduled at t=4)
/// t=10 out flips OFF (corrected: NOR(0,1)=0)    (out scheduled from settle at t=8)
/// ```
///
/// Total settle: 10 game ticks. `out` changes twice (a glitch); every other
/// watched net changes exactly once.
struct GlitchCircuit {
    world: World,
    lever_pos: Position,
    torch_a: Position,
    repeater_a: Position,
    torch_b1: Position,
    torch_b2: Position,
    repeater_b: Position,
    /// The output torch, standing on the merge block both repeaters drive.
    /// This is the watched "out" net -- not to be confused with the merge
    /// block itself (`out.down()`), which the repeaters actually power.
    out: Position,
}

fn build_glitch_circuit() -> GlitchCircuit {
    let mut world = World::new(9, 4, 6);

    let lever_pos = Position::new(2, 1, 2);
    let support_a = Position::new(3, 1, 2); // east of lever
    let support_b1 = Position::new(2, 1, 3); // south of lever

    let torch_a = Position::new(4, 1, 2); // east of support_a; attached West
    let torch_b1 = Position::new(2, 1, 4); // south of support_b1; attached North
    let support_b2 = Position::new(3, 1, 4); // east of torch_b1
    let torch_b2 = Position::new(4, 1, 4); // east of support_b2; attached West

    // Path A reads torch_a directly -- a torch drives its horizontal
    // neighbours regardless of weak/strong, so no dust is needed here.
    let repeater_a = Position::new(5, 1, 2); // east of torch_a, facing East

    // Path B needs to reach the merge block from a different row, so it
    // bridges the gap with two cells of dust (dust adds no delay, only a
    // decaying strength, which does not matter for a boolean signal).
    let dust_b: [Position; 2] = [Position::new(5, 1, 4), Position::new(6, 1, 4)];
    let repeater_b = Position::new(6, 1, 3); // north of dust_b's last cell, facing North

    let merge = Position::new(6, 1, 2); // east of repeater_a, south of repeater_b
    let out = merge.up(); // the output torch stands on the merge block

    // Lever off: nothing is powered yet. Initial `lit`/`power` values are
    // deliberately left at their defaults (torches/repeaters off, dust at 0)
    // -- an initial `run_until_stable` call (done by the caller) settles
    // them into the correct steady state before anything is measured, so
    // hand-picking self-consistent initial values is unnecessary here
    // (unlike the pure-inverter-chain test, which has no such warm-up step).
    world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));
    world.set(support_a.x, support_a.y, support_a.z, stone());
    world.set(support_b1.x, support_b1.y, support_b1.z, stone());

    world.set(torch_a.x, torch_a.y, torch_a.z, wall_torch(Facing::East));
    world.set(torch_b1.x, torch_b1.y, torch_b1.z, wall_torch(Facing::South));
    world.set(support_b2.x, support_b2.y, support_b2.z, stone());
    world.set(torch_b2.x, torch_b2.y, torch_b2.z, wall_torch(Facing::East));

    world.set(repeater_a.x, repeater_a.y, repeater_a.z, repeater(Facing::East, 1));

    for &d in &dust_b {
        world.set(d.x, d.y, d.z, dust());
    }
    world.set(repeater_b.x, repeater_b.y, repeater_b.z, repeater(Facing::North, 2));

    world.set(merge.x, merge.y, merge.z, stone());
    world.set(out.x, out.y, out.z, standing_torch());

    // Sanity-check the geometry this whole derivation depends on, so a typo
    // in a coordinate fails loudly here instead of producing a silently
    // wrong tick count below.
    assert_eq!(repeater_a.offset(Facing::East), merge, "repeater_a must face directly into the merge block");
    assert_eq!(repeater_b.offset(Facing::North), merge, "repeater_b must face directly into the merge block");
    assert_eq!(*dust_b.last().unwrap(), repeater_b.offset(Facing::South), "repeater_b must read the last dust_b cell");
    assert_eq!(merge.up(), out, "the output torch must stand directly on the merge block");

    GlitchCircuit { world, lever_pos, torch_a, repeater_a, torch_b1, torch_b2, repeater_b, out }
}

#[test]
fn two_paths_of_different_length_glitch_exactly_as_hand_computed() {
    let circuit = build_glitch_circuit();
    let mut simulator = Simulator::new(circuit.world);
    simulator.run_until_stable(MAX_TICKS).expect("the circuit must settle to its lever-off steady state");

    assert!(
        !simulator.world().get(circuit.out.x, circuit.out.y, circuit.out.z).lit,
        "steady state: NOR(NOT(0), 0) = NOR(1, 0) = 0, `out` must be dark"
    );

    let watched = vec![
        (circuit.torch_a, "a_inv".to_string()),
        (circuit.repeater_a, "a".to_string()),
        (circuit.torch_b1, "b_inv1".to_string()),
        (circuit.torch_b2, "b_inv2".to_string()),
        (circuit.repeater_b, "b".to_string()),
        (circuit.out, "out".to_string()),
    ];
    simulator.attach_observer(watched);

    let result = measure_transition(&mut simulator, MAX_TICKS, |sim| {
        flip_lever(sim, circuit.lever_pos, true);
    })
    .expect("this is a feedforward circuit -- it must settle");

    assert_eq!(result.settle_game_ticks, 10, "see the hand-derived timeline in build_glitch_circuit's doc comment");

    let arrival = |label: &str| result.nets.get(label).map(|n| n.arrival_tick());
    assert_eq!(arrival("a_inv"), Some(Some(2)));
    assert_eq!(arrival("b_inv1"), Some(Some(2)));
    assert_eq!(arrival("a"), Some(Some(4)));
    assert_eq!(arrival("b_inv2"), Some(Some(4)));
    assert_eq!(arrival("b"), Some(Some(8)));
    assert_eq!(arrival("out"), Some(Some(10)));

    let out = &result.nets["out"];
    assert_eq!(
        out.changes(),
        &[(6, true), (10, false)],
        "`out` must glitch on at t=6 (a already dropped, b not yet arrived) and correct at t=10"
    );
    assert!(out.glitched(), "two changes to settle to the same final value is exactly a glitch");
    assert_eq!(out.change_count(), 2);

    for label in ["a_inv", "b_inv1", "a", "b_inv2", "b"] {
        let net = &result.nets[label];
        assert!(!net.glitched(), "{label} is on a single, non-reconvergent path and must not glitch");
        assert_eq!(net.change_count(), 1);
    }

    assert!(
        !simulator.world().get(circuit.out.x, circuit.out.y, circuit.out.z).lit,
        "final state must match the unchanged steady-state value"
    );
}

// ---------------------------------------------------------------------
// Test 3: attaching an observer must not change simulator behaviour.
// ---------------------------------------------------------------------

#[test]
fn attaching_an_observer_does_not_change_settle_ticks_or_final_state() {
    let (netlist, output_signal) = build_and4_netlist();

    let run = |with_observer: bool| -> (Vec<u64>, Vec<bool>) {
        let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
        let lever_positions: BTreeMap<&str, (i32, i32, i32)> = AND4_INPUT_NAMES
            .iter()
            .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
            .collect();
        let output_position = *compiled.output_positions.get(&output_signal).unwrap();
        let watched = watch_all_nets(&compiled);

        let mut simulator = Simulator::new(compiled.world);
        simulator.run_until_stable(MAX_TICKS).expect("must settle before the first reading");

        if with_observer {
            simulator.attach_observer(watched);
        }

        let mut tick_counts = Vec::new();
        let mut outputs = Vec::new();
        for combination in 0u8..16 {
            let bits = [
                (combination >> 3) & 1,
                (combination >> 2) & 1,
                (combination >> 1) & 1,
                combination & 1,
            ];
            for (&name, &bit) in AND4_INPUT_NAMES.iter().zip(bits.iter()) {
                let (x, y, z) = lever_positions[name];
                let pos = Position::new(x, y, z);
                simulator.world_mut().set(pos.x, pos.y, pos.z, lever(bit == 1));
                let ticks = simulator.run_until_stable(MAX_TICKS).expect("must settle");
                tick_counts.push(ticks);
            }
            let (ox, oy, oz) = output_position;
            outputs.push(simulator.world().get(ox, oy, oz).lit);
        }
        (tick_counts, outputs)
    };

    let (ticks_without, outputs_without) = run(false);
    let (ticks_with, outputs_with) = run(true);

    assert_eq!(ticks_without, ticks_with, "attaching an observer must not change any settle tick count");
    assert_eq!(outputs_without, outputs_with, "attaching an observer must not change the final output sequence");
}
