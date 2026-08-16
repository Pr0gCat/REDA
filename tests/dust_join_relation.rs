//! The dust-join relation, **derived by running the simulator** rather than
//! by reading the rules and transcribing them.
//!
//! `src/compile/planner.rs`'s `keep_out` is a *conservative* stand-in for
//! `redstone::simulator::connectivity::dust_reach`: it refuses all twelve
//! cells one cardinal step away in a horizontal axis at `y-1`, `y` and `y+1`,
//! unconditionally, while `dust_reach` gates the two vertical arms on the
//! contents of one particular cell. Nobody had measured how much of that is
//! over-claim, so `2026-08-15-routing-at-scale.md` §8.6 records the question
//! as open ("S found no excess; V says not measured").
//!
//! This file answers it by experiment. Every row of the artifact at
//! `docs/derived/dust-join-relation.md` is produced by building a scratch
//! `World`, powering one conductor through a redstone-block-fed dust run, and
//! running the real `Simulator` to stable — then reading whether the *other*
//! conductor came up. No row is read off the rules.
//!
//! ## The rig, and why it is shaped this way
//!
//! Two dust cells, `A` at a fixed origin and `B` at a relative offset. Only
//! one side is fed. The feed is a redstone block plus a three-cell dust run
//! laid in the direction *away* from the other conductor, because a redstone
//! block is the one source in the vocabulary that drives adjacent dust while
//! powering no block at all (`taxonomy::power_emitted_by`'s `RedstoneBlock`
//! arm: `block_power: None`) — so nothing can leak through a shared floor.
//!
//! **Every measurement is a difference against a control**, and the control is
//! the same world with the *driven* conductor deleted. If the read cell still
//! lights with the driven conductor gone, the reading did not travel through
//! it, and the row is reported `contaminated` rather than as a join. That is
//! what makes a reading of "lit" mean "joined" instead of "lit by something
//! else in the rig" — the third-party sources in the vocabulary (a lit torch,
//! a redstone block) would otherwise be indistinguishable from a join.
//!
//! **Both directions are measured separately.** `dust_connections` is a
//! directed relation and the simulator's power BFS walks it directed
//! (`propagate::recompute_dust_strengths`), so `A -> B` and `B -> A` are two
//! measurements, not one. They do not always agree; see the artifact.
//!
//! ## What the artifact is for
//!
//! Rule 4 — a cited number needs a reproducible method in the tree.
//! [`the_committed_table_is_what_the_simulator_says_today`] regenerates the
//! whole file in memory and compares it byte for byte, so the committed table
//! cannot drift from the simulator without a test going red.
//! [`regenerate_the_dust_join_table`] is the `#[ignore]`d writer that produces
//! it.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;

use reda::redstone::rules::taxonomy::flags_of;
use reda::redstone::simulator::connectivity::{dust_connections, dust_reach};
use reda::redstone::simulator::position::{Position, HORIZONTAL};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

/// Generous: the biggest rig here is a dozen dust cells and at most one
/// repeater, so anything that has not settled by now is oscillating.
const MAX_TICKS: u64 = 400;

const ARTIFACT_PATH: &str = "docs/derived/dust-join-relation.md";

// ---------------------------------------------------------------------
// The block vocabulary
// ---------------------------------------------------------------------

/// What this compiler actually writes into a world, plus two blocks it does
/// not.
///
/// The two extras are deliberate controls, not padding. `glass` and
/// `redstone_block` are the only blocks in the whole taxonomy that **support a
/// dust step without conducting** (`taxonomy::flags_of`: `SUPPORT_FULL` set,
/// `CONDUCTIVE` clear), and the climb and descend rules read those two
/// properties of the *same* cell with opposite polarity. Everything the
/// compiler writes has the two properties equal, so without these two rows the
/// table could not tell which of the two predicates it had measured.
///
/// Confirmed against `src/compile/`: the writing vocabulary is `stone`
/// (`mod.rs:281`), `dust` (`287`), `wall_torch` (`295`), `lamp` (`313`),
/// `lever` (`330`), repeaters, and air. `Torch`, `Glass`, `RedstoneBlock`,
/// `Observer`, `Target` and `Comparator` appear in `src/compile/` only inside
/// test helpers or inside `match` arms that *read* a block somebody else
/// placed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fill {
    Air,
    Solid,
    Dust,
    Repeater,
    Torch,
    WallTorch,
    Lever,
    Lamp,
    /// Not written by this compiler. Support without conduction, control 1.
    Glass,
    /// Not written by this compiler. Support without conduction, control 2 --
    /// and also the rig's own power source.
    RedstoneBlock,
}

/// Every fill, in the order the artifact prints them.
const FILLS: [Fill; 10] = [
    Fill::Air,
    Fill::Solid,
    Fill::Dust,
    Fill::Repeater,
    Fill::Torch,
    Fill::WallTorch,
    Fill::Lever,
    Fill::Lamp,
    Fill::Glass,
    Fill::RedstoneBlock,
];

impl Fill {
    fn name(self) -> &'static str {
        match self {
            Fill::Air => "air",
            Fill::Solid => "solid",
            Fill::Dust => "dust",
            Fill::Repeater => "repeater",
            Fill::Torch => "torch",
            Fill::WallTorch => "wall_torch",
            Fill::Lever => "lever",
            Fill::Lamp => "lamp",
            Fill::Glass => "glass",
            Fill::RedstoneBlock => "redstone_block",
        }
    }

    fn state(self) -> BlockState {
        let mut state = BlockState::air();
        match self {
            Fill::Air => return state,
            Fill::Solid => {
                state.kind = BlockKind::Solid;
                state.name = "minecraft:stone".to_string();
            }
            Fill::Dust => {
                state.kind = BlockKind::RedstoneWire;
                state.name = "minecraft:redstone_wire".to_string();
            }
            Fill::Repeater => {
                state.kind = BlockKind::Repeater;
                state.name = "minecraft:repeater".to_string();
                // Unpowered and pointing across every axis the rig probes, so
                // it contributes geometry and not a signal.
                state.facing = Some(Facing::North);
                state.delay = 1;
                state.lit = false;
            }
            Fill::Torch => {
                state.kind = BlockKind::Torch;
                state.name = "minecraft:redstone_torch".to_string();
                state.lit = true;
            }
            Fill::WallTorch => {
                state.kind = BlockKind::WallTorch;
                state.name = "minecraft:redstone_wall_torch".to_string();
                state.facing = Some(Facing::North);
                state.lit = true;
            }
            Fill::Lever => {
                state.kind = BlockKind::Lever;
                state.name = "minecraft:lever".to_string();
                state.face = Some(Face::Floor);
                state.facing = Some(Facing::North);
                state.lit = false;
            }
            Fill::Lamp => {
                state.kind = BlockKind::Lamp;
                state.name = "minecraft:redstone_lamp".to_string();
                state.lit = false;
            }
            Fill::Glass => {
                state.kind = BlockKind::Glass;
                state.name = "minecraft:glass".to_string();
            }
            Fill::RedstoneBlock => {
                state.kind = BlockKind::RedstoneBlock;
                state.name = "minecraft:redstone_block".to_string();
            }
        }
        state
    }
}

