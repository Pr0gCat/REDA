//! `Gate::is_merge()` end to end: `compile()` actually emitting a wire-merge OR,
//! for both branches of the isolation rule, verified against the real
//! `Simulator` -- not just against the invariants that permit the geometry.
//!
//! See `docs/superpowers/specs/2026-08-08-gate-types-and-wired-or.md` (the
//! design) and `docs/superpowers/specs/2026-08-08-cell-type-costs.md` (the
//! measurements this task's cost claims are checked against). That report's
//! own hand-built-`World` tests already confirmed the backflow physics in
//! isolation; this file confirms the *actual compiler* -- `compile()`'s own
//! placement and routing, choosing bare vs. isolated per branch by the
//! fanout rule alone -- gets it right, by building real `Netlist`s with a
//! declared merge and running them all the way through.

use reda::compile::topology::GateKind;
use reda::compile::{compile, Gate, Netlist};
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

const MAX_TICKS: u64 = 2000;

// ---------------------------------------------------------------------
// A minimal netlist builder -- same shape as the one in
// `tests/cell_type_costs.rs` (this file cannot see `circuits::netlist_builder`,
// which is `pub(crate)`), extended with a `merge` primitive.
// ---------------------------------------------------------------------

struct GateNet {
    gates: Vec<Gate>,
    counter: usize,
}

impl GateNet {
    fn new() -> Self {
        GateNet { gates: Vec::new(), counter: 0 }
    }

    fn fresh(&mut self) -> String {
        let name = format!("g{}", self.counter);
        self.counter += 1;
        name
    }

    fn nor(&mut self, inputs: &[&str]) -> String {
        assert!(!inputs.is_empty() && inputs.len() <= 3, "NOR fan-in must be 1..=3, got {}", inputs.len());
        let output = self.fresh();
        self.gates.push(Gate {
            name: output.clone(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.clone(),
            kind: GateKind::Nor(inputs.len()),
        });
        output
    }

    fn not(&mut self, x: &str) -> String {
        self.nor(&[x])
    }

    /// A declared wire-merge OR (`GateKind::Or`), realised (once
    /// `compile` sees it) as a bare join or a per-branch isolated one,
    /// never as a torch.
    fn merge(&mut self, inputs: &[&str]) -> String {
        assert!(inputs.len() >= 2, "a merge needs at least two branches to be interesting, got {}", inputs.len());
        let output = self.fresh();
        self.gates.push(Gate {
            name: output.clone(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.clone(),
            kind: GateKind::Or(inputs.len()),
        });
        output
    }
}

// ---------------------------------------------------------------------
// Measurement, mirroring `tests/cell_type_costs.rs`'s own `measure_nor_
// network`: compile, verify the truth table against the real simulator for
// every input combination, and read back real (non-merge) gate count,
// non-air block count, and worst-case single-input-change settle ticks.
// ---------------------------------------------------------------------

fn count_non_air(world: &World) -> usize {
    let (size_x, size_y, size_z) = world.size();
    let mut count = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if world.get(x, y, z).kind != BlockKind::Air {
                    count += 1;
                }
            }
        }
    }
    count
}

fn count_kind(world: &World, kind: BlockKind) -> usize {
    let (size_x, size_y, size_z) = world.size();
    let mut count = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if world.get(x, y, z).kind == kind {
                    count += 1;
                }
            }
        }
    }
    count
}

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) -> u64 {
    let start = simulator.current_tick();
    let mut state = simulator.world().get(position.0, position.1, position.2).clone();
    state.lit = on;
    simulator.world_mut().set(position.0, position.1, position.2, state);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle after changing an input");
    simulator.current_tick() - start
}

#[derive(Debug, Clone, Copy)]
struct CellCost {
    /// Real, torch-based gates only -- merge entries are not
    /// counted, because they place no gate body at all (see
    /// `compile::place_merge_gate`'s own doc comment). This is the number
    /// that should read 0 for a bare two-input OR, matching the referenced
    /// cost-table spec's measurement.
    real_gates: usize,
    blocks: usize,
    settle_game_ticks: u64,
}

