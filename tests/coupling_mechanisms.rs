//! Every way two cells become electrically coupled, **derived by running the
//! simulator** rather than by reading `taxonomy.rs` and transcribing it.
//!
//! `tests/dust_join_relation.rs` did this for one edge type: dust against
//! dust. That is the only edge type `compile::verify_connectivity`
//! (`src/compile/mod.rs:5174`) walks -- it seeds from every
//! `BlockKind::RedstoneWire` cell and follows `connectivity::dust_connections`,
//! which returns dust neighbours and nothing else. Twice on this branch a
//! circuit passed all four physical invariants and computed the wrong
//! function through an edge of some *other* type:
//!
//! * a lit lever strongly powered the block above it, and a route's floor
//!   landed there and read 15 from an input it was never connected to;
//! * a lit gate torch strongly powered the block above it, `full_adder`
//!   routed over it, and eight of its 22 gates came out wrong.
//!
//! Neither had a dust-to-dust edge anywhere. Both couplings were
//! block-mediated: conductor -> strongly powered block -> foreign dust.
//!
//! This file enumerates the edge types by experiment, so the class can be
//! named rather than rediscovered. Every mark in the artifact at
//! `docs/derived/coupling-mechanisms.md` is a `Simulator` run.
//!
//! ## The rig, and why it is shaped this way
//!
//! Three cells: an **emitter** `E`, an optional **mediator** `M`, and a
//! **receiver** `R`. Only the emitter is driven. The receiver is usually a
//! bare redstone dust cell **with air beneath it**, so the mediator the row
//! names is the only block that can ever re-drive it -- a floor would be a
//! second, unnamed mediator and every reading would then be ambiguous about
//! which block carried it. Table 5 replaces the dust with the other four
//! things this compiler writes that can *read* a block, because the receiver
//! turns out to decide the answer as much as the emitter does.
//!
//! Emitters that cannot power themselves (dust, repeater, comparator) are fed
//! by a single redstone block laid on the emitter's own `facing` side -- the
//! rear input of a diode, an arbitrary but fixed side for dust. A redstone
//! block is the one source in the vocabulary that drives adjacent dust while
//! powering **no** block at all (`taxonomy::power_emitted_by`'s
//! `RedstoneBlock` arm: `block_power: None`), so the feed cannot leak into the
//! mediator. [`valid`] refuses, before any world is built, every geometry in
//! which the feed would land on or touch the mediator or the receiver; those
//! cells read `x` rather than being measured with a second source in reach.
//! The first version of this file lacked the adjacency half of that check and
//! the artifact reported `~` (contaminated) down a whole column -- the control
//! caught it, which is the point of having one.
//!
//! **Every measurement is a difference against a control**, and the control is
//! the same world with the emitter cell **written as air**. The receiver's
//! reading in the control must equal its quiescent value (dark dust, dark
//! lamp, *lit* torch, unlit diode); if it does not, something other than the
//! emitter is already driving it and the cell is reported `~`, never as a
//! coupling. A coupling is then simply *the reading changed*, which is why a
//! torch -- whose coupled state is `unlit` -- needs no special case.
//!
//! ## The column that makes this file worth having
//!
//! Every coupled cell also carries whether `verify_connectivity`'s own walk
//! would have seen it. [`joined_by_the_dust_walk`] mirrors that function's
//! loop exactly -- seed from every dust cell in `positions_of` order, BFS
//! forward along `dust_connections`, share one `visited` set across seeds --
//! and asks whether the emitter and the receiver ever land in the same
//! component. `J` means coupled and **invisible** to it; `j` means coupled and
//! visible.
//!
//! ## What the artifact is for
//!
//! Rule 4 -- a cited number needs a reproducible method in the tree.
//! [`the_committed_table_is_what_the_simulator_says_today`] regenerates the
//! whole file in memory and compares it byte for byte.
//! [`regenerate_the_coupling_tables`] is the `#[ignore]`d writer.

use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;

use reda::redstone::rules::taxonomy::{flags_of, power_emitted_toward, BlockPower};
use reda::redstone::simulator::connectivity::dust_connections;
use reda::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use reda::redstone::simulator::propagate::block_signal_at;
use reda::redstone::simulator::{SimulationError, Simulator};
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

/// Generous: the biggest rig here is five cells and at most one diode, so
/// anything not settled by now is oscillating.
const MAX_TICKS: u64 = 400;

const ARTIFACT_PATH: &str = "docs/derived/coupling-mechanisms.md";

/// Room for `ORIGIN` plus or minus four in every axis.
const SIZE: (i32, i32, i32) = (17, 17, 17);
const ORIGIN: (i32, i32, i32) = (8, 8, 8);

fn origin() -> Position {
    Position::new(ORIGIN.0, ORIGIN.1, ORIGIN.2)
}

fn short(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "N",
        Facing::South => "S",
        Facing::East => "E",
        Facing::West => "W",
        Facing::Up => "U",
        Facing::Down => "D",
    }
}

/// Are these two cells cardinal neighbours?
fn adjacent(a: Position, b: Position) -> bool {
    ALL_SIX.into_iter().any(|d| a.offset(d) == b)
}

// ---------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------

/// What can stand in the mediator cell.
///
/// `stone` and `lamp` are this compiler's own conductive blocks; `glass` is
/// the control that is a full cube **without** conducting, which is the one
/// property `propagate::block_signal_at` gates on before it will report a
/// block as powered at all. `dust` is there because a mediator need not be a
/// block, and `air` is the built-in negative control for every row: with
/// nothing in the middle, a coupling two cells apart would have to be
/// something this rig does not model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fill {
    Air,
    Stone,
    Glass,
    Lamp,
    Dust,
}

const MEDIATORS: [Fill; 5] = [Fill::Air, Fill::Stone, Fill::Glass, Fill::Lamp, Fill::Dust];

impl Fill {
    fn name(self) -> &'static str {
        match self {
            Fill::Air => "air",
            Fill::Stone => "stone",
            Fill::Glass => "glass",
            Fill::Lamp => "lamp",
            Fill::Dust => "dust",
        }
    }

    fn state(self) -> BlockState {
        let mut state = BlockState::air();
        match self {
            Fill::Air => return state,
            Fill::Stone => {
                state.kind = BlockKind::Solid;
                state.name = "minecraft:stone".to_string();
            }
            Fill::Glass => {
                state.kind = BlockKind::Glass;
                state.name = "minecraft:glass".to_string();
            }
            Fill::Lamp => {
                state.kind = BlockKind::Lamp;
                state.name = "minecraft:redstone_lamp".to_string();
            }
            Fill::Dust => {
                state.kind = BlockKind::RedstoneWire;
                state.name = "minecraft:redstone_wire".to_string();
            }
        }
        state
    }
}

/// Everything that can stand in the emitter cell.
///
/// The first three emit nothing at all and are the rig's negative controls:
/// `stone` and `glass` are inert blocks, and `lamp` is inert *as a source*
/// even though it is a component this compiler writes and a conductor when it
/// is the mediator. Without them a table of all-`.` rows would be
/// indistinguishable from a rig that had gone blind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Stone,
    Glass,
    Lamp,
    RedstoneBlock,
    Dust,
    Repeater,
    Comparator,
    Torch,
    WallTorch,
    Lever,
    Button,
    PressurePlate,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Stone => "stone",
            Kind::Glass => "glass",
            Kind::Lamp => "lamp",
            Kind::RedstoneBlock => "redstone_block",
            Kind::Dust => "dust",
            Kind::Repeater => "repeater",
            Kind::Comparator => "comparator",
            Kind::Torch => "torch",
            Kind::WallTorch => "wall_torch",
            Kind::Lever => "lever",
            Kind::Button => "button",
            Kind::PressurePlate => "pressure_plate",
        }
    }

    /// Does the component's own `facing` change what it emits? Only these
    /// three are swept over all four horizontal facings; a floor lever's
    /// `facing` orients its handle and nothing else (`compile::mod`'s own
    /// `lever()` says so, and `power_emitted_toward` never reads it).
    fn orientable(self) -> bool {
        matches!(self, Kind::Repeater | Kind::Comparator | Kind::WallTorch)
    }

    /// Does it need something upstream before it emits anything?
    fn needs_feed(self) -> bool {
        matches!(self, Kind::Dust | Kind::Repeater | Kind::Comparator)
    }
}

/// One emitter configuration: a kind plus the facing it is placed with.
///
/// `facing` also names the side the feed goes on, which is why it is carried
/// even for kinds that ignore it: a diode reads its input from exactly the
/// cell its `facing` points at, so one field serves both jobs and a feed can
/// never land on a diode's output side by accident.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Emitter {
    kind: Kind,
    facing: Facing,
}

impl Emitter {
    fn new(kind: Kind, facing: Facing) -> Emitter {
        Emitter { kind, facing }
    }