// ---------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------

const SIZE: (i32, i32, i32) = (20, 7, 20);
const ORIGIN: (i32, i32, i32) = (8, 3, 8);
/// Dust cells between the redstone block and the conductor it drives. Three
/// puts the source four cells out, far enough that no offset in the table
/// reaches it, and leaves the driven conductor at strength 12 -- high enough
/// that nothing in the table can be a decay artefact.
const FEED_LEN: i32 = 3;

fn a_pos() -> Position {
    Position::new(ORIGIN.0, ORIGIN.1, ORIGIN.2)
}

fn b_pos(offset: (i32, i32, i32)) -> Position {
    Position::new(
        ORIGIN.0 + offset.0,
        ORIGIN.1 + offset.1,
        ORIGIN.2 + offset.2,
    )
}

/// The horizontal direction from `A` toward `B`. Pure-vertical offsets have
/// none; `West` is chosen so the rig is still well defined, and those rows are
/// flagged in the artifact.
fn primary_dir(offset: (i32, i32, i32)) -> Facing {
    let (dx, _, dz) = offset;
    if dx > 0 {
        Facing::East
    } else if dx < 0 {
        Facing::West
    } else if dz > 0 {
        Facing::South
    } else if dz < 0 {
        Facing::North
    } else {
        Facing::West
    }
}

fn step_by(from: Position, direction: Facing, times: i32) -> Position {
    let mut at = from;
    for _ in 0..times {
        at = at.offset(direction);
    }
    at
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    A,
    B,
}

/// One geometry to measure.
///
/// `gate` is written into the cell one cardinal step from `A` at `A`'s own
/// level -- the single cell both of `dust_reach`'s vertical arms consult. For
/// a climb (`dy == +1`) that cell is `B`'s own floor; for a descend
/// (`dy == -1`) it is the cell directly above `B`. It is only written when the
/// offset is one cardinal step with a level change, because that is the only
/// shape in which the cell is distinct from `A`, `B` and their floors.
///
/// `ceiling` is written into `A.up()`, the cell `dust_reach`'s climb arm tests
/// for conductivity.
#[derive(Clone, Copy)]
struct Case {
    offset: (i32, i32, i32),
    gate: Fill,
    ceiling: Fill,
}

impl Case {
    /// The shape a compiled world actually presents: every dust cell stands on
    /// a stone floor and nothing is laid above it.
    fn as_built(offset: (i32, i32, i32)) -> Case {
        Case {
            offset,
            // A climb's gate cell is B's floor, and the router lays stone
            // under every cell it routes through. A descend's gate cell is the
            // cell above B, which the router leaves empty.
            gate: if offset.1 > 0 { Fill::Solid } else { Fill::Air },
            ceiling: Fill::Air,
        }
    }

    fn has_gate_cell(&self) -> bool {
        self.offset.1 != 0 && self.offset.0.abs() + self.offset.2.abs() == 1
    }
}

fn set(world: &mut World, at: Position, fill: Fill) {
    world.set(at.x, at.y, at.z, fill.state());
}

/// Build the rig. `removed` deletes one conductor, which is how the control
/// run is produced.
///
/// Write order is load-bearing and deliberate, because several offsets make
/// two roles land on the same cell: floors, then the ceiling, then the gate
/// cell, then the conductors, then the feed. So a `B` sitting directly above
/// `A` overwrites the ceiling fill (that offset is forced to `ceiling = air`),
/// and a climb's gate fill overwrites the stone floor that step 1 laid under
/// `B` (which is the point -- the gate cell *is* that floor).
fn build(case: Case, driven: Side, removed: Option<Side>) -> World {
    let a = a_pos();
    let b = b_pos(case.offset);
    let mut world = World::new(SIZE.0, SIZE.1, SIZE.2);

    set(&mut world, a.down(), Fill::Solid);
    set(&mut world, b.down(), Fill::Solid);
    set(&mut world, a.up(), case.ceiling);
    if case.has_gate_cell() {
        set(
            &mut world,
            Position::new(a.x + case.offset.0, a.y, a.z + case.offset.2),
            case.gate,
        );
    }

    // Written as air rather than merely skipped: for the two pure-vertical
    // offsets one conductor's cell doubles as the other's floor, so "not
    // placing" it would leave a stone floor standing where the control needs
    // an empty cell, and the control would be measuring a step instead of an
    // absence.
    if removed == Some(Side::A) {
        set(&mut world, a, Fill::Air);
    } else {
        set(&mut world, a, Fill::Dust);
    }
    if removed == Some(Side::B) {
        set(&mut world, b, Fill::Air);
    } else {
        set(&mut world, b, Fill::Dust);
    }

    let primary = primary_dir(case.offset);
    let (root, away) = match driven {
        Side::A => (a, primary.opposite()),
        Side::B => (b, primary),
    };
    for k in 1..=FEED_LEN {
        let cell = step_by(root, away, k);
        set(&mut world, cell.down(), Fill::Solid);
        set(&mut world, cell, Fill::Dust);
    }
    set(
        &mut world,
        step_by(root, away, FEED_LEN + 1),
        Fill::RedstoneBlock,
    );

    world
}