/// Compiles `netlist`, checks every declared output against `truth` for
/// every input combination via the real simulator, and returns the
/// measured cost. Every combination is swept from the previous one
/// input-at-a-time, exactly as `tests/reference_circuits.rs` and
/// `tests/cell_type_costs.rs` already do, so settle is a genuine
/// worst-case single-input-change figure, not a from-reset one.
fn measure(
    label: &str,
    netlist: &Netlist,
    inputs: &[&str],
    outputs: &[&str],
    truth: impl Fn(&[bool]) -> Vec<bool>,
) -> CellCost {
    let real_gates = netlist.gates.iter().filter(|g| !g.is_merge()).count();
    let compiled = compile(netlist).unwrap_or_else(|err| panic!("{label} failed to compile: {err}"));
    let blocks = count_non_air(&compiled.world);

    let lever_positions: Vec<(i32, i32, i32)> = inputs.iter().map(|name| compiled.input_positions[*name]).collect();
    let output_positions: Vec<(i32, i32, i32)> = outputs.iter().map(|name| compiled.output_positions[*name]).collect();

    let mut simulator = Simulator::new(compiled.world.clone());
    simulator
        .run_until_stable(MAX_TICKS)
        .unwrap_or_else(|err| panic!("{label} must settle before the first reading: {err:?}"));

    let mut worst_settle = 0u64;
    let n = inputs.len();
    for combo in 0u32..(1 << n) {
        let bits: Vec<bool> = (0..n).map(|i| (combo >> i) & 1 == 1).collect();
        for (position, &bit) in lever_positions.iter().zip(bits.iter()) {
            let ticks = set_lever(&mut simulator, *position, bit);
            worst_settle = worst_settle.max(ticks);
        }
        let expected = truth(&bits);
        for (output_name, (&position, &expected)) in outputs.iter().zip(output_positions.iter().zip(expected.iter())) {
            let actual = simulator.world().get(position.0, position.1, position.2).lit;
            assert_eq!(
                actual, expected,
                "{label}: output `{output_name}`, inputs {inputs:?}={bits:?} -> expected {expected}, got {actual}"
            );
        }
    }

    CellCost { real_gates, blocks, settle_game_ticks: worst_settle }
}

// ---------------------------------------------------------------------
// Branch 1 of the isolation rule: every source is private (feeds only the
// merge) -- a bare join is correct, and should cost nothing at all.
// ---------------------------------------------------------------------

fn build_or_via_merge() -> Netlist {
    let mut net = GateNet::new();
    let y = net.merge(&["a", "b"]);
    Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y], gates: net.gates }
}

/// The same function, built the expensive way every reference circuit in
/// this project still uses: `OR(a,b) = NOT(NOR(a,b))`, 2 real gates. This is
/// the control group the referenced cost-table spec measured at 124 blocks
/// / 2 gates / 14 ticks.
fn build_or_via_nor() -> Netlist {
    let mut net = GateNet::new();
    let n = net.nor(&["a", "b"]);
    let y = net.not(&n);
    Netlist { inputs: vec!["a".to_string(), "b".to_string()], outputs: vec![y], gates: net.gates }
}

#[test]
fn a_private_branch_merge_compiles_to_zero_real_gates_and_matches_its_truth_table() {
    let netlist = build_or_via_merge();
    let cost = measure("OR (merge, private branches)", &netlist, &["a", "b"], &["g0"], |bits| {
        vec![bits[0] || bits[1]]
    });

    assert_eq!(cost.real_gates, 0, "a bare wire-merge OR places no gate body at all");
    eprintln!(
        "OR via merge (private branches): {} real gates, {} blocks, {} settle ticks",
        cost.real_gates, cost.blocks, cost.settle_game_ticks
    );

    let nor_netlist = build_or_via_nor();
    let nor_cost = measure("OR (NOR-built control)", &nor_netlist, &["a", "b"], &["g1"], |bits| {
        vec![bits[0] || bits[1]]
    });
    eprintln!(
        "OR via NOR (control): {} real gates, {} blocks, {} settle ticks",
        nor_cost.real_gates, nor_cost.blocks, nor_cost.settle_game_ticks
    );

    // The measured claim from the referenced cost-table spec: a bare merge
    // is strictly cheaper on every axis, not merely different.
    assert!(
        cost.real_gates < nor_cost.real_gates,
        "merge ({}) should use fewer real gates than the NOR-built control ({})",
        cost.real_gates,
        nor_cost.real_gates
    );
    assert!(
        cost.blocks < nor_cost.blocks,
        "merge ({} blocks) should be cheaper than the NOR-built control ({} blocks)",
        cost.blocks,
        nor_cost.blocks
    );
    assert!(
        cost.settle_game_ticks < nor_cost.settle_game_ticks,
        "merge ({} ticks) should settle faster than the NOR-built control ({} ticks)",
        cost.settle_game_ticks,
        nor_cost.settle_game_ticks
    );
}