    fn facing_label(self) -> &'static str {
        if self.kind.orientable() {
            short(self.facing)
        } else {
            "-"
        }
    }

    fn state(self) -> BlockState {
        let mut state = BlockState::air();
        match self.kind {
            Kind::Stone => {
                state.kind = BlockKind::Solid;
                state.name = "minecraft:stone".to_string();
            }
            Kind::Glass => {
                state.kind = BlockKind::Glass;
                state.name = "minecraft:glass".to_string();
            }
            Kind::Lamp => {
                state.kind = BlockKind::Lamp;
                state.name = "minecraft:redstone_lamp".to_string();
            }
            Kind::RedstoneBlock => {
                state.kind = BlockKind::RedstoneBlock;
                state.name = "minecraft:redstone_block".to_string();
            }
            Kind::Dust => {
                state.kind = BlockKind::RedstoneWire;
                state.name = "minecraft:redstone_wire".to_string();
            }
            Kind::Repeater => {
                state.kind = BlockKind::Repeater;
                state.name = "minecraft:repeater".to_string();
                state.facing = Some(self.facing);
                state.delay = 1;
                // Unlit at construction: the settle pass is what turns it on
                // from its own feed, so a row that reads `0` means the feed
                // genuinely failed rather than that a placeholder was believed.
                state.lit = false;
            }
            Kind::Comparator => {
                state.kind = BlockKind::Comparator;
                state.name = "minecraft:comparator".to_string();
                state.facing = Some(self.facing);
                state.power = 0;
                state.lit = false;
            }
            Kind::Torch => {
                state.kind = BlockKind::Torch;
                state.name = "minecraft:redstone_torch".to_string();
                state.lit = true;
            }
            Kind::WallTorch => {
                state.kind = BlockKind::WallTorch;
                state.name = "minecraft:redstone_wall_torch".to_string();
                state.facing = Some(self.facing);
                state.lit = true;
            }
            Kind::Lever => {
                state.kind = BlockKind::Lever;
                state.name = "minecraft:lever".to_string();
                // `Floor`, matching `compile::mod`'s own `lever()`; Minecraft's
                // default is `Wall`, and this compiler never builds a wall for one.
                state.face = Some(Face::Floor);
                state.facing = Some(Facing::North);
                state.lit = true;
            }
            Kind::Button => {
                state.kind = BlockKind::Button;
                state.name = "minecraft:stone_button".to_string();
                state.face = Some(Face::Floor);
                state.facing = Some(Facing::North);
                state.lit = true;
            }
            Kind::PressurePlate => {
                state.kind = BlockKind::PressurePlate;
                state.name = "minecraft:stone_pressure_plate".to_string();
                state.lit = true;
            }
        }
        state
    }

    /// Is this emitter actually emitting, once the world has settled?
    ///
    /// Asked of the settled world rather than assumed, so a row can say `0`
    /// (rig dead) instead of quietly reporting `.` for an emitter that never
    /// turned on -- the failure mode that would make every negative row in
    /// this file worthless.
    fn alive(self, world: &World, at: Position) -> bool {
        let state = world.get(at.x, at.y, at.z);
        match self.kind {
            // The three inert controls and the redstone block have no state
            // that could fail; they are alive by construction.
            Kind::Stone | Kind::Glass | Kind::Lamp | Kind::RedstoneBlock => true,
            Kind::Dust | Kind::Comparator => state.power > 0,
            Kind::Repeater
            | Kind::Torch
            | Kind::WallTorch
            | Kind::Lever
            | Kind::Button
            | Kind::PressurePlate => state.lit,
        }
    }
}

/// Every emitter configuration the two line tables sweep, in artifact order.
fn emitter_configurations() -> Vec<Emitter> {
    let mut out = Vec::new();
    for kind in [
        Kind::Stone,
        Kind::Glass,
        Kind::Lamp,
        Kind::RedstoneBlock,
        Kind::Dust,
        Kind::Repeater,
        Kind::Comparator,
        Kind::Torch,
        Kind::WallTorch,
        Kind::Lever,
        Kind::Button,
        Kind::PressurePlate,
    ] {
        if kind.orientable() {
            for facing in HORIZONTAL {
                out.push(Emitter::new(kind, facing));
            }
        } else {
            out.push(Emitter::new(kind, Facing::North));
        }
    }
    out
}

// ---------------------------------------------------------------------
// The receiver
// ---------------------------------------------------------------------

/// What sits in the cell being read.
///
/// Dust is the default and the only one Tables 1 to 3 use, because dust is
/// what a *net* is made of and therefore what a connectivity invariant is
/// about. The other four are here because each reads a block by a different
/// rule, and two of them accept power that dust refuses -- see Table 5.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Receiver {
    Dust,
    Lamp,
    /// Mounted on the block in `facing.opposite()`, which it reads.
    WallTorch(Facing),
    /// Reads its input from the cell in `facing`.
    Repeater(Facing),
    /// Reads its main signal from the cell in `facing`.
    Comparator(Facing),
}

impl Receiver {
    fn name(self) -> &'static str {
        match self {
            Receiver::Dust => "dust",
            Receiver::Lamp => "lamp",
            Receiver::WallTorch(_) => "wall_torch",
            Receiver::Repeater(_) => "repeater",
            Receiver::Comparator(_) => "comparator",
        }
    }

    fn state(self) -> BlockState {
        let mut state = BlockState::air();
        match self {
            Receiver::Dust => {
                state.kind = BlockKind::RedstoneWire;
                state.name = "minecraft:redstone_wire".to_string();
            }
            Receiver::Lamp => {
                state.kind = BlockKind::Lamp;
                state.name = "minecraft:redstone_lamp".to_string();
                state.lit = false;
            }
            Receiver::WallTorch(facing) => {
                state.kind = BlockKind::WallTorch;
                state.name = "minecraft:redstone_wall_torch".to_string();
                state.facing = Some(facing);
                // Lit is the *quiescent* state of a torch. A coupling puts it
                // out; that is what makes it an inverter.
                state.lit = true;
            }
            Receiver::Repeater(facing) => {
                state.kind = BlockKind::Repeater;
                state.name = "minecraft:repeater".to_string();
                state.facing = Some(facing);
                state.delay = 1;
                state.lit = false;
            }
            Receiver::Comparator(facing) => {
                state.kind = BlockKind::Comparator;
                state.name = "minecraft:comparator".to_string();
                state.facing = Some(facing);
                state.power = 0;
                state.lit = false;
            }
        }
        state
    }

    /// The one number that says whether this receiver noticed anything.
    fn reading(self, world: &World, at: Position) -> u8 {
        let state = world.get(at.x, at.y, at.z);
        match self {
            Receiver::Dust | Receiver::Comparator(_) => state.power,
            Receiver::Lamp | Receiver::WallTorch(_) | Receiver::Repeater(_) => state.lit as u8,
        }
    }

    /// What that number must be when nothing is driving it.
    fn quiescent(self) -> u8 {
        match self {
            Receiver::WallTorch(_) => 1,
            _ => 0,
        }
    }
}

// ---------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------

/// Where the emitter's feed goes, if it needs one.
fn feed_position(emitter: Emitter, at: Position) -> Option<Position> {
    emitter.kind.needs_feed().then(|| at.offset(emitter.facing))
}

fn set(world: &mut World, at: Position, state: BlockState) {
    world.set(at.x, at.y, at.z, state);
}

/// Build one rig. `removed` deletes the emitter, which is how the control run
/// is produced.
fn build(
    emitter: Emitter,
    e: Position,
    mediator: Option<(Position, Fill)>,
    receiver: Receiver,
    r: Position,
    removed: bool,
) -> World {
    let mut world = World::new(SIZE.0, SIZE.1, SIZE.2);

    if let Some((m, fill)) = mediator {
        set(&mut world, m, fill.state());
    }
    if let Some(feed) = feed_position(emitter, e) {
        let mut block = BlockState::air();
        block.kind = BlockKind::RedstoneBlock;
        block.name = "minecraft:redstone_block".to_string();
        set(&mut world, feed, block);
    }
    // Written as air rather than skipped: the control's whole meaning is
    // "everything except the emitter".
    if removed {
        set(&mut world, e, BlockState::air());
    } else {
        set(&mut world, e, emitter.state());
    }
    // Last, so the receiver never loses to another role's write. `valid`
    // proves it cannot collide with anything in the geometries used here.
    set(&mut world, r, receiver.state());

    world
}

/// Whether this rig can be built at all.
///
/// The feed is a second source, so it must neither occupy nor touch the
/// mediator or the receiver -- a redstone block one cell from the receiver
/// would drive it outright, and the control would then read non-quiescent for
/// a reason that has nothing to do with the mechanism under test. Checked
/// structurally, before any world exists, so those cells are reported `x`
/// rather than measured and then explained away.
fn valid(emitter: Emitter, e: Position, mediator: Option<Position>, r: Position) -> bool {
    let Some(feed) = feed_position(emitter, e) else {
        return true;
    };
    if feed == r || adjacent(feed, r) {
        return false;
    }
    match mediator {
        Some(m) => feed != m && !adjacent(feed, m),
        None => true,
    }
}

/// How far the settle got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Settled {
    /// `run_until_stable` returned `Ok`.
    Stable,
    /// `run_until_stable` refused the world outright with
    /// `UnsupportedComponent` and mutated nothing, so the only state available
    /// is what `Simulator::new`'s own constructor-time
    /// `recompute_dust_strengths` produced. Reported, never hidden.
    LoadOnly,
    Diverged,
}

impl Settled {
    fn tag(self) -> &'static str {
        match self {
            Settled::Stable => "stable",
            Settled::LoadOnly => "load-only",
            Settled::Diverged => "diverged",
        }
    }
}

fn settle(world: World) -> (Settled, World) {
    let mut simulator = Simulator::new(world);
    let mode = match simulator.run_until_stable(MAX_TICKS) {
        Ok(_) => Settled::Stable,
        Err(SimulationError::UnsupportedComponent { .. }) => Settled::LoadOnly,
        Err(SimulationError::Diverged { .. }) => Settled::Diverged,
    };
    (mode, simulator.world().clone())
}