/// The feed's outermost dust cell -- the one touching the redstone block, and
/// the seed the structural walk starts from.
fn feed_seed(case: Case, driven: Side) -> Position {
    let primary = primary_dir(case.offset);
    let (root, away) = match driven {
        Side::A => (a_pos(), primary.opposite()),
        Side::B => (b_pos(case.offset), primary),
    };
    step_by(root, away, FEED_LEN)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// The read conductor lit, and it lit *because* of the driven one.
    Join,
    /// The read conductor stayed dark with the driven one at full strength.
    No,
    /// The read conductor lit even with the driven one deleted, so this rig
    /// cannot attribute the reading. Reported, never counted as a join.
    Contaminated,
    /// The driven conductor itself read zero -- the feed did not reach it, so
    /// the row says nothing.
    RigDead,
    /// The circuit never settled.
    Diverged,
}

impl Verdict {
    fn tag(self) -> &'static str {
        match self {
            Verdict::Join => "join",
            Verdict::No => "no",
            Verdict::Contaminated => "contaminated",
            Verdict::RigDead => "rig-dead",
            Verdict::Diverged => "diverged",
        }
    }
}

fn settle_and_read(world: World, probes: &[Position]) -> Option<Vec<u8>> {
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).ok()?;
    Some(
        probes
            .iter()
            .map(|p| simulator.world().get(p.x, p.y, p.z).power)
            .collect(),
    )
}

/// Drive one side, read the other, and subtract the control.
fn measure(case: Case, driven: Side) -> Verdict {
    let a = a_pos();
    let b = b_pos(case.offset);
    let (drive_cell, read_cell) = match driven {
        Side::A => (a, b),
        Side::B => (b, a),
    };

    let Some(main) = settle_and_read(build(case, driven, None), &[drive_cell, read_cell]) else {
        return Verdict::Diverged;
    };
    let Some(control) = settle_and_read(build(case, driven, Some(driven)), &[read_cell]) else {
        return Verdict::Diverged;
    };

    if control[0] > 0 {
        return Verdict::Contaminated;
    }
    if main[0] == 0 {
        return Verdict::RigDead;
    }
    if main[1] > 0 {
        Verdict::Join
    } else {
        Verdict::No
    }
}

// ---------------------------------------------------------------------
// The structural mirror
// ---------------------------------------------------------------------