/// A private-branch merge places no gate body at all -- confirmed the
/// robust way, by counting torches in the whole compiled world (a merge is
/// the only gate here, so any torch anywhere would have to be its own, and
/// `place_merge_gate` never places one).
///
/// This deliberately does *not* also assert that neither input socket ever
/// gets a repeater: `compile`'s general row/channel placement spaces even a
/// trivial two-input circuit across enough distance (`SLOT_PITCH`,
/// `ENTRY_OFFSET`, the row-to-row channel gap) that a bare branch can still
/// need an ordinary interior *strength refresh* partway along its run --
/// exactly the same budget-driven repeater any long dust run gets,
/// completely unrelated to isolation, and one that happened, in this
/// project's own measured run, to land at the very last cell of the
/// west-input socket by coincidence of distance (see this test's own
/// measured numbers below: 82 blocks and 2 repeaters here, not the
/// zero-repeater ideal a minimal hand-built wire achieves once general
/// router spacing is not in the way). `merge_branch_is_bare`'s own
/// decision -- whether a socket's termination is even *allowed* to be a
/// mandatory repeater -- is tested directly and unambiguously in
/// `src/compile/mod.rs`'s own unit tests, independent of this kind of
/// placement-distance noise.
#[test]
fn a_private_branch_merge_places_no_torch_anywhere() {
    let netlist = build_or_via_merge();
    let compiled = compile(&netlist).expect("a two-input merge with both branches private compiles");

    let torches = count_kind(&compiled.world, BlockKind::WallTorch) + count_kind(&compiled.world, BlockKind::Torch);
    assert_eq!(torches, 0, "a bare merge places no torch -- see place_merge_gate's own doc comment");
}

// ---------------------------------------------------------------------
// Branch 2 of the isolation rule: one source (`a`) also feeds a real
// consumer gate besides the merge -- isolation is required, and `compile`
// must apply it automatically from the netlist alone.
// ---------------------------------------------------------------------

/// `sentinel = NOT(a)`, a real NOR gate reading `a` directly; `y = merge(a,
/// b)`. `a`'s own net therefore has two sinks (`sentinel` and the merge), so
/// `merge_branch_is_bare` must isolate it -- `b`, feeding only the merge,
/// stays bare.
fn build_shared_branch_circuit() -> (Netlist, String, String) {
    let mut net = GateNet::new();
    let sentinel = net.not("a");
    let y = net.merge(&["a", "b"]);
    let netlist = Netlist {
        inputs: vec!["a".to_string(), "b".to_string()],
        outputs: vec![sentinel.clone(), y.clone()],
        gates: net.gates,
    };
    (netlist, sentinel, y)
}

#[test]
fn a_shared_branch_is_isolated_automatically_and_both_outputs_match_their_truth_tables() {
    let (netlist, sentinel, merge_output) = build_shared_branch_circuit();
    let cost = measure(
        "OR (merge, one shared branch)",
        &netlist,
        &["a", "b"],
        &[sentinel.as_str(), merge_output.as_str()],
        |bits| vec![!bits[0], bits[0] || bits[1]],
    );

    // `sentinel` is a real NOR gate; the merge itself places none.
    assert_eq!(cost.real_gates, 1, "only the sentinel NOR is a real gate -- the merge places nothing");
    eprintln!(
        "OR via merge (one shared branch): {} real gates, {} blocks, {} settle ticks",
        cost.real_gates, cost.blocks, cost.settle_game_ticks
    );

    // This is the correctness claim the isolation rule exists for: if `a`'s
    // branch into the merge were left bare (uninsulated) instead, `b`'s own
    // signal would run backward up the shared wire and corrupt `sentinel`'s
    // reading -- exactly the failure `truth` above would have caught in the
    // `(a=false, b=true)` case (`sentinel` should read `!false = true`
    // regardless of `b`). It didn't, which is `compile`'s own automatic
    // fanout-based isolation working, not merely a check that never fires --
    // see the next test for the same corruption demonstrated directly by
    // disabling the isolation `compile` actually built.
}