/// One cell of a table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Coupling {
    /// The receiver's reading changed because of the emitter, and
    /// `verify_connectivity`'s own walk does **not** contain the edge.
    Invisible,
    /// The reading changed, and the walk does contain the edge.
    Visible,
    /// The reading did not change, with the emitter emitting.
    No,
    /// The control's reading was not the receiver's quiescent value, so
    /// something other than the emitter is already driving it and this rig
    /// cannot attribute anything.
    Contaminated,
    /// The emitter itself never turned on.
    RigDead,
    /// The circuit never settled.
    Diverged,
    /// The feed would have had to occupy or touch the cell under test.
    Invalid,
}

impl Coupling {
    fn code(self) -> &'static str {
        match self {
            Coupling::Invisible => "J",
            Coupling::Visible => "j",
            Coupling::No => ".",
            Coupling::Contaminated => "~",
            Coupling::RigDead => "0",
            Coupling::Diverged => "!",
            Coupling::Invalid => "x",
        }
    }

    fn is_coupled(self) -> bool {
        matches!(self, Coupling::Invisible | Coupling::Visible)
    }
}

/// Drive the emitter, read the receiver, subtract the control.
fn measure(
    emitter: Emitter,
    e: Position,
    mediator: Option<(Position, Fill)>,
    receiver: Receiver,
    r: Position,
) -> (Coupling, Settled) {
    if !valid(emitter, e, mediator.map(|(m, _)| m), r) {
        return (Coupling::Invalid, Settled::Stable);
    }

    let (main_mode, main) = settle(build(emitter, e, mediator, receiver, r, false));
    let (control_mode, control) = settle(build(emitter, e, mediator, receiver, r, true));
    if main_mode == Settled::Diverged || control_mode == Settled::Diverged {
        return (Coupling::Diverged, Settled::Diverged);
    }

    if receiver.reading(&control, r) != receiver.quiescent() {
        return (Coupling::Contaminated, main_mode);
    }
    if !emitter.alive(&main, e) {
        return (Coupling::RigDead, main_mode);
    }
    if receiver.reading(&main, r) == receiver.reading(&control, r) {
        return (Coupling::No, main_mode);
    }
    if joined_by_the_dust_walk(&main, e, r) {
        (Coupling::Visible, main_mode)
    } else {
        (Coupling::Invisible, main_mode)
    }
}

/// The dust-receiver shorthand every table but Table 5 uses.
fn measure_dust(
    emitter: Emitter,
    e: Position,
    mediator: Option<(Position, Fill)>,
    r: Position,
) -> (Coupling, Settled) {
    measure(emitter, e, mediator, Receiver::Dust, r)
}

// ---------------------------------------------------------------------
// The mirror of what ships
// ---------------------------------------------------------------------