/// Every dust cell reachable from `seed` by walking `dust_connections`
/// forward -- the same directed relation `propagate::recompute_dust_strengths`
/// pushes power along and `compile::verify_connectivity` partitions nets with.
fn forward_reachable(world: &World, seed: Position) -> HashSet<Position> {
    let mut seen = HashSet::new();
    seen.insert(seed);
    let mut queue = VecDeque::new();
    queue.push_back(seed);
    while let Some(pos) = queue.pop_front() {
        for direction in HORIZONTAL {
            for next in dust_connections(world, pos, direction).iter() {
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
    }
    seen
}

/// The same differential as [`measure`], asked of the graph instead of the
/// electricity: is the read cell forward-reachable from the feed with the
/// driven conductor present, and not without it?
fn structural_join(case: Case, driven: Side) -> bool {
    let read_cell = match driven {
        Side::A => b_pos(case.offset),
        Side::B => a_pos(),
    };
    let seed = feed_seed(case, driven);
    let with = forward_reachable(&build(case, driven, None), seed);
    let without = forward_reachable(&build(case, driven, Some(driven)), seed);
    with.contains(&read_cell) && !without.contains(&read_cell)
}

/// Does `dust_reach` name the read cell as a *direct* target of the driven
/// one, in the rig's own world?
fn dust_reach_names_it(case: Case, driven: Side) -> bool {
    let (from, to) = match driven {
        Side::A => (a_pos(), b_pos(case.offset)),
        Side::B => (b_pos(case.offset), a_pos()),
    };
    let world = build(case, driven, None);
    HORIZONTAL
        .iter()
        .any(|&d| dust_reach(&world, from, d).iter().any(|p| p == to))
}

// ---------------------------------------------------------------------
// `keep_out`'s twelve cells, mirrored
// ---------------------------------------------------------------------

/// The twelve offsets `planner.rs`'s `keep_out` (1659) refuses: each of the
/// four horizontal neighbours, and that neighbour shifted up and down by one.
///
/// Mirrored here because `keep_out` is private to `src/compile`. The mirror is
/// anchored by `keep_out_is_exactly_the_twelve_offsets_the_derived_table_scores`
/// in `planner.rs`'s own test module, which asserts set equality against the
/// real function -- so this list cannot drift without that test going red.
const KEEP_OUT_OFFSETS: [(i32, i32, i32); 12] = [
    (0, -1, -1),
    (0, 0, -1),
    (0, 1, -1),
    (0, -1, 1),
    (0, 0, 1),
    (0, 1, 1),
    (1, -1, 0),
    (1, 0, 0),
    (1, 1, 0),
    (-1, -1, 0),
    (-1, 0, 0),
    (-1, 1, 0),
];

fn in_keep_out(offset: (i32, i32, i32)) -> bool {
    KEEP_OUT_OFFSETS.contains(&offset)
}

// ---------------------------------------------------------------------
// The closed form the measurements turned out to obey
// ---------------------------------------------------------------------

/// The two cells a vertical join depends on, named in terms of the *pair*
/// rather than of a direction of travel.
///
/// Take the higher conductor `P` and the lower one `Q`, one cardinal step
/// apart horizontally. Exactly two cells decide whether they are one network:
///
/// * `S` -- the cell directly below `P`, which is also the cell horizontally
///   beside `Q` at `Q`'s own level: the step `Q` would climb.
/// * `C` -- the cell directly above `Q`, which is also the cell horizontally
///   beside `P` at `P`'s own level: the lid over `Q`.
///
/// Returns `(S, C)`.
fn step_and_lid(higher: Position, lower: Position) -> (Position, Position) {
    (higher.down(), lower.up())
}

/// Does the world's block at `at` present a full top face -- can dust stand on
/// it? (`taxonomy::flags_of(..).can_carry_dust()`.)
fn supports(world: &World, at: Position) -> bool {
    flags_of(world.get(at.x, at.y, at.z)).can_carry_dust()
}

/// Does the world's block at `at` conduct?
fn conducts(world: &World, at: Position) -> bool {
    flags_of(world.get(at.x, at.y, at.z)).is_conductive()
}

/// The rule this file's measurements turned out to obey, **directed**: does
/// power leaving `from` arrive at `to` in one hop?
///
/// For two dust cells one cardinal step apart horizontally, with `S` and `C` as
/// [`step_and_lid`] names them:
///
/// * **same layer** -- joined, unconditionally, both ways.
/// * **`to` one layer up** (a climb) -- `supports(S) && !conducts(C)`.
/// * **`to` one layer down** (a descend) -- `!supports(C)`.
/// * **anything else** -- never, including directly above/below, in-plane
///   diagonals, and everything at horizontal distance two.
///
/// `supports` and `conducts` are two independent lookups of the *same cell*
/// `C`, with opposite polarity, and that is the whole source of asymmetry in
/// this relation: a block that is a full cube without conducting blocks the
/// descent and permits the climb.
fn directed_join(world: &World, from: Position, to: Position) -> bool {
    let (dx, dy, dz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    if dx.abs() + dz.abs() != 1 {
        return false;
    }
    match dy {
        0 => true,
        // `to` is higher: `from` climbs. The step is what `to` stands on; the
        // lid is what sits over `from`.
        1 => {
            let (s, c) = step_and_lid(to, from);
            supports(world, s) && !conducts(world, c)
        }
        // `to` is lower: `from` descends. The lid is what sits over `to`.
        -1 => {
            let (_, c) = step_and_lid(from, to);
            !supports(world, c)
        }
        _ => false,
    }
}

/// Are the two cells one network at all -- in either direction?
///
/// This is the predicate `keep_out` is a stand-in for: `verify_connectivity`
/// walks `dust_connections` forward from *every* dust cell, so a one-way edge
/// still merges two nets and is still a violation.
fn pair_join(world: &World, a: Position, b: Position) -> bool {
    directed_join(world, a, b) || directed_join(world, b, a)
}

/// The cell whose contents decide a vertical pair: directly above the lower
/// conductor. Expressed in the rig's own two varied slots -- `ceiling` when B
/// is the higher cell, `gate` when B is the lower one.
fn lid_slot(offset: (i32, i32, i32)) -> Option<Position> {
    if offset.1 == 0 || offset.0.abs() + offset.2.abs() != 1 {
        return None;
    }
    let a = a_pos();
    let b = b_pos(offset);
    let lower = if offset.1 > 0 { a } else { b };
    Some(lower.up())
}

/// Every fill of the one cell [`lid_slot`] names, with everything else left as
/// the router builds it.
///
/// When `B` is the higher cell that slot is `A.up()`, which the rig calls the
/// ceiling and `B`'s floor stays stone; when `B` is the lower one the slot is
/// `B.up()`, which the rig calls the gate.
fn lid_sweep(offset: (i32, i32, i32)) -> Vec<Case> {
    FILLS
        .iter()
        .map(|&fill| {
            if offset.1 > 0 {
                Case {
                    offset,
                    gate: Fill::Solid,
                    ceiling: fill,
                }
            } else {
                Case {
                    offset,
                    gate: fill,
                    ceiling: Fill::Air,
                }
            }
        })
        .collect()
}

/// Whether some content of that one cell makes the pair stop being one network
/// in **both** directions.
fn some_lid_shuts_it(offset: (i32, i32, i32)) -> bool {
    lid_sweep(offset)
        .into_iter()
        .any(|case| measure(case, Side::A) == Verdict::No && measure(case, Side::B) == Verdict::No)
}

// ---------------------------------------------------------------------
// Table 1 -- every offset, in the shape a compiled world presents
// ---------------------------------------------------------------------

/// The whole box `|dx| <= 2`, `|dy| <= 1`, `|dz| <= 2`, origin excluded.
///
/// Two cells out in both horizontal axes, because `COLUMN_CLEARANCE = 2` is
/// the number `2026-08-09-channel-safety-condition.md` derives, and a table
/// that stopped at one could not show the boundary it claims.
fn table_one_offsets() -> Vec<(i32, i32, i32)> {
    let mut offsets = Vec::new();
    for dy in -1..=1 {
        for dz in -2..=2 {
            for dx in -2..=2 {
                if (dx, dy, dz) != (0, 0, 0) {
                    offsets.push((dx, dy, dz));
                }
            }
        }
    }
    offsets
}

fn table_one(out: &mut String) {
    out.push_str(
        "## Table 1 — every offset, in the shape a compiled world presents\n\n\
         Both conductors are bare dust on a stone floor; nothing is laid above \
         either. `A->B` drives A and reads B; `B->A` is the mirror rig. \
         `reach` is `connectivity::dust_reach` asked whether it names the read \
         cell directly, in the very same world. `keep_out` is whether \
         `planner.rs` refuses the offset.\n\n\
         ```\n\
         offset(dx,dy,dz)  A->B    B->A    reach(A->B)  reach(B->A)  keep_out\n",
    );
    for offset in table_one_offsets() {
        let case = Case::as_built(offset);
        let forward = measure(case, Side::A);
        let backward = measure(case, Side::B);
        let _ = writeln!(
            out,
            "{:<17} {:<7} {:<7} {:<12} {:<12} {}",
            format!("({},{},{})", offset.0, offset.1, offset.2),
            forward.tag(),
            backward.tag(),
            dust_reach_names_it(case, Side::A),
            dust_reach_names_it(case, Side::B),
            in_keep_out(offset),
        );
    }
    out.push_str("```\n\n");
}

// ---------------------------------------------------------------------
// Table 2 -- the two cells the vertical arms actually read
// ---------------------------------------------------------------------

/// The four cardinal offsets with a level change, in the artifact's order.
const VERTICAL_OFFSETS: [(i32, i32, i32); 8] = [
    (1, 1, 0),
    (-1, 1, 0),
    (0, 1, 1),
    (0, 1, -1),
    (1, -1, 0),
    (-1, -1, 0),
    (0, -1, 1),
    (0, -1, -1),
];

fn table_two(out: &mut String) {
    out.push_str(
        "## Table 2 — the two cells the vertical arms read, varied\n\n\
         East only, so the two varied cells are enumerated fully; Table 2b \
         repeats the gate sweep in all four directions. `gate` is the cell one \
         step east of A at A's own level — B's floor for the climb, the cell \
         above B for the descend. `ceiling` is `A.up()`.\n\n\
         ```\n\
         offset(dx,dy,dz)  gate            ceiling         A->B    B->A    reach(A->B)  reach(B->A)\n",
    );
    for offset in [(1, 1, 0), (1, -1, 0)] {
        for gate in FILLS {
            for ceiling in FILLS {
                let case = Case {
                    offset,
                    gate,
                    ceiling,
                };
                let _ = writeln!(
                    out,
                    "{:<17} {:<15} {:<15} {:<7} {:<7} {:<12} {}",
                    format!("({},{},{})", offset.0, offset.1, offset.2),
                    gate.name(),
                    ceiling.name(),
                    measure(case, Side::A).tag(),
                    measure(case, Side::B).tag(),
                    dust_reach_names_it(case, Side::A),
                    dust_reach_names_it(case, Side::B),
                );
            }
        }
    }
    out.push_str("```\n\n");
}

fn table_two_b(out: &mut String) {
    out.push_str(
        "## Table 2b — the same gate sweep, all four directions\n\n\
         `ceiling` is air throughout. This is the symmetry check: the relation \
         must not depend on which cardinal axis it is measured along.\n\n\
         ```\n\
         offset(dx,dy,dz)  gate            A->B    B->A    reach(A->B)  reach(B->A)\n",
    );
    for offset in VERTICAL_OFFSETS {
        for gate in FILLS {
            let case = Case {
                offset,
                gate,
                ceiling: Fill::Air,
            };
            let _ = writeln!(
                out,
                "{:<17} {:<15} {:<7} {:<7} {:<12} {}",
                format!("({},{},{})", offset.0, offset.1, offset.2),
                gate.name(),
                measure(case, Side::A).tag(),
                measure(case, Side::B).tag(),
                dust_reach_names_it(case, Side::A),
                dust_reach_names_it(case, Side::B),
            );
        }
    }
    out.push_str("```\n\n");
}

// ---------------------------------------------------------------------
// Table 3 -- a repeater as the second conductor
// ---------------------------------------------------------------------

/// Build the repeater rig: dust `A` fed as usual, a repeater at the offset,
/// and a dust probe on the repeater's output face.
///
/// `along` orients the repeater so its signal travels in the same direction
/// the offset points (A is then at its rear, the one face it reads); `across`
/// turns it ninety degrees, which is the geometry
/// `2026-08-09-channel-safety-condition.md` calls a firewall.
fn build_repeater_rig(offset: (i32, i32, i32), along: bool, remove_a: bool) -> (World, Position) {
    let a = a_pos();
    let r = b_pos(offset);
    let primary = primary_dir(offset);
    let travel = if along {
        primary
    } else {
        match primary {
            Facing::East | Facing::West => Facing::South,
            _ => Facing::East,
        }
    };
    let probe = r.offset(travel);

    let mut world = World::new(SIZE.0, SIZE.1, SIZE.2);
    set(&mut world, a.down(), Fill::Solid);
    set(&mut world, r.down(), Fill::Solid);
    set(&mut world, probe.down(), Fill::Solid);
    if !remove_a {
        set(&mut world, a, Fill::Dust);
    }
    set(&mut world, probe, Fill::Dust);

    let mut repeater = Fill::Repeater.state();
    // `facing` stores the opposite of the travel direction -- the same
    // convention `compile::mod`'s own `repeater()` helper uses.
    repeater.facing = Some(travel.opposite());
    world.set(r.x, r.y, r.z, repeater);

    let away = primary.opposite();
    for k in 1..=FEED_LEN {
        let cell = step_by(a, away, k);
        set(&mut world, cell.down(), Fill::Solid);
        set(&mut world, cell, Fill::Dust);
    }
    set(&mut world, step_by(a, away, FEED_LEN + 1), Fill::RedstoneBlock);

    (world, probe)
}

fn measure_repeater(offset: (i32, i32, i32), along: bool) -> Verdict {
    let (main_world, probe) = build_repeater_rig(offset, along, false);
    let a = a_pos();
    let Some(main) = settle_and_read(main_world, &[a, probe]) else {
        return Verdict::Diverged;
    };
    let (control_world, _) = build_repeater_rig(offset, along, true);
    let Some(control) = settle_and_read(control_world, &[probe]) else {
        return Verdict::Diverged;
    };
    if control[0] > 0 {
        return Verdict::Contaminated;
    }
    if main[0] == 0 {
        return Verdict::RigDead;
    }
    if main[1] > 0 {
        Verdict::Join
    } else {
        Verdict::No
    }
}

fn table_three(out: &mut String) {
    out.push_str(
        "## Table 3 — a repeater standing where the second conductor would\n\n\
         A is dust, fed as usual. At the offset stands a repeater, and the \
         reading is taken from a dust probe on the repeater's *output* face — \
         so a `join` here means A's signal crossed the repeater. `along` puts \
         A at the repeater's rear (the positive control: this must pass at \
         the same layer, or the rig cannot see anything); `across` turns the \
         repeater ninety degrees, the side-contact geometry \
         `2026-08-09-channel-safety-condition.md` calls a firewall.\n\n\
         ```\n\
         offset(dx,dy,dz)  orientation  probe   keep_out\n",
    );
    for offset in KEEP_OUT_OFFSETS {
        for (along, label) in [(true, "along"), (false, "across")] {
            let _ = writeln!(
                out,
                "{:<17} {:<12} {:<7} {}",
                format!("({},{},{})", offset.0, offset.1, offset.2),
                label,
                measure_repeater(offset, along).tag(),
                in_keep_out(offset),
            );
        }
    }
    out.push_str("```\n");
}

// ---------------------------------------------------------------------
// The artifact
// ---------------------------------------------------------------------

fn header() -> String {
    format!(
        "# The dust-join relation, measured\n\n\
         **Generated. Do not edit by hand.** Every row below is a `Simulator` \
         run, not a reading of the rules. Regenerate with\n\n\
         ```\n\
         cargo test --release --test dust_join_relation -- --ignored \
         regenerate_the_dust_join_table\n\
         ```\n\n\
         and `the_committed_table_is_what_the_simulator_says_today` in the same \
         file fails if this text and the simulator ever disagree.\n\n\
         Method, in full, lives in that file's module doc comment. In short: \
         two dust cells at a relative offset; one is driven by a redstone block \
         through a {FEED_LEN}-cell feed run laid away from the other; the world \
         is run to stable; the other cell's power is read; and the whole thing \
         is repeated with the driven cell deleted, as a control. A reading that \
         survives the control is `contaminated` and is never counted as a join.\n\n"
    )
}

/// The counts the rest of the document is worth reading for, computed from the
/// same runs rather than typed in.
fn summary(out: &mut String) {
    let mut joins_as_built = 0usize;
    let mut conditional = Vec::new();
    for offset in KEEP_OUT_OFFSETS {
        let built = Case::as_built(offset);
        let f = measure(built, Side::A);
        let b = measure(built, Side::B);
        if f == Verdict::Join || b == Verdict::Join {
            joins_as_built += 1;
        }
        // Is there any content of the one cell above the lower conductor under
        // which the pair stops being one network?
        if offset.1 != 0 && some_lid_shuts_it(offset) {
            conditional.push(offset);
        }
    }

    let touching = measure(Case::as_built((1, 0, 0)), Side::A);
    let one_apart = measure(Case::as_built((2, 0, 0)), Side::A);

    let _ = write!(
        out,
        "## Summary, computed from the runs below\n\n\
         * **{joins_as_built} of `keep_out`'s 12 cells really join** in the \
         shape a compiled world presents: every dust cell on a stone floor, \
         nothing laid above it. So in the world the router actually builds, \
         `keep_out` is not conservative at all — it is exact.\n\
         * **{} of the 12 are nonetheless conditional**: there is some content \
         of one single cell that stops the pair being one network. They are \
         {:?} — every offset with a level change, and none without.\n\
         * The one cell is always **the cell directly above the lower \
         conductor**. Two independent properties of it are read, with opposite \
         polarity: the higher cell descends when that cell does *not* support a \
         dust step, and the lower cell climbs when it does *not* conduct.\n\
         * The four same-layer cells are **unconditional**. No content of any \
         cell anywhere makes two dust cells one cardinal step apart at the same \
         Y stop being one network; the simulator's own `dust_connections` has \
         no branch that could.\n\
         * `CONDUCTOR_CLEARANCE = 2` holds exactly: touching reads `{}`, one \
         clear cell between reads `{}`.\n\n\
         The closed form, as `directed_join` in the generator states it and \
         `the_closed_form_rule_reproduces_every_attributable_row` checks it \
         against every run here. With `S` the cell below the higher conductor \
         and `C` the cell above the lower one, for a pair one cardinal step \
         apart horizontally:\n\n\
         ```\n\
         same layer          joined, unconditionally, both ways\n\
         lower -> higher     supports_dust_step(S) && !is_conductive(C)\n\
         higher -> lower     !supports_dust_step(C)\n\
         anything else       never\n\
         ```\n\n\
         Either direction is enough to merge two nets: `verify_connectivity` \
         walks `dust_connections` forward from every dust cell, so a one-way \
         edge is still a short.\n\n",
        conditional.len(),
        conditional,
        touching.tag(),
        one_apart.tag(),
    );
}

fn generate() -> String {
    let mut out = header();
    summary(&mut out);
    table_one(&mut out);
    table_two(&mut out);
    table_two_b(&mut out);
    table_three(&mut out);
    out
}

fn artifact_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT_PATH)
}

/// The writer. `#[ignore]`d because it mutates the tree.
#[test]
#[ignore = "writes docs/derived/dust-join-relation.md"]
fn regenerate_the_dust_join_table() {
    let path = artifact_path();
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("create docs/derived");
    std::fs::write(&path, generate()).expect("write the artifact");
    eprintln!("wrote {}", path.display());
}

/// Rule 4's half of the pair: the committed table is checked against the
/// simulator on every run, so no number in it can go stale silently.
#[test]
fn the_committed_table_is_what_the_simulator_says_today() {
    let path = artifact_path();
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must exist ({e}) -- regenerate it", path.display()));
    let fresh = generate();
    if committed.replace("\r\n", "\n") != fresh {
        let committed = committed.replace("\r\n", "\n");
        let first_difference = committed
            .lines()
            .zip(fresh.lines())
            .position(|(a, b)| a != b);
        let detail = match first_difference {
            Some(i) => format!(
                "line {}:\n  committed: {}\n  measured:  {}",
                i + 1,
                committed.lines().nth(i).unwrap_or(""),
                fresh.lines().nth(i).unwrap_or("")
            ),
            None => format!(
                "same prefix, different length ({} vs {} lines)",
                committed.lines().count(),
                fresh.lines().count()
            ),
        };
        panic!(
            "{} no longer matches the simulator -- {detail}\n\
             Rerun the ignored `regenerate_the_dust_join_table` and read the diff \
             before committing it: this table is what `keep_out`'s conservatism \
             is scored against.",
            path.display()
        );
    }
}

// ---------------------------------------------------------------------
// The claims the table is worth nothing without
// ---------------------------------------------------------------------

/// The rig can see a join at all, and can see the absence of one -- the same
/// boundary `tests/spacing_derivation.rs` establishes for the channel router's
/// constants, re-measured through this file's own differential rig so that
/// every other row here inherits a rig known to work in both directions.
#[test]
fn the_rig_reads_a_join_at_distance_one_and_none_at_distance_two() {
    assert_eq!(
        measure(Case::as_built((1, 0, 0)), Side::A),
        Verdict::Join,
        "two touching dust cells must join"
    );
    assert_eq!(
        measure(Case::as_built((2, 0, 0)), Side::A),
        Verdict::No,
        "one clear cell between them must be enough -- this is CONDUCTOR_CLEARANCE = 2"
    );
}

/// The electrical measurement and the graph the simulator itself walks agree
/// on every row the rig could attribute.
///
/// This is what lets the `contaminated` rows still carry an answer: the bridge
/// from "lit" to "one network" is measured over the whole table, not assumed.
#[test]
fn electricity_and_dust_connections_agree_wherever_the_rig_can_attribute() {
    let mut checked = 0usize;
    let mut cases: Vec<Case> = table_one_offsets().into_iter().map(Case::as_built).collect();
    for offset in VERTICAL_OFFSETS {
        for gate in FILLS {
            cases.push(Case {
                offset,
                gate,
                ceiling: Fill::Air,
            });
        }
    }
    for case in cases {
        for side in [Side::A, Side::B] {
            let verdict = measure(case, side);
            let structural = structural_join(case, side);
            match verdict {
                Verdict::Join | Verdict::No => {
                    assert_eq!(
                        verdict == Verdict::Join,
                        structural,
                        "offset {:?} gate {} ceiling {} driving {:?}: the simulator said {} but \
                         dust_connections' own forward walk said {structural}",
                        case.offset,
                        case.gate.name(),
                        case.ceiling.name(),
                        side,
                        verdict.tag(),
                    );
                    checked += 1;
                }
                _ => {}
            }
        }
    }
    assert!(
        checked > 200,
        "only {checked} rows were attributable -- the rig has gone blind"
    );
}

/// `keep_out` refuses twelve cells unconditionally. Under the fill a compiled
/// world actually presents, **all twelve really join** -- in both directions.
///
/// This is the answer to §8.6 of `2026-08-15-routing-at-scale.md` ("whether
/// `keep_out`'s two vertical arms over-claim"), and it is the opposite of what
/// the question expected: measured against the world the router builds, there
/// is no excess to harvest. `keep_out` only over-claims where some *other*
/// route has already laid a conductive lid over the lower cell, which is a
/// different claim from "the arms are conservative".
///
/// Asserted rather than printed, because a silent change to it would be a
/// silent change to what the wedge is made of.
#[test]
fn all_twelve_of_keep_out_s_cells_really_join_in_the_shape_a_compiled_world_presents() {
    let mut joining = Vec::new();
    for offset in KEEP_OUT_OFFSETS {
        let case = Case::as_built(offset);
        let forward = measure(case, Side::A);
        let backward = measure(case, Side::B);
        assert_eq!(
            (forward, backward),
            (Verdict::Join, Verdict::Join),
            "offset {offset:?} as built read {} / {} -- every one of keep_out's \
             twelve cells is supposed to be a real join in this shape",
            forward.tag(),
            backward.tag()
        );
        joining.push(offset);
    }
    assert_eq!(joining.len(), 12);
}

/// The eight vertical cells are nonetheless **conditional**, and the four
/// same-layer ones are not.
///
/// The distinction is the whole point: a conditional refusal is one an exact
/// rule could in principle lift, an unconditional one is not. Measured by
/// sweeping the one gate cell over the whole vocabulary and asking whether any
/// content shuts both directions.
#[test]
fn every_vertical_keep_out_cell_is_conditional_and_every_flat_one_is_not() {
    for offset in KEEP_OUT_OFFSETS {
        if offset.1 == 0 {
            assert!(
                lid_slot(offset).is_none(),
                "a same-layer pair has no lid cell to be conditional on"
            );
            // Nothing in either varied slot may break a same-layer join.
            for fill in FILLS {
                for case in [
                    Case {
                        offset,
                        gate: fill,
                        ceiling: Fill::Air,
                    },
                    Case {
                        offset,
                        gate: Fill::Air,
                        ceiling: fill,
                    },
                ] {
                    assert_ne!(
                        measure(case, Side::A),
                        Verdict::No,
                        "a {} beside or above the driven cell must not break the \
                         same-layer join at {offset:?} -- dust_connections' \
                         same-layer arm has no gate at all",
                        fill.name()
                    );
                }
            }
        } else {
            assert!(
                some_lid_shuts_it(offset),
                "the vertical cell {offset:?} must be conditional -- some content \
                 of the cell above the lower conductor has to shut it, or the \
                 climb/descend arms are not gated at all"
            );
        }
    }
}

/// The rule, stated once as a closed form, reproduces every row the rig could
/// attribute.
///
/// This is what turns the tables from data into a relation: the electrical
/// measurement is independent of `dust_reach`, and the closed form is written
/// over the *pair* (a step cell and a lid cell) rather than over a direction of
/// travel, so agreeing with both is a real constraint and not a restatement.
#[test]
fn the_closed_form_rule_reproduces_every_attributable_row() {
    let mut cases: Vec<Case> = table_one_offsets().into_iter().map(Case::as_built).collect();
    for offset in VERTICAL_OFFSETS {
        for gate in FILLS {
            for ceiling in FILLS {
                cases.push(Case {
                    offset,
                    gate,
                    ceiling,
                });
            }
        }
    }

    let mut checked = 0usize;
    for case in cases {
        for side in [Side::A, Side::B] {
            let verdict = measure(case, side);
            if !matches!(verdict, Verdict::Join | Verdict::No) {
                continue;
            }
            let world = build(case, side, None);
            let (from, to) = match side {
                Side::A => (a_pos(), b_pos(case.offset)),
                Side::B => (b_pos(case.offset), a_pos()),
            };
            // The closed form is a *direct* join between the two conductors.
            // A reading can also arrive by a longer path -- a dust filler
            // bridges -- so the comparison is one-way where a bridge exists,
            // and `structural_join` is what covers those rows instead.
            let bridged = case.gate == Fill::Dust;
            let predicted = directed_join(&world, from, to);
            if !bridged || !predicted {
                assert_eq!(
                    verdict == Verdict::Join,
                    predicted,
                    "offset {:?} gate {} ceiling {} driving {:?}: simulator said {}, \
                     closed form said {predicted}",
                    case.offset,
                    case.gate.name(),
                    case.ceiling.name(),
                    side,
                    verdict.tag(),
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 400, "only {checked} rows were attributable");
}

/// The descend arm is conditional, and the condition is a cell the planner may
/// not have decided.
///
/// Measured both ways round in the same offset, so the row is a demonstration
/// rather than an assertion about a rule: an empty gate cell joins, a stone one
/// does not.
#[test]
fn the_descend_arm_turns_on_and_off_with_one_cell_the_router_may_not_have_placed_yet() {
    let offset = (1, -1, 0);
    let open = Case {
        offset,
        gate: Fill::Air,
        ceiling: Fill::Air,
    };
    let closed = Case {
        offset,
        gate: Fill::Solid,
        ceiling: Fill::Air,
    };
    assert_eq!(
        measure(open, Side::A),
        Verdict::Join,
        "with nothing above the lower cell, the upper dust descends into it"
    );
    assert_eq!(
        measure(closed, Side::A),
        Verdict::No,
        "a stone lid over the lower cell blocks the descent"
    );
    assert_eq!(
        measure(closed, Side::B),
        Verdict::No,
        "and blocks the climb back, because stone is conductive as well as \
         step-supporting"
    );
}

/// The climb arm is conditional on a *second* cell, and that one is above the
/// upper conductor.
#[test]
fn the_climb_arm_turns_off_when_the_cell_above_the_lower_conductor_conducts() {
    let offset = (1, 1, 0);
    let open = Case {
        offset,
        gate: Fill::Solid,
        ceiling: Fill::Air,
    };
    let lidded = Case {
        offset,
        gate: Fill::Solid,
        ceiling: Fill::Solid,
    };
    assert_eq!(measure(open, Side::A), Verdict::Join);
    assert_eq!(
        measure(lidded, Side::A),
        Verdict::No,
        "a conductive block directly above the lower dust cuts the climb"
    );
}

/// The one place the join relation is genuinely asymmetric, and the two blocks
/// that produce it.
///
/// The climb and descend arms read the *same* cell with two *different*
/// predicates -- `supports_dust_step` for the descend, `is_conductive` for the
/// climb -- and those two predicates disagree on exactly the blocks that are a
/// full cube without conducting. So with glass or a redstone block in the gate
/// cell, the lower dust reaches up to the higher one while the higher one does
/// not reach down.
///
/// **Nothing this compiler writes lands in that gap**, which is why no compiled
/// circuit has ever hit it; the test names the property rather than the
/// circumstance, so it stays true if the vocabulary grows.
#[test]
fn a_full_but_non_conducting_step_makes_the_relation_one_way() {
    let glass_lid = Case {
        offset: (1, -1, 0),
        gate: Fill::Glass,
        ceiling: Fill::Air,
    };
    assert_eq!(
        (measure(glass_lid, Side::A), measure(glass_lid, Side::B)),
        (Verdict::No, Verdict::Join),
        "with glass above the lower cell the relation must be one-way: the \
         descent is blocked because glass supports a dust step, and the climb \
         is open because glass does not conduct"
    );

    // The control: stone is full *and* conducting, so both directions shut --
    // which is what shows the asymmetry above is the missing conduction and not
    // merely the full cube.
    let stone_lid = Case {
        offset: (1, -1, 0),
        gate: Fill::Solid,
        ceiling: Fill::Air,
    };
    assert_eq!(measure(stone_lid, Side::A), Verdict::No);
    assert_eq!(measure(stone_lid, Side::B), Verdict::No);

    // The second block with the same flag pair is a redstone block, and it
    // cannot be measured this way at all: it is a power source, so the control
    // run lights the read cell on its own. Recorded rather than dropped -- the
    // property is the block's flags, and `flags_of` confirms the pair directly.
    let redstone_lid = Case {
        offset: (1, -1, 0),
        gate: Fill::RedstoneBlock,
        ceiling: Fill::Air,
    };
    assert_eq!(measure(redstone_lid, Side::A), Verdict::Contaminated);
    let world = build(redstone_lid, Side::A, None);
    let lid = lid_slot((1, -1, 0)).expect("a vertical offset has a lid cell");
    assert_eq!(lid, Position::new(a_pos().x + 1, a_pos().y, a_pos().z));
    assert!(supports(&world, lid) && !conducts(&world, lid));
    assert!(!directed_join(&world, a_pos(), b_pos((1, -1, 0))));
    assert!(directed_join(&world, b_pos((1, -1, 0)), a_pos()));
    assert!(pair_join(&world, a_pos(), b_pos((1, -1, 0))));
}

/// No pure-vertical join exists, at any fill -- `keep_out` never claimed one
/// and the simulator never produces one.
///
/// The downward case cannot be attributed by this rig (any dust beside `A`
/// descends into the cell below it, so the control lights too) and is reported
/// as such rather than silently counted.
#[test]
fn a_cell_directly_above_another_never_joins_it() {
    let up = Case::as_built((0, 1, 0));
    assert_eq!(measure(up, Side::A), Verdict::No);
    assert!(!dust_reach_names_it(up, Side::A));
    assert!(!dust_reach_names_it(up, Side::B));

    let down = Case::as_built((0, -1, 0));
    assert_eq!(measure(down, Side::B), Verdict::No);
    // Driving downward cannot be attributed at all, and the reason is worth
    // recording rather than hiding: *any* dust beside the upper cell descends
    // into the cell below it, so the control lights whether or not the upper
    // cell is there. The offset is outside `keep_out` and the closed form says
    // no join, and both remain true.
    assert_eq!(measure(down, Side::A), Verdict::Contaminated);
    assert!(!dust_reach_names_it(down, Side::A));
    assert!(!in_keep_out((0, 1, 0)) && !in_keep_out((0, -1, 0)));
}

/// The channel-safety document's repeater claim, re-measured at all twelve of
/// `keep_out`'s offsets instead of the one geometry
/// `tests/spacing_derivation.rs` covers.
#[test]
fn a_repeater_is_a_firewall_on_every_side_but_its_own_axis() {
    assert_eq!(
        measure_repeater((1, 0, 0), true),
        Verdict::Join,
        "the positive control: a repeater read from its own rear must pass the \
         signal, or this test proves nothing"
    );
    for offset in KEEP_OUT_OFFSETS {
        assert_eq!(
            measure_repeater(offset, false),
            Verdict::No,
            "a repeater turned across the approach must not pass anything at {offset:?}"
        );
    }
}