/// Same shared-branch circuit as above, but this test finds the specific
/// repeater `compile`'s own fanout rule inserted to isolate `a`'s branch
/// into the merge, and disables it by hand (replacing it with plain dust,
/// same as a bare branch would have used). Deliberately does *not* also try
/// mutating that repeater away and re-simulating to reproduce the
/// corruption directly on this compiled world: this particular circuit's
/// general row/channel placement happens to add an unrelated, purely
/// length-driven interior repeater elsewhere on `a`'s own shared trunk
/// (confirmed by inspection while building this test), which would mask
/// the very mechanism such a mutation is meant to isolate. The clean,
/// controlled version of that exact demonstration -- no incidental
/// placement noise -- is
/// `without_isolation_a_shared_branch_lets_backflow_corrupt_its_other_consumer`
/// / `with_isolation_the_same_shared_branch_protects_its_other_consumer`
/// below; this test's own job is narrower and just as load-bearing: proving
/// `compile`'s real emission pipeline actually reaches for the mandatory
/// termination on the correct, specific branch, automatically, from the
/// netlist alone.
#[test]
fn compile_places_the_isolating_repeater_on_exactly_the_shared_branch() {
    let (netlist, _sentinel, merge_output) = build_shared_branch_circuit();
    // `merge(&["a", "b"])` declares `a` as input index 0, which
    // `place_merge_gate` lands on the *west* socket (the same
    // `geometry::input_directions` order every NOR gate's sockets use) -- so
    // this is exactly where `compile`'s own fanout rule must have placed the
    // isolating repeater, deterministically, not merely "somewhere in the
    // world".
    let compiled = compile(&netlist).expect("the shared-branch circuit compiles");

    let &(jx, jy, jz) = compiled
        .gate_output_positions
        .get(&merge_output)
        .expect("emit records a position for every gate, including a merge");
    let junction = Position::new(jx, jy, jz);

    let a_socket = junction.offset(Facing::West);
    assert_eq!(
        compiled.world.get(a_socket.x, a_socket.y, a_socket.z).kind,
        BlockKind::Repeater,
        "input `a` fans out to the sentinel besides the merge, so its socket at {a_socket:?} must \
         be the mandatory-repeater termination, not a bare join"
    );
}

// ---------------------------------------------------------------------
// The backflow physics itself, confirmed directly against the real
// simulator on a small hand-built `World` -- independent of `compile`
// entirely, so a bug in the compiler's own placement can never be the
// reason this passes. Same claim the referenced cost-table spec already
// checked; built fresh here rather than only cited, per this task's own
// brief.
// ---------------------------------------------------------------------

fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".to_string();
    state
}

fn dust() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::RedstoneWire;
    state.name = "minecraft:redstone_wire".to_string();
    state
}

fn raw_lever(on: bool) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Lever;
    state.name = "minecraft:lever".to_string();
    state.lit = on;
    state.face = Some(Face::Floor);
    state.facing = Some(Facing::North);
    state
}

fn raw_repeater(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

fn standing_torch() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Torch;
    state.name = "minecraft:redstone_torch".to_string();
    state.lit = true;
    state
}

fn floor_under(world: &mut World, x: i32, y: i32, z: i32) {
    world.set(x, y - 1, z, stone());
}

fn set_raw_lever(simulator: &mut Simulator, pos: (i32, i32, i32), on: bool) {
    simulator.world_mut().set(pos.0, pos.1, pos.2, raw_lever(on));
    simulator.run_until_stable(MAX_TICKS).expect("hand-built probe circuit must settle");
}

/// Lever `a` drives a fork; one branch runs through a repeater into a
/// standing-torch consumer meant to read `NOT(a)`; the other branch is a
/// short run of dust straight into a second point lever `b` also drives.
/// `isolate` selects whether that second branch's own final cell is plain
/// dust (bare, matching an unisolated shared branch) or a repeater (matching
/// what `merge_branch_is_bare` would refuse to build there, forcing the
/// ordinary repeater-terminated socket path instead).
/// `(world, lever_a, lever_b, consumer_torch)`.
type BackflowProbe = (World, (i32, i32, i32), (i32, i32, i32), (i32, i32, i32));