/// `compile::verify_connectivity`'s walk, asked whether it puts `a` and `b`
/// in one component.
///
/// Mirrored rather than called because `verify_connectivity` is private to
/// `src/compile` and takes a `Reservation` and a `Netlist` this rig has no
/// business inventing. The walk itself is copied exactly, including the two
/// details that decide the answer: seeds come from
/// `world.positions_of(BlockKind::RedstoneWire)` in flat-index order, and one
/// `visited` set is shared across every seed -- so a purely one-way
/// `dust_connections` edge can leave two electrically joined cells in
/// different components, and that is a property of the shipping walk, not of
/// this mirror.
///
/// A mirror can drift, so it is anchored the same way `KEEP_OUT_OFFSETS` is:
/// `verify_connectivity_misses_a_one_way_dust_edge_that_runs_against_its_seed_order`
/// in `src/compile/mod.rs`'s own test module builds the same geometry and asks
/// the **real** `verify_connectivity`, with its two nets and its
/// `Reservation`, for the same answer. Change the shipping walk and that test
/// goes red, which is the notice to come back here.
fn joined_by_the_dust_walk(world: &World, a: Position, b: Position) -> bool {
    let mut visited: HashSet<Position> = HashSet::new();

    for flat in world.positions_of(BlockKind::RedstoneWire) {
        let (x, y, z) = world.decode(flat);
        let start = Position::new(x, y, z);
        if !visited.insert(start) {
            continue;
        }

        let mut component: HashSet<Position> = HashSet::new();
        component.insert(start);
        let mut queue: VecDeque<Position> = VecDeque::new();
        queue.push_back(start);

        while let Some(pos) = queue.pop_front() {
            for direction in HORIZONTAL {
                for next in dust_connections(world, pos, direction).iter() {
                    if !visited.insert(next) {
                        continue;
                    }
                    component.insert(next);
                    queue.push_back(next);
                }
            }
        }

        if component.contains(&a) && component.contains(&b) {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------
// Tables 1 and 2 -- the emitter's own six faces, bare and across a block
// ---------------------------------------------------------------------

/// A row of six direction columns, plus the settle mode they were taken in.
fn line_row(emitter: Emitter, with_mediator: Option<Fill>) -> (Vec<Coupling>, Settled) {
    let e = origin();
    let mut cells = Vec::with_capacity(ALL_SIX.len());
    let mut mode = Settled::Stable;
    for d in ALL_SIX {
        let (mediator, r) = match with_mediator {
            None => (None, e.offset(d)),
            Some(fill) => (Some((e.offset(d), fill)), e.offset(d).offset(d)),
        };
        let (cell, settled) = measure_dust(emitter, e, mediator, r);
        if settled == Settled::LoadOnly {
            mode = Settled::LoadOnly;
        }
        cells.push(cell);
    }
    (cells, mode)
}

fn codes(cells: &[Coupling]) -> String {
    cells.iter().map(|c| c.code()).collect::<Vec<_>>().join(" ")
}

fn direction_header(directions: &[Facing]) -> String {
    directions
        .iter()
        .map(|&d| short(d))
        .collect::<Vec<_>>()
        .join(" ")
}

fn line_table(out: &mut String, with_mediator: Option<Fill>) {
    let _ = writeln!(
        out,
        "```\n\
         emitter          facing  {}  settle",
        direction_header(&ALL_SIX)
    );
    for emitter in emitter_configurations() {
        let (cells, mode) = line_row(emitter, with_mediator);
        let _ = writeln!(
            out,
            "{:<16} {:<7} {}  {}",
            emitter.kind.name(),
            emitter.facing_label(),
            codes(&cells),
            mode.tag(),
        );
    }
    out.push_str("```\n\n");
}

const LEGEND: &str = "`J` coupled and invisible to `verify_connectivity`'s walk · \
     `j` coupled and visible to it · `.` not coupled · `~` contaminated (the \
     control was not quiescent) · `0` rig dead (the emitter never turned on) · \
     `x` rig invalid (the feed would occupy or touch the cell under test) · \
     `!` diverged.\n\n";

fn table_one(out: &mut String) {
    out.push_str(
        "## Table 1 — the emitter against a bare dust cell on each of its six faces\n\n\
         No mediator at all: the receiver is a floating dust cell one step from \
         the emitter, in the direction the column names. This is the direct \
         drive relation — which neighbours a component lights with no block in \
         between. `facing` is the component's own, and also the side its feed \
         sits on, which is why the diodes read `x` in exactly one column: a \
         diode's rear is where its input has to come from, so this rig cannot \
         ask what a diode does to its own rear.\n\n",
    );
    out.push_str(LEGEND);
    line_table(out, None);
}

fn table_two(out: &mut String) {
    out.push_str(
        "## Table 2 — the emitter across one stone block\n\n\
         Same sweep with a stone block inserted between: emitter at the origin, \
         stone one step out in the column's direction, dust one step beyond \
         that. A mark here is the shipped bugs' own mechanism — conductor, \
         strongly powered block, foreign dust — and the emitter and the \
         receiver are two cells apart, so no dust-to-dust edge can exist \
         between them at all.\n\n\
         Read this against Table 1 and the difference is the whole weak/strong \
         distinction: a torch drives dust on five of its six faces directly, \
         and drives dust *across a block* on exactly one, because only its \
         upward power is `BlockPower::Strong`.\n\n",
    );
    line_table(out, Some(Fill::Stone));
}

// ---------------------------------------------------------------------
// Table 3 -- what the mediator has to be, and which of its faces drive
// ---------------------------------------------------------------------

/// The drivers Table 3 puts against the mediator, in artifact order.
const FACE_DRIVERS: [Kind; 8] = [
    Kind::Lever,
    Kind::Torch,
    Kind::WallTorch,
    Kind::Repeater,
    Kind::Comparator,
    Kind::Dust,
    Kind::RedstoneBlock,
    Kind::Stone,
];

/// How a Table 3 driver is oriented, given which side of the mediator it is on.
///
/// A wire is fed from the side *away* from the mediator, so all five faces stay
/// measurable. A diode cannot be: its feed has to sit at its rear, and a diode's
/// rear is horizontal by construction, so one column is always lost to `x`.
fn face_driver(kind: Kind, d_in: Facing) -> Emitter {
    Emitter::new(
        kind,
        if kind == Kind::Dust {
            d_in
        } else {
            Facing::North
        },
    )
}

/// The five faces of the mediator that are not the driver's own cell.
fn face_outputs(d_in: Facing) -> Vec<Facing> {
    ALL_SIX.into_iter().filter(|&d| d != d_in).collect()
}

fn face_table(out: &mut String, d_in: Facing) {
    let outputs = face_outputs(d_in);
    let _ = writeln!(
        out,
        "```\n\
         driver           mediator  {}  settle",
        direction_header(&outputs)
    );

    let m = origin();
    let e = m.offset(d_in);
    for kind in FACE_DRIVERS {
        let driver = face_driver(kind, d_in);
        for fill in MEDIATORS {
            let mut cells = Vec::new();
            let mut mode = Settled::Stable;
            for &d_out in &outputs {
                let (cell, settled) = measure_dust(driver, e, Some((m, fill)), m.offset(d_out));
                if settled == Settled::LoadOnly {
                    mode = Settled::LoadOnly;
                }
                cells.push(cell);
            }
            let _ = writeln!(
                out,
                "{:<16} {:<9} {}  {}",
                driver.kind.name(),
                fill.name(),
                codes(&cells),
                mode.tag(),
            );
        }
    }
    out.push_str("```\n\n");
}

fn table_three(out: &mut String) {
    out.push_str(
        "## Table 3 — the mediator's material, and which of its faces drive\n\n\
         The geometry of both shipped bugs: the driver sits **directly below** \
         the mediator and the receiver is a bare dust cell on one of the \
         mediator's five other faces. Sweeping the material answers what makes \
         a block able to carry a coupling at all; sweeping the face answers \
         which cells it then drives. Same alphabet as Table 1.\n\n\
         The two diodes read `x` in the `N` column: their feed has to sit at \
         their rear, a diode's rear is horizontal, and that puts the feed one \
         step from the mediator's own north face. A wire has no such \
         constraint and is fed from directly below instead, so its five \
         columns all survive.\n\n",
    );
    face_table(out, Facing::Down);

    out.push_str(
        "## Table 3b — the same sweep with the driver standing *on* the mediator\n\n\
         The mirror, and the one geometry in which a dust driver actually \
         powers the mediator: dust weakly powers the block it stands on, and \
         nothing else. So this table is where mechanism 4 — weak power reaching \
         a block and stopping there — is visible, in the `D` column of the \
         `dust` rows.\n\n\
         `torch` here stands on the mediator, so the mediator is its own \
         support: its whole row is the statement that a torch does not power \
         what it stands on.\n\n",
    );
    face_table(out, Facing::Up);
}

// ---------------------------------------------------------------------
// Table 4 -- a one-way dust edge, and the walk's seed order
// ---------------------------------------------------------------------

/// The two dust cells Table 4 pairs: an upper one and a lower one, one
/// cardinal step apart horizontally.
///
/// `floor` fills the cell under the **upper** wire and `mid` fills the cell
/// between them -- above the lower wire, beside the upper one. Those are
/// exactly the two cells `dust_join_relation.rs` names `S` and `C`, and they
/// are what makes the relation asymmetric.
fn pair_cells() -> (Position, Position, Position, Position) {
    let upper = origin();
    let lower = upper.offset(Facing::East).offset(Facing::Down);
    let mid = upper.offset(Facing::East);
    let floor = upper.offset(Facing::Down);
    (upper, lower, mid, floor)
}

fn build_pair(floor: Fill, mid: Fill, drive_upper: bool, removed: bool) -> World {
    let (upper, lower, mid_cell, floor_cell) = pair_cells();
    let mut world = World::new(SIZE.0, SIZE.1, SIZE.2);

    // The lower wire always stands on stone; only the upper one's floor is
    // swept, because that is the cell the climb consults.
    set(&mut world, lower.down(), Fill::Stone.state());
    set(&mut world, floor_cell, floor.state());
    set(&mut world, mid_cell, mid.state());

    let driven = if drive_upper { upper } else { lower };
    for cell in [upper, lower] {
        if removed && cell == driven {
            set(&mut world, cell, BlockState::air());
        } else {
            set(&mut world, cell, Fill::Dust.state());
        }
    }

    // Fed from the far side of the driven wire, away from the other one.
    let feed = if drive_upper {
        upper.offset(Facing::West)
    } else {
        lower.offset(Facing::East)
    };
    let mut block = BlockState::air();
    block.kind = BlockKind::RedstoneBlock;
    block.name = "minecraft:redstone_block".to_string();
    set(&mut world, feed, block);

    world
}

fn measure_pair(floor: Fill, mid: Fill, drive_upper: bool) -> Coupling {
    let (upper, lower, _, _) = pair_cells();
    let (driven, read) = if drive_upper {
        (upper, lower)
    } else {
        (lower, upper)
    };

    let (main_mode, main) = settle(build_pair(floor, mid, drive_upper, false));
    let (control_mode, control) = settle(build_pair(floor, mid, drive_upper, true));
    if main_mode == Settled::Diverged || control_mode == Settled::Diverged {
        return Coupling::Diverged;
    }
    if control.get(read.x, read.y, read.z).power > 0 {
        return Coupling::Contaminated;
    }
    if main.get(driven.x, driven.y, driven.z).power == 0 {
        return Coupling::RigDead;
    }
    if main.get(read.x, read.y, read.z).power == 0 {
        return Coupling::No;
    }
    if joined_by_the_dust_walk(&main, driven, read) {
        Coupling::Visible
    } else {
        Coupling::Invisible
    }
}

/// The three fills Table 4 sweeps into its two decisive cells.
const PAIR_FILLS: [Fill; 3] = [Fill::Air, Fill::Stone, Fill::Glass];

fn table_four(out: &mut String) {
    out.push_str(
        "## Table 4 — a one-way dust edge, and what the walk's seed order does with it\n\n\
         Two wires one cardinal step apart with the second a layer down. \
         `floor` is the cell under the **upper** wire; `mid` is the cell \
         between them. `up→lo` drives the upper wire and reads the lower one, \
         `lo→up` is the mirror rig, and `walk` is whether \
         `verify_connectivity`'s own walk puts the two in one component.\n\n\
         This is still mechanism 1 — dust against dust, the one relation the \
         walk is supposed to cover — so every coupled row here should be `j`. \
         The rows where it is `J` instead are rows where the edge exists in \
         **one direction only** and the walk's seed order runs against it: \
         seeds arrive in `positions_of` order, which is flat-index order, \
         which is lowest `y` first, and a cell already claimed by an earlier \
         seed's component is skipped with `continue` before any owner is \
         compared. So a descend-only edge — upper drives lower, lower cannot \
         climb back — is walked from the lower cell first, finds nothing, and \
         the upper cell then forms a component of its own that never meets it.\n\n\
         **NOT MEASURED: whether any circuit this compiler builds contains \
         such a pair.** The structural condition is that the upper wire's own \
         floor does not support a dust step, which is not the shape \
         `Case::as_built` in `tests/dust_join_relation.rs` reports the router \
         producing. Nothing here says it cannot happen; nothing here says it \
         does.\n\n",
    );
    let _ = writeln!(
        out,
        "```\n\
         floor   mid     up→lo  lo→up  walk"
    );
    for floor in PAIR_FILLS {
        for mid in PAIR_FILLS {
            let (upper, lower, _, _) = pair_cells();
            let world = build_pair(floor, mid, true, false);
            let _ = writeln!(
                out,
                "{:<7} {:<7} {:<6} {:<6} {}",
                floor.name(),
                mid.name(),
                measure_pair(floor, mid, true).code(),
                measure_pair(floor, mid, false).code(),
                joined_by_the_dust_walk(&world, upper, lower),
            );
        }
    }
    out.push_str("```\n\n");
}

// ---------------------------------------------------------------------
// Table 5 -- the receiver decides whether weak power couples
// ---------------------------------------------------------------------

/// Where each receiver sits relative to the mediator, and which way the
/// `beside` driver is placed from it.
///
/// A lamp and a dust cell hang under the mediator; the three components that
/// read a *neighbouring* block are placed north of it, each oriented so the
/// mediator is the cell it reads (a wall torch's support is
/// `facing.opposite()`; a diode's input is `facing`).
fn receiver_slots(m: Position) -> Vec<(Receiver, Position, Facing)> {
    vec![
        (Receiver::Dust, m.down(), Facing::North),
        (Receiver::Lamp, m.down(), Facing::North),
        (
            Receiver::WallTorch(Facing::North),
            m.offset(Facing::North),
            Facing::North,
        ),
        (Receiver::Repeater(Facing::South), m.offset(Facing::North), Facing::North),
        (
            Receiver::Comparator(Facing::South),
            m.offset(Facing::North),
            Facing::North,
        ),
    ]
}

/// The four drivers Table 5 sweeps: one of each power class, plus an inert
/// control.
fn receiver_drivers() -> [(&'static str, Kind); 4] {
    [
        ("lever (strong)", Kind::Lever),
        ("dust (weak)", Kind::Dust),
        ("redstone_block (no block power)", Kind::RedstoneBlock),
        ("stone (inert)", Kind::Stone),
    ]
}

fn table_five(out: &mut String) {
    out.push_str(
        "## Table 5 — the receiver decides whether weak power couples\n\n\
         Tables 1 to 3 all read a dust cell, and dust is only ever re-driven \
         by **strong** block power (`recompute_dust_strengths` seeds a wire \
         from `block_signal_at` only when the answer is \
         `BlockPower::Strong`). Four other things this compiler writes can \
         read a block, and two of them accept weak power. That is a whole \
         second class of edge, and no dust probe anywhere can see it.\n\n\
         `across` puts the driver on top of the mediator so the coupling has \
         to cross it — a lever there powers it strongly, a dust cell standing \
         on it powers it weakly. `beside` moves the driver next to the \
         receiver instead and leaves the mediator unpowered, which is the \
         control for the same row: a mark under `beside` is a direct \
         component-to-receiver edge, and its absence says the receiver reads \
         only the one cell it is supposed to.\n\n\
         The mediator is stone throughout. `dust` and `lamp` hang under it; \
         `wall_torch`, `repeater` and `comparator` stand north of it, each \
         oriented so the mediator is the cell it reads.\n\n",
    );

    let m = origin();
    let slots = receiver_slots(m);
    let _ = writeln!(
        out,
        "```\n\
         path    driver                           {}  settle",
        slots
            .iter()
            .map(|(receiver, _, _)| format!("{:<10}", receiver.name()))
            .collect::<Vec<_>>()
            .join(" ")
    );

    for path in ["across", "beside"] {
        for (label, kind) in receiver_drivers() {
            let mut cells = Vec::new();
            let mut mode = Settled::Stable;
            for &(receiver, r, away) in &slots {
                let (e, feed_dir) = match path {
                    "across" => (m.up(), Facing::Up),
                    _ => (r.offset(away), away),
                };
                let driver = Emitter::new(kind, feed_dir);
                let (cell, settled) =
                    measure(driver, e, Some((m, Fill::Stone)), receiver, r);
                if settled == Settled::LoadOnly {
                    mode = Settled::LoadOnly;
                }
                cells.push(cell);
            }
            let _ = writeln!(
                out,
                "{:<7} {:<32} {}  {}",
                path,
                label,
                cells
                    .iter()
                    .map(|c| format!("{:<10}", c.code()))
                    .collect::<Vec<_>>()
                    .join(" "),
                mode.tag(),
            );
        }
    }
    out.push_str("```\n\n");
}

// ---------------------------------------------------------------------
// Table 6 -- the taxonomy's own answer, for comparison
// ---------------------------------------------------------------------

/// What `taxonomy::power_emitted_toward` says a component sends each way.
///
/// Printed beside the electrical readings rather than instead of them: this is
/// the shipping rule's own answer, and the point of the table is that for
/// three components it is a *fallback* -- `power_emitted_toward`'s `_ => full`
/// arm -- rather than a modelled direction.
fn declared(emitter: Emitter, direction: Facing) -> &'static str {
    let state = emitter.state();
    let out = power_emitted_toward(&state, direction);
    match (out.drives_dust, out.block_power) {
        (false, BlockPower::None) => "-",
        (_, BlockPower::Strong) => "S",
        (_, BlockPower::Weak) => "w",
        (true, BlockPower::None) => "d",
    }
}

fn table_six(out: &mut String) {
    out.push_str(
        "## Table 6 — the taxonomy's own answer, for comparison\n\n\
         `taxonomy::power_emitted_toward` asked in all six directions, with no \
         world at all. `S` strong block power · `w` weak block power · `d` \
         drives dust but powers no block · `-` inert. Read against Table 2: an \
         `S` here is what a `J` there is made of, and a `w` is what a `.` \
         there is made of.\n\n\
         The dust row reads `-` in five of six directions and that is **not** a \
         claim that dust is inert horizontally — `power_emitted_toward` has no \
         world and therefore cannot know a wire's connection shape. The \
         world-aware answer is `propagate::dust_power_toward`, and Table 3b \
         measures it.\n\n\
         The three rows to look at are `lever`, `button` and `pressure_plate`. \
         Their `face` is not modelled by `power_emitted_toward` — they fall \
         through its `_ => full` arm — so the taxonomy gives them **strong \
         power in all six directions**. Vanilla gives a `face=floor` lever \
         strong power to the block **below** it only.\n\n",
    );
    let _ = writeln!(
        out,
        "```\n\
         emitter          facing  {}",
        direction_header(&ALL_SIX)
    );
    for emitter in emitter_configurations() {
        let _ = writeln!(
            out,
            "{:<16} {:<7} {}",
            emitter.kind.name(),
            emitter.facing_label(),
            ALL_SIX
                .iter()
                .map(|&d| declared(emitter, d))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    out.push_str("```\n\n");
}

// ---------------------------------------------------------------------
// The artifact
// ---------------------------------------------------------------------

fn header() -> String {
    "# Every coupling mechanism, measured\n\n\
     **Generated. Do not edit by hand.** Every mark below is a `Simulator` \
     run, not a reading of the rules. Regenerate with\n\n\
     ```\n\
     cargo test --release --test coupling_mechanisms -- --ignored \
     regenerate_the_coupling_tables\n\
     ```\n\n\
     and `the_committed_table_is_what_the_simulator_says_today` in the same \
     file fails if this text and the simulator ever disagree.\n\n\
     Method, in full, lives in `tests/coupling_mechanisms.rs`'s module doc \
     comment. In short: an emitter, an optional mediator block, and a receiver \
     — usually a **bare dust cell with air beneath it**, so the mediator the \
     row names is the only block that can re-drive it. The world is run to \
     stable, the receiver is read, and the whole thing is repeated with the \
     emitter cell written as air, as a control. A coupling is a reading that \
     *changed*; a control that was not already quiescent is reported `~` and \
     is never counted as one.\n\n\
     The dust-to-dust relation is **not** re-derived here. It already has its \
     own artifact at `docs/derived/dust-join-relation.md`, measured the same \
     way.\n\n"
        .to_string()
}

/// The counts the document is worth reading for, computed from the same runs
/// rather than typed in.
fn summary(out: &mut String) {
    let mut direct_coupled = 0usize;
    let mut direct_invisible = 0usize;
    let mut across_coupled = 0usize;
    let mut across_invisible = 0usize;
    let mut strong_emitters: Vec<&'static str> = Vec::new();
    let mut load_only: Vec<&'static str> = Vec::new();

    for emitter in emitter_configurations() {
        let (direct, direct_mode) = line_row(emitter, None);
        let (across, across_mode) = line_row(emitter, Some(Fill::Stone));
        for cell in &direct {
            if cell.is_coupled() {
                direct_coupled += 1;
                if *cell == Coupling::Invisible {
                    direct_invisible += 1;
                }
            }
        }
        for cell in &across {
            if cell.is_coupled() {
                across_coupled += 1;
                if *cell == Coupling::Invisible {
                    across_invisible += 1;
                }
            }
        }
        if across.iter().any(|c| c.is_coupled()) && !strong_emitters.contains(&emitter.kind.name())
        {
            strong_emitters.push(emitter.kind.name());
        }
        if (direct_mode == Settled::LoadOnly || across_mode == Settled::LoadOnly)
            && !load_only.contains(&emitter.kind.name())
        {
            load_only.push(emitter.kind.name());
        }
    }

    // Which materials carry a coupling at all, and how many faces they drive.
    let m = origin();
    let lever = Emitter::new(Kind::Lever, Facing::North);
    let mut conducting: Vec<(&'static str, usize)> = Vec::new();
    for fill in MEDIATORS {
        let driven = face_outputs(Facing::Down)
            .into_iter()
            .filter(|&d_out| {
                measure_dust(lever, m.down(), Some((m, fill)), m.offset(d_out))
                    .0
                    .is_coupled()
            })
            .count();
        conducting.push((fill.name(), driven));
    }

    // Which receivers a *weakly* powered block reaches -- the class no dust
    // probe anywhere can see.
    let weak_driver = Emitter::new(Kind::Dust, Facing::Up);
    let weak_reaches: Vec<&'static str> = receiver_slots(m)
        .into_iter()
        .filter(|&(receiver, r, _)| {
            measure(weak_driver, m.up(), Some((m, Fill::Stone)), receiver, r)
                .0
                .is_coupled()
        })
        .map(|(receiver, _, _)| receiver.name())
        .collect();

    // Mechanism 1, walked by the shipping invariant -- but not in every
    // geometry. Count the pairs that couple electrically and are nonetheless
    // in two different components of that walk.
    let mut one_way_missed = Vec::new();
    for floor in PAIR_FILLS {
        for mid in PAIR_FILLS {
            for drive_upper in [true, false] {
                if measure_pair(floor, mid, drive_upper) == Coupling::Invisible {
                    one_way_missed.push((floor.name(), mid.name(), drive_upper));
                }
            }
        }
    }

    let one_way_count = one_way_missed.len();

    let _ = write!(
        out,
        "## Summary, computed from the runs below\n\n\
         * **{direct_coupled} direct-drive couplings into a dust cell** (Table \
         1), of which **{direct_invisible} are invisible** to \
         `verify_connectivity`'s walk.\n\
         * **{across_coupled} couplings into a dust cell across one stone \
         block** (Table 2), of which **{across_invisible} are invisible** to \
         that walk — all of them. That is not a coincidence and not a bug in \
         the walk's implementation: the walk follows `dust_connections`, and \
         `dust_connections` has no edge that leaves a dust cell for anything \
         but another dust cell, so no block-mediated coupling can ever appear \
         in it.\n\
         * The emitters that reach a dust cell **through** a stone block, \
         measured rather than read off the taxonomy: {strong_emitters:?}.\n\
         * A lit lever directly below the mediator drives this many of the \
         mediator's five other faces, by material: {conducting:?}. `air` is the \
         negative control (nothing in the middle, nothing carried); `glass` is \
         the full cube that does **not** conduct, which is the property \
         `block_signal_at` gates on.\n\
         * A **weakly** powered block reaches these receivers and no others: \
         {weak_reaches:?}. A dust cell is not among them, which is why no \
         amount of dust probing can find this class.\n\
         * `run_until_stable` **refuses** any world containing these emitters \
         outright, with `SimulationError::UnsupportedComponent`: {load_only:?}. \
         Their rows are taken from `Simulator::new`'s constructor-time \
         `recompute_dust_strengths` instead, and are tagged `load-only`.\n\
         * And the walk does not even cover mechanism 1 completely. \
         **{one_way_count} of Table 4's 18 dust-against-dust rigs couple electrically and \
         still land in two different components** of `verify_connectivity`'s \
         own walk, as `(floor, mid, drove_the_upper_wire)`: \
         {one_way_missed:?}. The cause is seed order against a one-way edge; \
         Table 4 says how, and what is not known about whether it is \
         reachable.\n\n\
         ## The mechanisms, named\n\n\
         Each is an edge in the realised world's electrical graph. The netlist \
         asks for the first two; realisation supplies the rest for free.\n\n\
         1. **dust ↔ dust.** `connectivity::dust_connections`. Derived in full \
         at `docs/derived/dust-join-relation.md`. Same layer unconditionally, \
         plus a gated climb and descend.\n\
         2. **component → adjacent dust.** Table 1. A lit lever, torch, \
         redstone block, or a diode's output face lights a dust cell touching \
         it with no block in between. Note the torch: it drives dust on five \
         faces, withholding only its own support.\n\
         3. **component → block → dust.** Tables 2 and 3. Requires two things \
         at once: the block conducts \
         (`taxonomy::flags_of(..).is_conductive()`, the gate at the top of \
         `propagate::block_signal_at`) **and** the power arriving is \
         `BlockPower::Strong`. A block powered this way then drives dust on \
         **every one of its six faces**, not only the face pointing away from \
         the source. **Both shipped bugs are this mechanism.**\n\
         4. **component → block → torch / diode rear, on *weak* power.** Table \
         5. `component::torch_should_be_lit` puts a torch out when its support \
         is powered *at all* (`block_power_at != None`), and \
         `propagate::diode_rear_signal` hands a repeater or comparator the \
         weak strength its rear block carries. So a wire whose run merely ends \
         against a gate's support block turns that gate off, with no strong \
         power anywhere and no dust-to-dust edge anywhere. This is the class \
         `TorchMergeFailure::ForeignNetReachesSupport` names.\n\
         5. **weak power → dust.** Does not exist: `recompute_dust_strengths` \
         seeds a wire from a neighbouring block only when \
         `block_signal_at` answers `Strong`. Measured in Table 3b's `dust` \
         rows and asserted by `a_block_powered_only_by_dust_drives_no_further_dust`.\n\
         6. **block → block.** Does not exist. `block_signal_at` reads its six \
         neighbours through `dust_power_toward`, which defers to \
         `power_emitted_toward` for everything that is not dust, and no arm \
         there emits anything for a plain block. Measured by \
         `a_strongly_powered_block_cannot_power_the_next_block`, which is also \
         why a lamp sitting on a strongly powered block stays dark (Table 5, \
         `across` × `lamp`).\n\
         7. **torch → its own support.** Does not exist, and that is the whole \
         reason a torch inverts. The withheld direction moves with `facing`, \
         measured at all four in \
         `a_torch_never_powers_its_own_support`.\n\
         8. **quasi-connectivity.** **NOT MEASURED, and not modelled at all.** \
         `src/redstone/simulator/mod.rs`'s module doc names it as out of \
         scope. Nothing in this file can say whether a realised world contains \
         one, because the simulator this file drives has no such edge to find.\n\n\
         ## What `verify_connectivity` walks\n\n\
         Mechanism 1, and nothing else — and not all of that. \
         `verify_connectivity` (`src/compile/mod.rs:5174`) seeds from every \
         `BlockKind::RedstoneWire` cell and follows `dust_connections`; every \
         other mechanism above leaves a dust cell for a block, or never \
         touches a dust cell at all, and is therefore outside the relation it \
         walks. **Mechanisms 2, 3 and 4 are entirely unchecked by it**, and \
         mechanism 1 is checked except where a one-way edge runs against its \
         seed order (Table 4).\n\n\
         Mechanism 3 is partly covered elsewhere — `verify_torch_merge`'s \
         `net_reach` (`src/compile/mod.rs:5405`) does follow block power, in \
         all six directions, and its `mark_powered` even carries the \
         weak/strong split mechanism 4 turns on. But it is anchored at a \
         **gate's own support block**: it asks which nets reach *this torch*, \
         not whether any two nets became one somewhere else in the world. A \
         lever over a route, or a torch under one, that lands nowhere near a \
         gate's support is outside both invariants. That is the shape of the \
         two shipped bugs, and it is why they shipped.\n\n",
    );
}

fn generate() -> String {
    let mut out = header();
    summary(&mut out);
    table_one(&mut out);
    table_two(&mut out);
    table_three(&mut out);
    table_four(&mut out);
    table_five(&mut out);
    table_six(&mut out);
    out
}

fn artifact_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ARTIFACT_PATH)
}

/// The writer. `#[ignore]`d because it mutates the tree.
#[test]
#[ignore = "writes docs/derived/coupling-mechanisms.md"]
fn regenerate_the_coupling_tables() {
    let path = artifact_path();
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("create docs/derived");
    std::fs::write(&path, generate()).expect("write the artifact");
    eprintln!("wrote {}", path.display());
}

/// Rule 4's half of the pair: the committed table is checked against the
/// simulator on every run, so no mark in it can go stale silently.
#[test]
fn the_committed_table_is_what_the_simulator_says_today() {
    let path = artifact_path();
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must exist ({e}) -- regenerate it", path.display()));
    let fresh = generate();
    let committed = committed.replace("\r\n", "\n");
    if committed != fresh {
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
             Rerun the ignored `regenerate_the_coupling_tables` and read the diff \
             before committing it: this table is the enumeration of edge types a \
             realised world can contain.",
            path.display()
        );
    }
}

// ---------------------------------------------------------------------
// The claims the tables are worth nothing without
// ---------------------------------------------------------------------

/// The rig can see a coupling, and can see the absence of one.
///
/// Both halves through the same differential machinery every other row uses,
/// so every row inherits a rig known to work in both directions. The positive
/// case is exactly the first shipped bug: a lit lever, the block above it, and
/// a foreign wire on the far side.
#[test]
fn the_rig_reads_a_coupling_across_a_conductor_and_none_across_an_insulator() {
    let m = origin();
    let lever = Emitter::new(Kind::Lever, Facing::North);

    assert_eq!(
        measure_dust(lever, m.down(), Some((m, Fill::Stone)), m.up()).0,
        Coupling::Invisible,
        "a lit lever under a stone block must light a wire sitting on that block"
    );
    assert_eq!(
        measure_dust(lever, m.down(), Some((m, Fill::Glass)), m.up()).0,
        Coupling::No,
        "glass is a full cube that does not conduct, so the same rig must go dark"
    );
    assert_eq!(
        measure_dust(lever, m.down(), Some((m, Fill::Air)), m.up()).0,
        Coupling::No,
        "and with nothing in the middle there is no path at all"
    );
    assert_eq!(
        measure_dust(
            Emitter::new(Kind::Stone, Facing::North),
            m.down(),
            Some((m, Fill::Stone)),
            m.up()
        )
        .0,
        Coupling::No,
        "an inert block in the emitter cell must read `.` -- this is what says the \
         positive result above came from the lever and not from the rig"
    );
}

/// Both shipped bugs, reproduced as edges, and shown to be invisible to the
/// invariant that was supposed to catch them.
///
/// Bug one: a lit lever strongly powers the block above it. Bug two: a lit
/// gate torch strongly powers the block above it. In both, the coupled cells
/// are two apart with a block between, so `dust_connections` -- which only
/// ever returns dust neighbours -- cannot contain the edge, and
/// `verify_connectivity`'s walk cannot put the two cells in one component.
#[test]
fn both_shipped_bugs_are_edges_the_dust_walk_cannot_contain() {
    let m = origin();

    for emitter in [
        Emitter::new(Kind::Lever, Facing::North),
        Emitter::new(Kind::Torch, Facing::North),
        Emitter::new(Kind::WallTorch, Facing::North),
    ] {
        let (cell, mode) = measure_dust(emitter, m.down(), Some((m, Fill::Stone)), m.up());
        assert_eq!(
            cell,
            Coupling::Invisible,
            "a lit {} below a stone block must drive the wire above it, and \
             verify_connectivity's walk must not see the edge",
            emitter.kind.name()
        );
        assert_eq!(mode, Settled::Stable);

        // The same claim stated the other way round, so it is not an artefact
        // of how `measure` classifies: the walk genuinely does not join them.
        let world = build(
            emitter,
            m.down(),
            Some((m, Fill::Stone)),
            Receiver::Dust,
            m.up(),
            false,
        );
        assert!(
            !joined_by_the_dust_walk(&world, m.down(), m.up()),
            "the two cells must be in different dust components"
        );
    }
}

/// Every coupling that crosses a block is invisible to the walk, over the
/// whole of Table 2 -- not just at the two geometries the bugs happened to
/// take.
///
/// This is the claim the whole file exists to establish, so it is asserted
/// rather than left to be read off a table.
#[test]
fn no_coupling_across_a_block_is_ever_visible_to_the_dust_walk() {
    let mut coupled = 0usize;
    for emitter in emitter_configurations() {
        let (cells, _) = line_row(emitter, Some(Fill::Stone));
        for (d, cell) in ALL_SIX.into_iter().zip(cells) {
            assert_ne!(
                cell,
                Coupling::Visible,
                "{} facing {} coupled toward {} across a stone block and the dust \
                 walk claimed to see it -- dust_connections has no edge that leaves \
                 a dust cell for a block, so this cannot happen without the walk \
                 having changed",
                emitter.kind.name(),
                short(emitter.facing),
                short(d),
            );
            if cell.is_coupled() {
                coupled += 1;
            }
        }
    }
    assert!(
        coupled >= 8,
        "only {coupled} block-crossing couplings were found -- the rig has gone blind"
    );
}

/// A strongly powered block drives dust on **all** of its remaining faces, not
/// only the face pointing away from whatever powered it.
///
/// This is the half of mechanism 3 that makes it dangerous: the coupling is
/// not a straight line, so a route that merely passes a powered block on any
/// side picks it up. Measured against a lever below the block, the geometry of
/// the first shipped bug.
#[test]
fn a_strongly_powered_block_drives_dust_on_every_face_it_has_left() {
    let m = origin();
    let lever = Emitter::new(Kind::Lever, Facing::North);
    for d_out in face_outputs(Facing::Down) {
        assert_eq!(
            measure_dust(lever, m.down(), Some((m, Fill::Stone)), m.offset(d_out)).0,
            Coupling::Invisible,
            "a stone block strongly powered from below must drive the dust on its {} \
             face too",
            short(d_out),
        );
    }
}

/// Weak power stops at the block, as far as dust is concerned.
///
/// Measured in the one geometry where a wire actually powers the mediator:
/// standing on it. `dust_powers_block_toward` answers `Down` unconditionally,
/// so this does not depend on the wire's shape -- and the reading is `.` all
/// the same.
#[test]
fn a_block_powered_only_by_dust_drives_no_further_dust() {
    let m = origin();
    let wire = Emitter::new(Kind::Dust, Facing::Up);

    // The premise, so a `.` below cannot be a wire that was never powered.
    let world = build(
        wire,
        m.up(),
        Some((m, Fill::Stone)),
        Receiver::Dust,
        m.down(),
        false,
    );
    let (mode, settled) = settle(world);
    assert_eq!(mode, Settled::Stable);
    assert!(settled.get(m.x, m.y + 1, m.z).power > 0, "the driving wire must be lit");
    assert_eq!(
        block_signal_at(&settled, m),
        (BlockPower::Weak, 15),
        "and it must weakly power the block it stands on"
    );

    assert_eq!(
        measure_dust(wire, m.up(), Some((m, Fill::Stone)), m.down()).0,
        Coupling::No,
        "a block powered only weakly must not re-drive the dust beneath it -- \
         recompute_dust_strengths only ever seeds a wire from Strong block power"
    );
}

/// A strongly powered block cannot power the block beside it.
///
/// Rule 3 -- this is the "cannot be X" claim, and the callers are open:
/// `propagate::block_signal_at` reads its six neighbours through
/// `dust_power_toward`, which defers to `taxonomy::power_emitted_toward` for
/// anything that is not dust, and that function has no arm for
/// `BlockKind::Solid` at all. Asserted electrically as well, so the claim does
/// not rest on reading the match.
#[test]
fn a_strongly_powered_block_cannot_power_the_next_block() {
    let m = origin();
    let first = m;
    let second = m.up();
    let lever = Emitter::new(Kind::Lever, Facing::North);

    let mut world = build(
        lever,
        m.down(),
        Some((first, Fill::Stone)),
        Receiver::Dust,
        second.up(),
        false,
    );
    set(&mut world, second, Fill::Stone.state());
    let (mode, settled) = settle(world);
    assert_eq!(mode, Settled::Stable);

    assert_eq!(
        block_signal_at(&settled, first).0,
        BlockPower::Strong,
        "the first block must be strongly powered, or this test proves nothing"
    );
    assert_eq!(
        block_signal_at(&settled, second),
        (BlockPower::None, 0),
        "a strongly powered block must not power the block above it"
    );
    assert_eq!(
        settled.get(second.x, second.y + 1, second.z).power,
        0,
        "and so the dust on the far side of the second block stays dark"
    );
}

/// A torch never drives the block it is mounted on, at any facing -- and it
/// drives every other face it has.
///
/// Both kinds, and for the wall torch all four facings, so the withheld
/// direction is shown to *move with* `facing` rather than being a fixed
/// direction that happens to be right once. Measured directly (Table 1's
/// geometry) rather than across a block, because across a block the four
/// horizontal faces are `.` for a different reason -- weak power -- and a test
/// that could not tell the two apart would pass with the attachment rule
/// deleted.
#[test]
fn a_torch_never_powers_its_own_support() {
    let e = origin();

    for (emitter, support_direction) in [
        (Emitter::new(Kind::Torch, Facing::North), Facing::Down),
        (Emitter::new(Kind::WallTorch, Facing::North), Facing::South),
        (Emitter::new(Kind::WallTorch, Facing::South), Facing::North),
        (Emitter::new(Kind::WallTorch, Facing::East), Facing::West),
        (Emitter::new(Kind::WallTorch, Facing::West), Facing::East),
    ] {
        for d in ALL_SIX {
            let cell = measure_dust(emitter, e, None, e.offset(d)).0;
            if d == support_direction {
                assert!(
                    !cell.is_coupled(),
                    "a {} facing {} must not drive anything at {} -- that is its own \
                     support, got {cell:?}",
                    emitter.kind.name(),
                    short(emitter.facing),
                    short(d),
                );
            } else {
                assert_eq!(
                    cell,
                    Coupling::Invisible,
                    "a {} facing {} must drive the dust at {}",
                    emitter.kind.name(),
                    short(emitter.facing),
                    short(d),
                );
            }
        }
    }

    // The strong/weak split, which is what makes the same torch look inert on
    // four faces once a block is in the way. Only `Up` survives a block.
    for emitter in [
        Emitter::new(Kind::Torch, Facing::North),
        Emitter::new(Kind::WallTorch, Facing::East),
    ] {
        for d in ALL_SIX {
            let cell = measure_dust(emitter, e, Some((e.offset(d), Fill::Stone)), e.offset(d).offset(d)).0;
            assert_eq!(
                cell.is_coupled(),
                d == Facing::Up,
                "a {} must reach dust across a block on its Up face and nowhere else; \
                 {} read {cell:?}",
                emitter.kind.name(),
                short(d),
            );
        }
    }
}

/// A repeater powers exactly the block it faces, and a comparator likewise --
/// measured at all four facings, so the direction is shown to follow `facing`
/// instead of being fixed.
///
/// `facing` records the direction from the output side to the *input* side
/// (Minecraft Wiki, and `taxonomy::power_emitted_toward`'s own comment), so
/// the powered block is at `facing.opposite()`. The rear column is `x`: the
/// feed lives there, and a rig that fed a diode from its rear and then asked
/// what the diode does to its rear would be measuring the feed.
#[test]
fn a_diode_powers_exactly_the_block_in_front_of_it() {
    let e = origin();
    for kind in [Kind::Repeater, Kind::Comparator] {
        for facing in HORIZONTAL {
            let emitter = Emitter::new(kind, facing);
            for d in ALL_SIX {
                let mediator = Some((e.offset(d), Fill::Stone));
                let cell = measure_dust(emitter, e, mediator, e.offset(d).offset(d)).0;
                if d == facing {
                    assert_eq!(
                        cell,
                        Coupling::Invalid,
                        "the feed occupies a diode's rear, so that column cannot be measured"
                    );
                } else if d == facing.opposite() {
                    assert_eq!(
                        cell,
                        Coupling::Invisible,
                        "a {} facing {} must strongly power the block at {}",
                        kind.name(),
                        short(facing),
                        short(d),
                    );
                } else {
                    assert!(
                        !cell.is_coupled(),
                        "a {} facing {} must not power the block at {}, got {cell:?}",
                        kind.name(),
                        short(facing),
                        short(d),
                    );
                }
            }
        }
    }
}

/// `verify_connectivity`'s walk misses a **one-way** dust edge when its seed
/// order runs against the edge, and this is mechanism 1 -- the one relation the
/// walk exists to cover.
///
/// The mechanism, stated once: seeds arrive in
/// `world.positions_of(BlockKind::RedstoneWire)` order, which is flat-index
/// order, which is lowest `y` first; and a cell already claimed by an earlier
/// seed's component is skipped with `continue` before its owner is ever
/// compared. So when the only edge runs downward -- upper drives lower, lower
/// cannot climb back -- the lower cell is seeded first, walks nowhere, and the
/// upper cell then forms a component of its own that never meets it.
///
/// Both halves are asserted, so this is a demonstration rather than a claim
/// about a rule: the same two cells with a supporting floor under the upper
/// wire are joined in both directions and the walk sees them.
///
/// **NOT MEASURED: whether any circuit this compiler builds contains such a
/// pair.** The condition is that the upper wire's own floor does not carry a
/// dust step, and `tests/dust_join_relation.rs`'s `Case::as_built` reports the
/// router laying stone under every routed cell.
#[test]
fn a_one_way_dust_edge_can_land_in_two_components_of_the_walk() {
    let (upper, lower, mid, floor) = pair_cells();

    // The edge is real and it is one-way. Asked of `dust_connections` itself,
    // in the very world the electrical run uses.
    let world = build_pair(Fill::Air, Fill::Air, true, false);
    assert_eq!(world.get(floor.x, floor.y, floor.z).kind, BlockKind::Air);
    assert_eq!(world.get(mid.x, mid.y, mid.z).kind, BlockKind::Air);
    assert!(
        HORIZONTAL
            .iter()
            .any(|&d| dust_connections(&world, upper, d).iter().any(|p| p == lower)),
        "the upper wire must descend into the lower one"
    );
    assert!(
        HORIZONTAL
            .iter()
            .all(|&d| dust_connections(&world, lower, d).iter().all(|p| p != upper)),
        "and the lower one must not climb back -- its step has no floor to stand on"
    );

    // Electrically the two are one net in the direction that matters.
    assert_eq!(measure_pair(Fill::Air, Fill::Air, true), Coupling::Invisible);
    assert_eq!(measure_pair(Fill::Air, Fill::Air, false), Coupling::No);
    assert!(
        !joined_by_the_dust_walk(&world, upper, lower),
        "and the walk puts them in two components, so a Reservation disagreement \
         between them would never be compared"
    );

    // The control that makes the above a finding rather than a rig artefact:
    // give the upper wire a floor and the climb comes back, the edge becomes
    // two-way, and the walk sees it.
    let floored = build_pair(Fill::Stone, Fill::Air, true, false);
    assert!(
        HORIZONTAL
            .iter()
            .any(|&d| dust_connections(&floored, lower, d).iter().any(|p| p == upper)),
        "with a floor under the upper wire the climb must fire"
    );
    assert_eq!(
        measure_pair(Fill::Stone, Fill::Air, true),
        Coupling::Visible,
        "and the walk must then see the pair"
    );
    assert_eq!(measure_pair(Fill::Stone, Fill::Air, false), Coupling::Visible);
}

/// Weak power couples into a torch and into a diode's rear, and into nothing
/// else -- the class no dust probe can see.
///
/// The driver is a wire standing on the mediator, which is the only geometry
/// in which a wire powers a block at all above it. Strong power (a lever in
/// the same place) is measured alongside as the control that says the rig is
/// not simply blind to the weak case.
#[test]
fn weak_power_reaches_a_torchs_support_and_a_diodes_rear_but_never_dust() {
    let m = origin();
    let expected_under_weak = [
        (Receiver::Dust, false),
        (Receiver::Lamp, false),
        (Receiver::WallTorch(Facing::North), true),
        (Receiver::Repeater(Facing::South), true),
        (Receiver::Comparator(Facing::South), true),
    ];

    for &(receiver, r, _) in &receiver_slots(m) {
        let expected = expected_under_weak
            .iter()
            .find(|(candidate, _)| candidate == &receiver)
            .map(|&(_, expected)| expected)
            .expect("every slot is listed");

        let weak = measure(
            Emitter::new(Kind::Dust, Facing::Up),
            m.up(),
            Some((m, Fill::Stone)),
            receiver,
            r,
        )
        .0;
        assert_eq!(
            weak.is_coupled(),
            expected,
            "a weakly powered block and a {} receiver: got {weak:?}",
            receiver.name()
        );

        // Strong power, same geometry. Everything except the lamp couples --
        // the lamp is a block, and block-to-block power does not exist.
        let strong = measure(
            Emitter::new(Kind::Lever, Facing::Up),
            m.up(),
            Some((m, Fill::Stone)),
            receiver,
            r,
        )
        .0;
        assert_eq!(
            strong.is_coupled(),
            receiver != Receiver::Lamp,
            "a strongly powered block and a {} receiver: got {strong:?}",
            receiver.name()
        );
    }
}

/// A lever, a button and a pressure plate all deliver strong power in **all
/// six** directions in this simulator, and vanilla gives none of them that.
///
/// The cause is one line: `taxonomy::power_emitted_toward` has no arm for
/// these three, so they fall through `_ => full` and keep `power_emitted_by`'s
/// isotropic answer. Their `face` -- `Floor`, `Wall` or `Ceiling` -- is stored
/// on the `BlockState` (`block.rs`'s `Face`) and never read by the power
/// rules. In vanilla a `face=floor` lever strongly powers the block
/// **beneath** it and nothing else.
///
/// The divergence is in the safe direction for a coupling checker built on
/// this simulator: the simulator's edge set is a strict superset of vanilla's,
/// so a checker calibrated to the simulator cannot miss an edge vanilla has.
/// It can report edges vanilla does not have, which costs routes and never
/// costs correctness.
#[test]
fn the_taxonomy_gives_levers_buttons_and_plates_six_directional_strong_power() {
    for kind in [Kind::Lever, Kind::Button, Kind::PressurePlate] {
        let emitter = Emitter::new(kind, Facing::North);
        for d in ALL_SIX {
            assert_eq!(
                declared(emitter, d),
                "S",
                "{} must fall through to isotropic strong power toward {}",
                kind.name(),
                short(d),
            );
        }
        // And `face` really is set on the state the rig places, so the
        // divergence is a gap in the rules and not a gap in the fixture.
        if kind != Kind::PressurePlate {
            assert_eq!(emitter.state().face, Some(Face::Floor));
        }
    }

    // Measured, not just declared: the lever is the one of the three
    // `run_until_stable` will actually run, and it couples through the block on
    // every face.
    let m = origin();
    for d_out in face_outputs(Facing::Down) {
        assert_eq!(
            measure_dust(
                Emitter::new(Kind::Lever, Facing::North),
                m.down(),
                Some((m, Fill::Stone)),
                m.offset(d_out)
            )
            .0,
            Coupling::Invisible,
        );
    }
}

/// The simulator refuses a world containing a button or a pressure plate
/// outright, so those rows are `load-only` rather than settled.
///
/// Recorded rather than worked around: a checker that walks a realised world
/// must not assume it can settle one. The refusal is exact -- it names the
/// position and the block -- and it happens before anything is mutated, which
/// is why `Simulator::new`'s constructor-time recompute is still readable.
#[test]
fn run_until_stable_refuses_buttons_and_pressure_plates() {
    let m = origin();
    for kind in [Kind::Button, Kind::PressurePlate] {
        let emitter = Emitter::new(kind, Facing::North);
        let world = build(
            emitter,
            m.down(),
            Some((m, Fill::Stone)),
            Receiver::Dust,
            m.up(),
            false,
        );
        let mut simulator = Simulator::new(world);
        match simulator.run_until_stable(MAX_TICKS) {
            Err(SimulationError::UnsupportedComponent { position, name }) => {
                assert_eq!(position, m.down());
                assert_eq!(name, emitter.state().name);
            }
            other => panic!("{} must be refused, got {other:?}", kind.name()),
        }
        // The constructor-time recompute still happened, so the coupling is
        // measurable even though the world cannot be settled.
        assert!(
            simulator.world().get(m.x, m.y + 1, m.z).power > 0,
            "{}'s coupling through the block must still be readable load-only",
            kind.name()
        );
    }

    // The control: a lever, which is the same taxonomy arm, settles fine.
    let world = build(
        Emitter::new(Kind::Lever, Facing::North),
        m.down(),
        Some((m, Fill::Stone)),
        Receiver::Dust,
        m.up(),
        false,
    );
    let mut simulator = Simulator::new(world);
    assert!(simulator.run_until_stable(MAX_TICKS).is_ok());
}

/// The conductivity gate is what decides whether a block can carry a coupling,
/// and it is `flags_of(..).is_conductive()` -- not "is it a full cube".
///
/// Glass is the separating case: a full cube that does not conduct. Measured
/// electrically and cross-checked against the flag, so the tables' `glass`
/// rows cannot be read as "glass is not a block".
#[test]
fn only_a_conducting_mediator_carries_the_coupling() {
    let m = origin();
    let lever = Emitter::new(Kind::Lever, Facing::North);
    for fill in MEDIATORS {
        // Any of the five faces, not one chosen face: a dust mediator carries
        // to its four horizontal neighbours and not to the cell directly above
        // it (pure-vertical pairs never join -- `dust_join_relation.md`), so
        // asking about one face alone would be asking about that face's
        // geometry rather than about the material.
        let carries = face_outputs(Facing::Down).into_iter().any(|d_out| {
            measure_dust(lever, m.down(), Some((m, fill)), m.offset(d_out))
                .0
                .is_coupled()
        });
        let conductive = flags_of(&fill.state()).is_conductive();
        // Dust in the mediator cell is not a block at all: it carries the
        // signal as dust, by mechanism 2 followed by mechanism 1, so it is
        // expected to carry while reading non-conductive.
        if fill == Fill::Dust {
            assert!(
                carries && !conductive,
                "a dust mediator must carry without conducting -- it is not a block"
            );
            continue;
        }
        assert_eq!(
            carries, conductive,
            "the {} mediator carried={carries} but is_conductive={conductive}",
            fill.name()
        );
    }
    // And the flag really does separate the two full cubes, so the rows above
    // are a measurement of conductivity and not of shape.
    assert!(flags_of(&Fill::Stone.state()).can_carry_dust());
    assert!(flags_of(&Fill::Glass.state()).can_carry_dust());
    assert!(flags_of(&Fill::Stone.state()).is_conductive());
    assert!(!flags_of(&Fill::Glass.state()).is_conductive());
}