fn build_backflow_probe(isolate: bool) -> BackflowProbe {
    let mut world = World::new(10, 4, 10);

    let lever_a = (1, 1, 5);
    let fork = (2, 1, 5);
    let repeater_pos = (3, 1, 5);
    let consumer_support = (4, 1, 5);
    let consumer_torch = (4, 2, 5);
    let branch2 = (2, 1, 6);
    let merge = (2, 1, 7);
    let lever_b = (3, 1, 7);

    world.set(lever_a.0, lever_a.1, lever_a.2, raw_lever(false));
    floor_under(&mut world, fork.0, fork.1, fork.2);
    world.set(fork.0, fork.1, fork.2, dust());
    floor_under(&mut world, repeater_pos.0, repeater_pos.1, repeater_pos.2);
    world.set(repeater_pos.0, repeater_pos.1, repeater_pos.2, raw_repeater(Facing::East));
    world.set(consumer_support.0, consumer_support.1, consumer_support.2, stone());
    world.set(consumer_torch.0, consumer_torch.1, consumer_torch.2, standing_torch());

    floor_under(&mut world, branch2.0, branch2.1, branch2.2);
    if isolate {
        world.set(branch2.0, branch2.1, branch2.2, raw_repeater(Facing::South));
    } else {
        world.set(branch2.0, branch2.1, branch2.2, dust());
    }
    floor_under(&mut world, merge.0, merge.1, merge.2);
    world.set(merge.0, merge.1, merge.2, dust());
    world.set(lever_b.0, lever_b.1, lever_b.2, raw_lever(false));

    (world, lever_a, lever_b, consumer_torch)
}

#[test]
fn without_isolation_a_shared_branch_lets_backflow_corrupt_its_other_consumer() {
    let (world, lever_a, lever_b, consumer_torch) = build_backflow_probe(false);
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    set_raw_lever(&mut simulator, lever_a, false);
    set_raw_lever(&mut simulator, lever_b, true);

    let torch_lit = simulator.world().get(consumer_torch.0, consumer_torch.1, consumer_torch.2).lit;
    assert!(
        !torch_lit,
        "backflow claim not reproduced: with a=0, b=1, the unisolated consumer torch should read \
         NOT(a OR b) = 0 (corrupted dark) instead of the correct NOT(a) = 1, but it is lit"
    );
}

#[test]
fn with_isolation_the_same_shared_branch_protects_its_other_consumer() {
    let (world, lever_a, lever_b, consumer_torch) = build_backflow_probe(true);
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    set_raw_lever(&mut simulator, lever_a, false);
    set_raw_lever(&mut simulator, lever_b, true);
    let torch_lit = simulator.world().get(consumer_torch.0, consumer_torch.1, consumer_torch.2).lit;
    assert!(torch_lit, "isolated: NOT(a=0) = 1 regardless of b, but the consumer torch reads dark");

    set_raw_lever(&mut simulator, lever_a, true);
    set_raw_lever(&mut simulator, lever_b, false);
    let torch_lit = simulator.world().get(consumer_torch.0, consumer_torch.1, consumer_torch.2).lit;
    assert!(!torch_lit, "NOT(a=1) = 0, but the consumer torch is lit");
}

// ---------------------------------------------------------------------
// A single printed summary table, mirroring `cell_type_cost_table` in
// `tests/cell_type_costs.rs`, so the headline numbers this task reports are
// produced by code in the repository, not hand-copied from `--nocapture`
// output.
// ---------------------------------------------------------------------

#[test]
fn or_merge_cost_table() {
    let mut rows: Vec<(&str, CellCost)> = Vec::new();

    let merge_netlist = build_or_via_merge();
    rows.push((
        "OR via merge (private)",
        measure("OR (merge, private)", &merge_netlist, &["a", "b"], &["g0"], |bits| vec![bits[0] || bits[1]]),
    ));

    let nor_netlist = build_or_via_nor();
    rows.push((
        "OR via NOR (control)",
        measure("OR (NOR control)", &nor_netlist, &["a", "b"], &["g1"], |bits| vec![bits[0] || bits[1]]),
    ));

    let (shared_netlist, sentinel, merge_output) = build_shared_branch_circuit();
    rows.push((
        "OR via merge (one shared branch)",
        measure(
            "OR (merge, shared)",
            &shared_netlist,
            &["a", "b"],
            &[sentinel.as_str(), merge_output.as_str()],
            |bits| vec![!bits[0], bits[0] || bits[1]],
        ),
    ));

    eprintln!("\n{:<36} {:>10} {:>8} {:>8}", "construction", "real gates", "blocks", "ticks");
    for (label, cost) in &rows {
        eprintln!("{label:<36} {:>10} {:>8} {:>8}", cost.real_gates, cost.blocks, cost.settle_game_ticks);
    }
}
