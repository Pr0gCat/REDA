//! What actually crosses the boundary between REDA's last cell and a cell
//! REDA does not own -- **measured** against this crate's simulator, not
//! asserted from memory.
//!
//! `docs/superpowers/specs/2026-08-30-io-terminals.md` gives a pinned port's
//! cell to the caller and promises, in the game's own words, that the cell is
//! *powered* when the signal is high and that whatever the caller does to
//! power it is *read* as high. The spec then refuses to say which
//! constructions make either true: "every claim above is a requirement to be
//! checked against the simulator's own power model in `src/redstone` and
//! pinned by tests". This file is that check. Nothing here changes a
//! terminal; every test builds a small world, runs the real `Simulator` to a
//! stable state, and reports what the model says.
//!
//! ## The rig
//!
//! A pin is `(at, toward)`. `P` ([`pinned_cell`]) is the caller's cell, and
//! `toward` is **the direction the signal travels through it**, which is a
//! requirement and not a hint: it names the single cell REDA may build in.
//!
//! * **Output** -- the signal leaves the circuit heading `toward`, so REDA
//!   drives `P` from `P - toward` and its own net continues back at
//!   `P - 2*toward`.
//! * **Input** -- the signal enters heading `toward`, so REDA reads `P` from
//!   `P + toward` and hands the result on at `P + 2*toward`.
//!
//! Either way the handover is **collinear** with `toward` and there is
//! exactly one of it ([`handover`]); every measurement below sweeps `toward`
//! over all four horizontal directions, because a pin may name any of them
//! and a construction that only works along one axis is not a construction.
//!
//! Feeds are always a **redstone block**: the one source in the vocabulary
//! that drives adjacent dust while powering no block at all
//! (`taxonomy::power_emitted_by`'s `RedstoneBlock` arm), so a feed can never
//! leak into the thing being measured. Supports under cells that a real build
//! would have to stand on are **glass**: a full cube that carries dust and
//! repeaters but does not conduct, so a floor is never a second, unnamed
//! conductor (the trick `lever_footprint`'s doc comment used to isolate its
//! own hazard).
//!
//! Probe cells are bare dust with nothing beneath them. Support is not part
//! of the power model -- `recompute_dust_strengths`, `dust_sides` and
//! `block_signal_at` never ask whether a dust cell is standing on anything;
//! only `dust_connections`' climb and descend rules consult a *horizontal*
//! neighbour's top face. A floating probe therefore reads exactly what a
//! supported one would, and giving every probe the same (absent) floor keeps
//! the six neighbours of `P` comparable when one of them is `P.down()`.
//!
//! ## What the measurements say
//!
//! **Delivery.** "Powered" is not a property of a cell; it is a property of
//! what stands in it. The receivers disagree with each other under the same
//! handover, and the disagreements are not small:
//!
//! | handover at `P - toward` | lamp at `P` | solid block at `P` | dust at `P` |
//! |--------------------------|-------------|--------------------|-------------|
//! | repeater aimed at `P`    | lit         | Strong 15          | 15          |
//! | straight dust run        | lit         | Weak 15            | 14          |
//! | dust with a bend         | **dark**    | **None 0**         | 14          |
//! | dust joined on its side  | **dark**    | **None 0**         | 14          |
//! | lit torch                | lit         | Weak 15            | 15          |
//! | strongly powered block   | **dark**    | **None 0**         | 15          |
//! | redstone block           | **dark**    | **None 0**         | 15          |
//! | lit lever                | lit         | Strong 15          | 15          |
//!
//! The two receivers left out of the table are the ones with nothing to
//! report: an empty cell reads exactly as it does with no handover at all,
//! and a glass cube reads unpowered under a repeater emitting 15 straight
//! into it, because `block_signal_at` gates on conductivity first
//! ([`an_empty_pinned_cell_reads_the_same_however_hard_reda_delivers`]). That
//! is why verification cannot check an output by inspecting the shipped
//! world: the cell the contract is about ships empty.
//!
//! Only the repeater (and the lever, which is a source and not something a
//! compiler may emit mid-circuit) satisfies the contract for every receiver,
//! and only the repeater does so *by construction*: it emits toward exactly
//! one cell, always at strength 15, whatever stands there. Everything the
//! dust rows lose, it loses to *shape*, and shape is decided by cells the
//! router is free to fill -- so a dust handover's promise is a promise about
//! the whole neighbourhood, while the repeater's is a promise about one
//! blockstate.
//!
//! **Sensing.** A repeater whose rear faces `P` reads every state a caller
//! can put in their cell, including two that dust beside `P` cannot see at
//! all -- a dust run spent down to strength 1, and a weakly powered block --
//! and it hands REDA's net a full 15 in every case, so source strength stops
//! being arithmetic and becomes geometry.
//!
//! **The same blockstate serves both roles.** A terminal repeater is
//! `facing = toward.opposite()` whichever side of it the caller is on
//! ([`a_terminal_repeater_is_the_same_blockstate_in_both_roles`]): the signal
//! travels `toward` through all three cells, and only the label on the middle
//! one changes.
//!
//! **Coupling.** The delivery repeater leaks nothing sideways, and nothing
//! the caller does to `P` reaches back through it. The caller's cell is the
//! leak, and the two directions are not symmetric:
//!
//! * REDA delivering into a conductive `P` makes `P` a strength-15 source for
//!   **dust** touching it, and stops there -- a *block* touching `P` stays
//!   inert, because blocks do not power blocks.
//! * The caller powering `P` with a *source* makes every conductive block
//!   touching `P` a source in its own right, which drives dust one cell
//!   further out again -- including the cell beside REDA's own handover.
//!
//! Stated as coordinates rather than depths, which is what a keep-out has to
//! be. Writing `h` for the step from `P` to REDA's handover
//! (`toward.opposite()` for an output, `toward` for an input), REDA's dust
//! must stay out of:
//!
//! * `P + d`, for every direction `d` but `h` -- always, because REDA's own
//!   delivery makes `P` a source for dust; and
//! * `P + d + h`, the cells flanking the handover -- because a conductive
//!   block the caller is entitled to place at `P + d` is one step from them,
//!   and carries a caller source straight into them
//!   ([`a_caller_conductor_beside_the_pinned_cell_reaches_dust_beside_the_handover`],
//!   measured in both roles).
//!
//! The second clause is unavoidable for an input, where the caller's source
//! is the whole point. For an output it bites only if the caller puts a
//! source of their own beside `P`, which the contract permits and does not
//! promise against.
//!
//! ## The recommendation these measurements support
//!
//! * **Deliver** with a repeater at `at - toward`, `facing = toward.opposite()`,
//!   fed from `at - 2*toward`. That rear cell is an ordinary net cell: it
//!   accepts a bent run, a T, and a run spent all the way down to strength 1,
//!   and delivers a flat 15 regardless
//!   ([`a_delivery_repeaters_rear_is_an_ordinary_net_cell`]). The pin costs
//!   the router one approach, and the repeater gives it straight back.
//! * **Sense** with a repeater at `at + toward`, `facing = toward.opposite()`,
//!   whose output cell `at + 2*toward` is the route's source at a flat 15.
//! * **The price**, measured and fixed: one redstone tick per terminal. A
//!   lamp in `P` behind a straight dust handover settles one game tick after
//!   the feed changes; behind the repeater, three
//!   ([`a_repeater_handover_costs_one_redstone_tick_more_than_dust`]). Nothing
//!   about it scales with the circuit.
//! * **Fixtures**: drive a pinned input with a **redstone block**, not a
//!   lever. Measured here, a lit lever is strong on all six neighbours, so
//!   its keep-out is two cells deep where a redstone block's is one -- and a
//!   redstone block drives dust just as hard in every direction, which is all
//!   a fixture has to do. Not measured here but worth the design's attention:
//!   a lever also needs an attachment face (`world::block::Face`), and the
//!   contract promises no attachment face: the other five neighbours may hold
//!   inert primitive support, floor, or fill, but no signal-carrying REDA cell.
//!   The simulator does not model placement legality, and `taxonomy`'s
//!   `air_supports_nothing` confirms that an air cell cannot provide a mount,
//!   so a fixture cannot rely on one. A redstone block needs no support at all.
//!   Read a pinned output with a **lamp**: no support either, and it is the
//!   receiver a dust handover fails while a dust probe would not notice.
//! * **The limit of the measurement**, stated rather than hidden: the
//!   contract names a piston among the things a caller might attach, and this
//!   simulator refuses pistons, observers, buttons and pressure plates
//!   outright ([`the_simulator_refuses_to_answer_for_a_piston_in_the_pinned_cell`]).
//!   Everything above is measured on what it does model.

use reda::redstone::rules::taxonomy::BlockPower;
use reda::redstone::simulator::component::repeater_input_position;
use reda::redstone::simulator::connectivity::dust_powers_block_toward;
use reda::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use reda::redstone::simulator::propagate::block_signal_at;
use reda::redstone::simulator::{SimulationError, Simulator};
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

/// Generous: the deepest rig is a fifteen-cell dust run plus one repeater.
const MAX_TICKS: u64 = 400;

/// Wide enough for the caller to build seventeen cells of their own away from
/// `P` along any horizontal axis, and four cells of REDA in every other
/// direction. `P` sits in the middle so the sweep over `toward` is symmetric
/// -- an out-of-range `World::set` is silently dropped, which would quietly
/// turn a measurement into a measurement of nothing.
const SIZE: (i32, i32, i32) = (41, 9, 41);
const ORIGIN: (i32, i32, i32) = (20, 4, 20);

/// The caller's cell.
fn pinned_cell() -> Position {
    Position::new(ORIGIN.0, ORIGIN.1, ORIGIN.2)
}

fn empty_world() -> World {
    World::new(SIZE.0, SIZE.1, SIZE.2)
}

fn put(world: &mut World, at: Position, state: BlockState) {
    assert!(
        world.index(at.x, at.y, at.z).is_some(),
        "{at:?} is outside the rig's world -- `World::set` would drop it silently"
    );
    world.set(at.x, at.y, at.z, state);
}

fn settled(world: World) -> World {
    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("every rig in this file is a handful of cells and must settle");
    simulator.world().clone()
}

/// The two horizontal directions perpendicular to `facing`.
fn perpendicular(facing: Facing) -> [Facing; 2] {
    match facing {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        _ => [Facing::North, Facing::South],
    }
}

/// `steps` cells along `direction` from `P`.
fn along(direction: Facing, steps: i32) -> Position {
    let mut cell = pinned_cell();
    for _ in 0..steps {
        cell = cell.offset(direction);
    }
    cell
}

// ---------------------------------------------------------------------
// The pin's geometry
// ---------------------------------------------------------------------

/// Which way the signal is crossing the boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// The signal leaves the circuit heading `toward`.
    Output,
    /// The signal enters the circuit heading `toward`.
    Input,
}

/// The **one** cell the pin leaves REDA, for this role.
///
/// Not a choice: the signal's direction of travel decides which side of the
/// caller's cell REDA is on.
fn handover(toward: Facing, role: Role) -> Position {
    match role {
        Role::Output => pinned_cell().offset(toward.opposite()),
        Role::Input => pinned_cell().offset(toward),
    }
}

/// REDA's first *own* net cell, one step further along the signal's travel
/// from the handover: the rear a delivery repeater reads, or the cell an
/// input repeater's output lands in.
fn net_cell(toward: Facing, role: Role) -> Position {
    match role {
        Role::Output => handover(toward, role).offset(toward.opposite()),
        Role::Input => handover(toward, role).offset(toward),
    }
}

// ---------------------------------------------------------------------
// Block vocabulary
// ---------------------------------------------------------------------

fn named(name: &str, kind: BlockKind) -> BlockState {
    let mut state = BlockState::air();
    state.kind = kind;
    state.name = name.to_string();
    state
}

fn stone() -> BlockState {
    named("minecraft:stone", BlockKind::Solid)
}

/// A full cube that carries dust, repeaters and torches but does **not**
/// conduct -- so a floor made of it can never be a second conductor in a
/// measurement.
fn glass() -> BlockState {
    named("minecraft:glass", BlockKind::Glass)
}

fn dust() -> BlockState {
    named("minecraft:redstone_wire", BlockKind::RedstoneWire)
}

fn lamp() -> BlockState {
    named("minecraft:redstone_lamp", BlockKind::Lamp)
}

fn redstone_block() -> BlockState {
    named("minecraft:redstone_block", BlockKind::RedstoneBlock)
}

fn lit_lever() -> BlockState {
    let mut state = named("minecraft:lever", BlockKind::Lever);
    state.face = Some(Face::Floor);
    state.lit = true;
    state
}

fn lit_torch() -> BlockState {
    let mut state = named("minecraft:redstone_torch", BlockKind::Torch);
    state.lit = true;
    state
}

/// A repeater whose *input* is the neighbour in `facing` and whose output
/// lands in `facing.opposite()` -- the wiki convention this crate pins in
/// `simulator::tests::a_north_facing_repeater_matches_the_wiki_convention`.
fn repeater(facing: Facing) -> BlockState {
    let mut state = named("minecraft:repeater", BlockKind::Repeater);
    state.facing = Some(facing);
    state.delay = 1;
    state
}

/// The repeater a terminal is built from: input upstream, output downstream,
/// both along `toward`. The same blockstate serves an input and an output --
/// see [`a_terminal_repeater_is_the_same_blockstate_in_both_roles`].
fn terminal_repeater(toward: Facing) -> BlockState {
    repeater(toward.opposite())
}

// ---------------------------------------------------------------------
// Delivery: what REDA can put in the handover cell
// ---------------------------------------------------------------------

/// Everything that could plausibly stand in the handover cell and try to
/// power the caller's cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handover {
    /// The control: nothing at all in the handover cell.
    Nothing,
    /// Dust fed from directly behind, so its run points straight at `P`.
    StraightDust,
    /// The same dust, fed from a perpendicular side instead -- one bend.
    BentDust,
    /// The straight run, plus one unrelated dust cell touching its side --
    /// what any route passing the terminal would look like.
    SideJoinedDust,
    /// A repeater whose output cell is `P`.
    Repeater,
    /// A standing torch on a non-conductive support, so it can never go out.
    Torch,
    /// A solid block strongly powered by a repeater behind it.
    PoweredBlock,
    RedstoneBlock,
    Lever,
}

/// Build the delivery construction at `P - toward`, with its feed laid
/// further upstream so the feed is never adjacent to `P` or to the receiver.
fn place_handover(world: &mut World, toward: Facing, construction: Handover) {
    let up = toward.opposite();
    let n = handover(toward, Role::Output);
    let feed = net_cell(toward, Role::Output);
    let far = feed.offset(up);

    match construction {
        Handover::Nothing => {}
        Handover::StraightDust => {
            put(world, n.down(), glass());
            put(world, n, dust());
            put(world, feed, redstone_block());
        }
        Handover::BentDust => {
            put(world, n.down(), glass());
            put(world, n, dust());
            put(world, n.offset(perpendicular(toward)[0]), redstone_block());
        }
        Handover::SideJoinedDust => {
            put(world, n.down(), glass());
            put(world, n, dust());
            put(world, feed, redstone_block());
            // An unrelated wire brushing past. It carries nothing of its own;
            // all it does is exist beside the handover.
            let beside = n.offset(perpendicular(toward)[0]);
            put(world, beside.down(), glass());
            put(world, beside, dust());
        }
        Handover::Repeater => {
            put(world, n.down(), glass());
            put(world, n, terminal_repeater(toward));
            put(world, feed, redstone_block());
        }
        Handover::Torch => {
            // Glass, not stone: a non-conductive support can never be
            // powered, so the torch stays lit for the whole measurement.
            put(world, n.down(), glass());
            put(world, n, lit_torch());
        }
        Handover::PoweredBlock => {
            put(world, n, stone());
            put(world, feed.down(), glass());
            put(world, feed, terminal_repeater(toward));
            put(world, far, redstone_block());
        }
        Handover::RedstoneBlock => put(world, n, redstone_block()),
        Handover::Lever => put(world, n, lit_lever()),
    }
}

/// What the caller might have put in their own cell for REDA to power.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receiver {
    Lamp,
    /// A plain conductive full cube -- the caller's own wire support, or the
    /// block a piston or door would read.
    Block,
    Dust,
    /// Nothing at all: the cell the contract says ships empty.
    Air,
    /// A full cube that does not conduct.
    Glass,
}

fn place_receiver(world: &mut World, receiver: Receiver) {
    let p = pinned_cell();
    match receiver {
        Receiver::Lamp => put(world, p, lamp()),
        Receiver::Block => put(world, p, stone()),
        Receiver::Dust => {
            put(world, p.down(), glass());
            put(world, p, dust());
        }
        Receiver::Air => {}
        Receiver::Glass => put(world, p, glass()),
    }
}

/// What the caller's cell reports after the world settles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// A lamp, lit or dark -- the receiver that answers the contract's own
    /// word "powered" most directly.
    Lamp(bool),
    /// A block, as `propagate::block_signal_at` reports it.
    Block(BlockPower, u8),
    /// Dust, at its settled strength.
    Dust(u8),
    /// There is nothing in the cell that can hold a reading.
    Nothing,
}

fn read_receiver(world: &World, receiver: Receiver) -> Reading {
    let p = pinned_cell();
    match receiver {
        Receiver::Lamp => Reading::Lamp(world.get(p.x, p.y, p.z).lit),
        Receiver::Block | Receiver::Glass => {
            let (kind, strength) = block_signal_at(world, p);
            Reading::Block(kind, strength)
        }
        Receiver::Dust => Reading::Dust(world.get(p.x, p.y, p.z).power),
        Receiver::Air => Reading::Nothing,
    }
}

/// Run one delivery experiment: this construction, this receiver, this
/// direction of travel.
fn delivered(toward: Facing, construction: Handover, receiver: Receiver) -> Reading {
    let mut world = empty_world();
    place_receiver(&mut world, receiver);
    place_handover(&mut world, toward, construction);
    let world = settled(world);
    read_receiver(&world, receiver)
}

// ---------------------------------------------------------------------
// Sensing: what REDA can read of the caller's cell
// ---------------------------------------------------------------------

/// Everything a caller might do to their own cell to mean "high".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallerState {
    /// The control: the cell as it ships.
    Nothing,
    Lever,
    RedstoneBlock,
    Torch,
    /// Dust in `P` at full strength.
    FullDust,
    /// Dust in `P` at strength 1 -- the far end of a run the caller allowed
    /// to decay.
    SpentDust,
    /// A solid block in `P` strongly powered by the caller's own repeater.
    StronglyPoweredBlock,
    /// A solid block in `P` weakly powered by the caller's own dust run.
    WeaklyPoweredBlock,
}

/// Build the caller's construction. Everything but `P` itself is laid
/// **upstream** -- against `toward`, the way the caller's own signal
/// approaches -- so none of it ever touches a cell REDA might use.
fn place_caller(world: &mut World, toward: Facing, caller: CallerState) {
    let p = pinned_cell();
    let up = toward.opposite();
    let out = |steps: i32| along(up, steps);

    match caller {
        CallerState::Nothing => {}
        CallerState::Lever => put(world, p, lit_lever()),
        CallerState::RedstoneBlock => put(world, p, redstone_block()),
        CallerState::Torch => {
            put(world, p.down(), glass());
            put(world, p, lit_torch());
        }
        CallerState::FullDust => {
            put(world, p.down(), glass());
            put(world, p, dust());
            put(world, out(1), redstone_block());
        }
        CallerState::SpentDust => {
            // Fifteen dust cells from the source: 15 at the first, 1 at `P`.
            for step in 0..=14 {
                put(world, out(step).down(), glass());
                put(world, out(step), dust());
            }
            put(world, out(15), redstone_block());
        }
        CallerState::StronglyPoweredBlock => {
            put(world, p, stone());
            put(world, out(1).down(), glass());
            // Input further upstream, output into `P`.
            put(world, out(1), repeater(up));
            put(world, out(2), redstone_block());
        }
        CallerState::WeaklyPoweredBlock => {
            put(world, p, stone());
            // A straight run pointing into `P`: two cells of dust and the
            // feed beyond them, so nothing bends and the block at the end of
            // the run is the one the run powers.
            for step in 1..=2 {
                put(world, out(step).down(), glass());
                put(world, out(step), dust());
            }
            put(world, out(3), redstone_block());
        }
    }
}

/// What REDA puts in the handover cell to read the caller's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    /// Plain dust -- REDA's net running right up to the boundary.
    Dust,
    /// A repeater whose rear input is the caller's cell.
    RepeaterRear,
}

/// The strength that lands on REDA's first *own* net cell, `P + 2*toward`.
///
/// One number for both readers, deliberately: it is exactly what the rest of
/// the circuit gets to work with, and it is where the difference between
/// "reads the caller" and "normalizes the caller" shows up.
fn sensed_strength(toward: Facing, caller: CallerState, reader: Reader) -> u8 {
    let n = handover(toward, Role::Input);
    let inward = net_cell(toward, Role::Input);

    let mut world = empty_world();
    place_caller(&mut world, toward, caller);
    put(&mut world, n.down(), glass());
    put(&mut world, inward.down(), glass());
    match reader {
        Reader::Dust => put(&mut world, n, dust()),
        Reader::RepeaterRear => put(&mut world, n, terminal_repeater(toward)),
    }
    put(&mut world, inward, dust());

    let world = settled(world);
    world.get(inward.x, inward.y, inward.z).power
}

// ---------------------------------------------------------------------
// 0. The geometry the pin fixes
// ---------------------------------------------------------------------

/// The sign convention, stated once so nothing below has to be read twice.
/// `toward` is the direction the signal travels *through* the caller's cell,
/// so the handover is always the cell the signal is on the other side of --
/// upstream for an output, downstream for an input -- and the two roles put
/// REDA on opposite sides of the same axis.
#[test]
fn toward_names_one_handover_cell_per_role_and_they_are_opposite() {
    let p = pinned_cell();
    for toward in HORIZONTAL {
        assert_eq!(
            handover(toward, Role::Output),
            p.offset(toward.opposite()),
            "an output's signal arrives from upstream, so REDA drives from there"
        );
        assert_eq!(
            handover(toward, Role::Input),
            p.offset(toward),
            "an input's signal continues downstream, so REDA reads from there"
        );
        assert_eq!(
            handover(toward, Role::Output).offset(toward).offset(toward),
            handover(toward, Role::Input),
            "the two handovers straddle the caller's cell on the `toward` axis"
        );

        // Everything else is the caller's to build in, and REDA may not.
        let reda = [handover(toward, Role::Output), handover(toward, Role::Input)];
        for direction in ALL_SIX {
            let neighbour = p.offset(direction);
            let claimed = reda.contains(&neighbour);
            assert_eq!(
                claimed,
                direction == toward || direction == toward.opposite(),
                "toward {toward:?}: only the `toward` axis may be REDA's, and \
                 {direction:?} is {}",
                if claimed { "claimed" } else { "free" }
            );
        }
    }
}

/// A terminal is one blockstate, not two. Whichever role it plays, the
/// repeater's input is the cell upstream along `toward` and its output the
/// cell downstream -- so an output terminal reads REDA and writes the caller,
/// an input terminal reads the caller and writes REDA, and the *block* is
/// identical.
#[test]
fn a_terminal_repeater_is_the_same_blockstate_in_both_roles() {
    for toward in HORIZONTAL {
        let state = terminal_repeater(toward);
        for role in [Role::Output, Role::Input] {
            let n = handover(toward, role);
            assert_eq!(
                repeater_input_position(&state, n),
                Some(n.offset(toward.opposite())),
                "toward {toward:?}, {role:?}: the terminal repeater reads upstream"
            );
            let output = n.offset(toward);
            assert!(
                output == pinned_cell() || output == net_cell(toward, role),
                "toward {toward:?}, {role:?}: its output is the next cell along \
                 the signal's travel"
            );
        }
        assert_eq!(
            handover(toward, Role::Output).offset(toward),
            pinned_cell(),
            "an output terminal writes the caller's cell"
        );
        assert_eq!(
            handover(toward, Role::Input).offset(toward.opposite()),
            pinned_cell(),
            "an input terminal reads it"
        );
    }
}

// ---------------------------------------------------------------------
// 1. Controls: the rigs are in the states their rows name
//
// Without these, half the measurements below could pass for the wrong
// reason. "The bent run leaves the lamp dark" says nothing if the bent run
// was never powered; "a run spent to 1 dies at the boundary" says nothing if
// the caller's run was actually dead at 0.
// ---------------------------------------------------------------------

/// Every caller construction stands at exactly the strength and kind of power
/// its row in [`dust_beside_the_pinned_cell_misses_the_weak_and_the_nearly_spent`]
/// claims -- measured with no reader present at all, so nothing REDA does can
/// be propping it up.
#[test]
fn every_caller_construction_reaches_the_state_its_row_names() {
    let p = pinned_cell();
    for toward in HORIZONTAL {
        let built = |caller: CallerState| {
            let mut world = empty_world();
            place_caller(&mut world, toward, caller);
            settled(world)
        };

        assert_eq!(
            built(CallerState::FullDust).get(p.x, p.y, p.z).power,
            15,
            "toward {toward:?}: the full-dust caller must actually be at 15"
        );
        assert_eq!(
            built(CallerState::SpentDust).get(p.x, p.y, p.z).power,
            1,
            "toward {toward:?}: the spent-dust caller must be at exactly 1 -- at \
             0 the boundary measurement would be about a dead wire, and at 2 it \
             would be about nothing at all"
        );
        assert_eq!(
            block_signal_at(&built(CallerState::StronglyPoweredBlock), p),
            (BlockPower::Strong, 15),
            "toward {toward:?}: the strongly powered caller block must really be strong"
        );
        assert_eq!(
            block_signal_at(&built(CallerState::WeaklyPoweredBlock), p),
            (BlockPower::Weak, 14),
            "toward {toward:?}: and the weakly powered one really weak, at the \
             run's own decayed strength"
        );
        assert!(
            built(CallerState::Torch).get(p.x, p.y, p.z).lit,
            "toward {toward:?}: the caller's torch must stay lit -- its support \
             is glass and can never be powered"
        );
        assert!(built(CallerState::Lever).get(p.x, p.y, p.z).lit);
        assert_eq!(
            built(CallerState::Nothing).get(p.x, p.y, p.z).kind,
            BlockKind::Air,
            "toward {toward:?}: the control must really be an empty cell"
        );
    }
}

/// Every delivery construction is switched on, and the three dust variants
/// differ *only* in shape. This is what makes
/// [`a_dust_handover_powers_the_pinned_cell_only_while_its_run_stays_straight`]
/// a statement about direction rather than about a wire that never lit.
#[test]
fn every_handover_construction_reaches_the_state_its_row_names() {
    for toward in HORIZONTAL {
        let n = handover(toward, Role::Output);
        // The caller's cell as it ships -- empty -- so the shape readings
        // below are the ones the emitted world actually has.
        let built = |construction: Handover| {
            let mut world = empty_world();
            place_handover(&mut world, toward, construction);
            settled(world)
        };

        assert!(
            built(Handover::Repeater).get(n.x, n.y, n.z).lit,
            "toward {toward:?}: the delivery repeater must be switched on"
        );

        let straight = built(Handover::StraightDust);
        assert_eq!(
            straight.get(n.x, n.y, n.z).power,
            15,
            "toward {toward:?}: the straight run must be carrying full strength"
        );
        assert!(
            dust_powers_block_toward(&straight, n, toward),
            "toward {toward:?}: and must be pointing at the caller"
        );

        for bent in [Handover::BentDust, Handover::SideJoinedDust] {
            let world = built(bent);
            assert_eq!(
                world.get(n.x, n.y, n.z).power,
                15,
                "toward {toward:?}, {bent:?}: carries exactly the same strength \
                 -- the only thing that differs is its shape"
            );
            assert!(
                !dust_powers_block_toward(&world, n, toward),
                "toward {toward:?}, {bent:?}: and that shape has no direction left"
            );
        }

        assert!(
            built(Handover::Torch).get(n.x, n.y, n.z).lit,
            "toward {toward:?}: the torch must be lit"
        );
        assert_eq!(
            block_signal_at(&built(Handover::PoweredBlock), n),
            (BlockPower::Strong, 15),
            "toward {toward:?}: the powered block must really be strongly powered"
        );
    }
}

// ---------------------------------------------------------------------
// 2. Delivery
// ---------------------------------------------------------------------

/// The one construction whose promise is geometric. A lit repeater emits
/// toward exactly one cell (`taxonomy::power_emitted_toward`'s diode arm) at
/// a fixed strength 15, and it does so whatever stands in that cell -- so
/// every receiver a caller could plausibly attach reads high, and none of
/// them can change the answer by being what they are.
#[test]
fn a_repeater_aimed_at_the_pinned_cell_powers_every_receiver_alike() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::Repeater, Receiver::Lamp),
            Reading::Lamp(true),
            "toward {toward:?}: a lamp in the caller's cell must light"
        );
        assert_eq!(
            delivered(toward, Handover::Repeater, Receiver::Block),
            Reading::Block(BlockPower::Strong, 15),
            "toward {toward:?}: a solid block must be *strongly* powered, so \
             the caller may build on from it"
        );
        assert_eq!(
            delivered(toward, Handover::Repeater, Receiver::Dust),
            Reading::Dust(15),
            "toward {toward:?}: the caller's own wire must start at full strength"
        );
    }
}

/// The control every delivery row is a difference against.
#[test]
fn an_empty_handover_cell_leaves_the_pinned_cell_unpowered() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::Nothing, Receiver::Lamp),
            Reading::Lamp(false)
        );
        assert_eq!(
            delivered(toward, Handover::Nothing, Receiver::Block),
            Reading::Block(BlockPower::None, 0)
        );
        assert_eq!(
            delivered(toward, Handover::Nothing, Receiver::Dust),
            Reading::Dust(0)
        );
    }
}

/// Dust delivers, but only while its own shape stays a straight run aimed at
/// the caller -- and shape is decided by cells the router is free to fill,
/// so the promise is about a neighbourhood rather than a block. Worse, the
/// failure is *selective*: a bent run still feeds the caller's dust
/// (dust-to-dust connection is unconditional at the same layer) while leaving
/// their lamp dark, so a circuit could pass a probe made of dust and fail the
/// demo made of lamps.
#[test]
fn a_dust_handover_powers_the_pinned_cell_only_while_its_run_stays_straight() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::StraightDust, Receiver::Lamp),
            Reading::Lamp(true),
            "toward {toward:?}: a straight run does light the caller's lamp"
        );
        assert_eq!(
            delivered(toward, Handover::StraightDust, Receiver::Block),
            Reading::Block(BlockPower::Weak, 15),
            "toward {toward:?}: but only *weakly* -- the caller cannot build \
             a wire onward from that block"
        );
        assert_eq!(
            delivered(toward, Handover::StraightDust, Receiver::Dust),
            Reading::Dust(14),
            "toward {toward:?}: and the caller's own wire starts one short"
        );

        for bent in [Handover::BentDust, Handover::SideJoinedDust] {
            assert_eq!(
                delivered(toward, bent, Receiver::Lamp),
                Reading::Lamp(false),
                "toward {toward:?}, {bent:?}: one join anywhere on the handover \
                 cell costs the run its direction and the caller's lamp goes dark"
            );
            assert_eq!(
                delivered(toward, bent, Receiver::Block),
                Reading::Block(BlockPower::None, 0),
                "toward {toward:?}, {bent:?}: the same join leaves a solid block \
                 completely inert"
            );
            assert_eq!(
                delivered(toward, bent, Receiver::Dust),
                Reading::Dust(14),
                "toward {toward:?}, {bent:?}: yet the caller's *dust* is fed \
                 exactly as before -- the failure is invisible to a dust probe"
            );
        }
    }
}

/// A torch delivers to everything, but never strongly: `power_emitted_toward`
/// gives a standing torch `Strong` upward only and `Weak` sideways, so a
/// solid block in the caller's cell is powered in the sense a lamp or a
/// piston sees and not in the sense that lets the caller carry the signal on
/// through that block.
#[test]
fn a_torch_beside_the_pinned_cell_powers_it_only_weakly() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::Torch, Receiver::Lamp),
            Reading::Lamp(true)
        );
        assert_eq!(
            delivered(toward, Handover::Torch, Receiver::Block),
            Reading::Block(BlockPower::Weak, 15),
            "toward {toward:?}: sideways a torch is weak, however bright it is"
        );
        assert_eq!(
            delivered(toward, Handover::Torch, Receiver::Dust),
            Reading::Dust(15)
        );
    }
}

/// The mechanism `tests/coupling_mechanisms.rs` exists to name, read as a
/// delivery: a strongly powered block re-drives every dust cell beside it and
/// powers no *block* at all. As a handover that is exactly backwards -- it
/// serves the one receiver the caller is least likely to place and fails the
/// two they are most likely to.
#[test]
fn a_powered_block_handover_reaches_the_callers_dust_and_nothing_else() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::PoweredBlock, Receiver::Lamp),
            Reading::Lamp(false),
            "toward {toward:?}: a powered block does not light a lamp beside it"
        );
        assert_eq!(
            delivered(toward, Handover::PoweredBlock, Receiver::Block),
            Reading::Block(BlockPower::None, 0),
            "toward {toward:?}: and powers no block beside it either"
        );
        assert_eq!(
            delivered(toward, Handover::PoweredBlock, Receiver::Dust),
            Reading::Dust(15),
            "toward {toward:?}: it only ever reaches dust"
        );
    }
}

/// The same shape as the powered block, for the same reason: a redstone block
/// carries `block_power: None`. It is the safest *fixture* in the vocabulary
/// precisely because it cannot power a block, which is what disqualifies it
/// as a delivery.
#[test]
fn a_redstone_block_handover_reaches_the_callers_dust_and_nothing_else() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::RedstoneBlock, Receiver::Lamp),
            Reading::Lamp(false)
        );
        assert_eq!(
            delivered(toward, Handover::RedstoneBlock, Receiver::Block),
            Reading::Block(BlockPower::None, 0)
        );
        assert_eq!(
            delivered(toward, Handover::RedstoneBlock, Receiver::Dust),
            Reading::Dust(15)
        );
    }
}

/// A lever satisfies the contract for every receiver -- and is the one
/// construction here that does so in *all six* directions at once, which is
/// why it can only ever be a fixture. See
/// [`a_lever_in_the_pinned_cell_strongly_powers_all_six_neighbours`] for the
/// bill.
#[test]
fn a_lever_handover_powers_every_receiver_and_is_therefore_only_a_fixture() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::Lever, Receiver::Lamp),
            Reading::Lamp(true)
        );
        assert_eq!(
            delivered(toward, Handover::Lever, Receiver::Block),
            Reading::Block(BlockPower::Strong, 15)
        );
        assert_eq!(
            delivered(toward, Handover::Lever, Receiver::Dust),
            Reading::Dust(15)
        );
    }
}

/// The contract's word "powered" has no meaning for a cell that ships empty,
/// and none either for a full cube that does not conduct. Both read exactly
/// as they do with no handover at all, under the *strongest* delivery this
/// file found.
///
/// This is why verification cannot check an output terminal by inspecting the
/// caller's cell in the emitted world: there is nothing in it to inspect. It
/// has to put a receiver there and probe, which is what the spec's
/// verification section already says.
#[test]
fn an_empty_pinned_cell_reads_the_same_however_hard_reda_delivers() {
    for toward in HORIZONTAL {
        assert_eq!(
            delivered(toward, Handover::Repeater, Receiver::Air),
            delivered(toward, Handover::Nothing, Receiver::Air),
            "toward {toward:?}: air holds no reading either way"
        );
        assert_eq!(
            delivered(toward, Handover::Repeater, Receiver::Glass),
            Reading::Block(BlockPower::None, 0),
            "toward {toward:?}: `block_signal_at` gates on conductivity before \
             anything else, so a glass block reports unpowered under a repeater \
             emitting 15 straight into it"
        );
    }
}

/// The cost the pin charges the router, and the discount the repeater gives
/// back. `toward` leaves REDA one cell, so the *terminal* has one approach --
/// but the repeater's rear at `P - 2*toward` is an ordinary net cell, and it
/// reads dust of any shape at any strength above zero. A run that bends into
/// it, a run with something joined to its side, and a run spent all the way
/// down to 1 all deliver the same flat 15 into the caller's cell.
///
/// That is the whole argument for the repeater over dust: the same shapes
/// that silently cost a dust handover the caller's lamp cost the repeater
/// nothing, because a diode reads its rear's `power` field directly whatever
/// the wire's shape (`propagate::signal_from`'s first path, and vanilla's own
/// `DiodeBlock::getInputSignal` fallback).
#[test]
fn a_delivery_repeaters_rear_is_an_ordinary_net_cell() {
    /// Three ways REDA's own net might arrive at the rear cell, none of them
    /// a straight run pointing at the terminal.
    #[derive(Clone, Copy, Debug)]
    enum Approach {
        /// The net turns a corner in the rear cell itself.
        Bend,
        /// The net arrives straight, with another wire joined to its side.
        Branch,
        /// The net arrives straight, but fifteen cells from its source, so
        /// the rear cell holds strength 1.
        Spent,
    }

    for toward in HORIZONTAL {
        let up = toward.opposite();
        let n = handover(toward, Role::Output);
        let rear = net_cell(toward, Role::Output);

        for approach in [Approach::Bend, Approach::Branch, Approach::Spent] {
            let mut world = empty_world();
            place_receiver(&mut world, Receiver::Lamp);
            put(&mut world, n.down(), glass());
            put(&mut world, n, terminal_repeater(toward));

            match approach {
                Approach::Bend => {
                    let side = perpendicular(toward)[0];
                    put(&mut world, rear.down(), glass());
                    put(&mut world, rear, dust());
                    put(&mut world, rear.offset(side).down(), glass());
                    put(&mut world, rear.offset(side), dust());
                    put(&mut world, rear.offset(side).offset(side), redstone_block());
                }
                Approach::Branch => {
                    let side = perpendicular(toward)[1];
                    put(&mut world, rear.down(), glass());
                    put(&mut world, rear, dust());
                    put(&mut world, rear.offset(up), redstone_block());
                    put(&mut world, rear.offset(side).down(), glass());
                    put(&mut world, rear.offset(side), dust());
                }
                Approach::Spent => {
                    for step in 0..=14 {
                        let cell = along(up, 2 + step);
                        put(&mut world, cell.down(), glass());
                        put(&mut world, cell, dust());
                    }
                    put(&mut world, along(up, 17), redstone_block());
                }
            }

            let world = settled(world);
            assert!(
                world.get(rear.x, rear.y, rear.z).power > 0,
                "toward {toward:?}, {approach:?}: the rear must carry a signal \
                 for this to measure anything"
            );
            assert_eq!(
                read_receiver(&world, Receiver::Lamp),
                Reading::Lamp(true),
                "toward {toward:?}, {approach:?}: the caller's lamp must light \
                 all the same"
            );
        }
    }
}

/// The price of that robustness, measured rather than waved at: the delivery
/// repeater costs one redstone tick that a dust handover does not. It is the
/// only thing dust is better at, and it is a fixed constant per terminal.
#[test]
fn a_repeater_handover_costs_one_redstone_tick_more_than_dust() {
    /// Settle with the feed removed, then put it back and count the game
    /// ticks the world takes to settle again.
    fn ticks_to_light(toward: Facing, construction: Handover) -> u64 {
        let feed = net_cell(toward, Role::Output);
        let mut world = empty_world();
        place_receiver(&mut world, Receiver::Lamp);
        place_handover(&mut world, toward, construction);
        put(&mut world, feed, BlockState::air());

        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(MAX_TICKS)
            .expect("the unfed rig settles");
        let p = pinned_cell();
        assert!(
            !simulator.world().get(p.x, p.y, p.z).lit,
            "toward {toward:?}, {construction:?}: the lamp must start dark"
        );

        simulator
            .world_mut()
            .set(feed.x, feed.y, feed.z, redstone_block());
        let ticks = simulator
            .run_until_stable(MAX_TICKS)
            .expect("the fed rig settles");
        assert!(
            simulator.world().get(p.x, p.y, p.z).lit,
            "toward {toward:?}, {construction:?}: the lamp must end lit"
        );
        ticks
    }

    for toward in HORIZONTAL {
        let by_dust = ticks_to_light(toward, Handover::StraightDust);
        let by_repeater = ticks_to_light(toward, Handover::Repeater);
        assert_eq!(
            by_dust, 1,
            "toward {toward:?}: dust carries the change the instant it is made, \
             and the one tick is the lamp's own scheduling"
        );
        assert_eq!(
            by_repeater, 3,
            "toward {toward:?}: the same lamp behind a repeater"
        );
        assert_eq!(
            by_repeater - by_dust,
            2,
            "toward {toward:?}: the terminal's whole price is a repeater's \
             minimum delay -- one redstone tick, two game ticks"
        );
    }
}

// ---------------------------------------------------------------------
// 3. Sensing
// ---------------------------------------------------------------------

/// The input half of the contract, in one table. A repeater whose rear faces
/// the caller's cell reads every way a caller can mean "high" -- and lands a
/// flat 15 on REDA's net for all of them, so the net's source strength is
/// geometry rather than whatever arithmetic the caller happened to leave.
#[test]
fn a_repeater_rear_senses_every_way_the_caller_can_power_their_cell() {
    let powered = [
        CallerState::Lever,
        CallerState::RedstoneBlock,
        CallerState::Torch,
        CallerState::FullDust,
        CallerState::SpentDust,
        CallerState::StronglyPoweredBlock,
        CallerState::WeaklyPoweredBlock,
    ];
    for toward in HORIZONTAL {
        for caller in powered {
            assert_eq!(
                sensed_strength(toward, caller, Reader::RepeaterRear),
                15,
                "toward {toward:?}: {caller:?} must be read as high and \
                 normalized to full strength"
            );
        }
        assert_eq!(
            sensed_strength(toward, CallerState::Nothing, Reader::RepeaterRear),
            0,
            "toward {toward:?}: an empty caller cell must read low"
        );
    }
}

/// The same table read by REDA's own dust, which is what an input terminal
/// would use if it simply ran its net up to the boundary. Two caller states
/// vanish entirely and every surviving one arrives one step decayed, carrying
/// whatever strength the caller happened to have left.
#[test]
fn dust_beside_the_pinned_cell_misses_the_weak_and_the_nearly_spent() {
    for toward in HORIZONTAL {
        assert_eq!(
            sensed_strength(toward, CallerState::SpentDust, Reader::Dust),
            0,
            "toward {toward:?}: a run the caller let decay to 1 dies at the \
             boundary -- dust propagation stops below strength 2"
        );
        assert_eq!(
            sensed_strength(toward, CallerState::WeaklyPoweredBlock, Reader::Dust),
            0,
            "toward {toward:?}: only *strong* block power can re-drive dust, \
             so a weakly powered caller block is invisible"
        );

        assert_eq!(sensed_strength(toward, CallerState::Lever, Reader::Dust), 14);
        assert_eq!(
            sensed_strength(toward, CallerState::RedstoneBlock, Reader::Dust),
            14
        );
        assert_eq!(sensed_strength(toward, CallerState::Torch, Reader::Dust), 14);
        assert_eq!(
            sensed_strength(toward, CallerState::StronglyPoweredBlock, Reader::Dust),
            14
        );
        assert_eq!(
            sensed_strength(toward, CallerState::FullDust, Reader::Dust),
            13,
            "toward {toward:?}: the caller's own wire decays across the \
             boundary like any other"
        );
        assert_eq!(sensed_strength(toward, CallerState::Nothing, Reader::Dust), 0);
    }
}

/// The comparison stated as the one claim a design can rest on: for every
/// caller state, the repeater rear reads at least what dust reads, and for
/// two of them it reads high where dust reads nothing at all.
#[test]
fn a_repeater_rear_senses_strictly_more_than_dust_does() {
    let states = [
        CallerState::Nothing,
        CallerState::Lever,
        CallerState::RedstoneBlock,
        CallerState::Torch,
        CallerState::FullDust,
        CallerState::SpentDust,
        CallerState::StronglyPoweredBlock,
        CallerState::WeaklyPoweredBlock,
    ];
    for toward in HORIZONTAL {
        for caller in states {
            let by_dust = sensed_strength(toward, caller, Reader::Dust);
            let by_repeater = sensed_strength(toward, caller, Reader::RepeaterRear);
            assert!(
                by_repeater >= by_dust,
                "toward {toward:?}, caller {caller:?}: repeater read \
                 {by_repeater}, dust read {by_dust} -- the repeater must never \
                 be the blinder reader"
            );
            assert_eq!(
                by_repeater > 0,
                caller != CallerState::Nothing,
                "toward {toward:?}, caller {caller:?}: the repeater must read \
                 high for every powered caller state and low only for none"
            );
        }
    }
}

// ---------------------------------------------------------------------
// 4. Backflow and coupling
// ---------------------------------------------------------------------

/// Probe every neighbour of the caller's cell except the handover, one fresh
/// world each, with a bare dust cell.
fn probe_neighbours_of_the_pinned_cell(
    toward: Facing,
    role: Role,
    build: impl Fn(&mut World),
) -> Vec<(Facing, u8)> {
    let p = pinned_cell();
    let n = handover(toward, role);
    ALL_SIX
        .into_iter()
        .filter(|&d| p.offset(d) != n)
        .map(|d| {
            let mut world = empty_world();
            build(&mut world);
            let probe = p.offset(d);
            put(&mut world, probe, dust());
            let world = settled(world);
            (d, world.get(probe.x, probe.y, probe.z).power)
        })
        .collect()
}

/// A repeater emits toward its output cell and nowhere else, so REDA's own
/// handover contributes nothing to any other neighbour of the caller's cell.
/// With the caller's cell empty -- the state the world ships in -- the whole
/// neighbourhood is quiet.
#[test]
fn a_delivery_repeater_puts_nothing_into_the_pinned_cells_other_neighbours() {
    for toward in HORIZONTAL {
        for (direction, strength) in
            probe_neighbours_of_the_pinned_cell(toward, Role::Output, |world| {
                place_handover(world, toward, Handover::Repeater)
            })
        {
            assert_eq!(
                strength, 0,
                "toward {toward:?}: a probe in the {direction:?} neighbour of \
                 the caller's cell must read nothing"
            );
        }
    }
}

/// And nothing into its own neighbours either -- the sides a repeater would
/// have to leak through to disturb a route running past the terminal.
#[test]
fn a_delivery_repeater_puts_nothing_into_its_own_side_cells() {
    for toward in HORIZONTAL {
        let n = handover(toward, Role::Output);
        for side in perpendicular(toward).into_iter().chain([Facing::Up]) {
            let mut world = empty_world();
            place_handover(&mut world, toward, Handover::Repeater);
            let probe = n.offset(side);
            put(&mut world, probe, dust());
            let world = settled(world);
            assert_eq!(
                world.get(probe.x, probe.y, probe.z).power,
                0,
                "toward {toward:?}: the delivery repeater must not reach its \
                 {side:?} neighbour"
            );
        }
    }
}

/// The price of the caller's cell being powered: anything conductive standing
/// in it becomes a full-strength source for every dust cell touching it. A
/// lamp counts -- `taxonomy::flags_of` conducts lamps exactly like stone --
/// so the demo's own lamp in the pinned cell turns that cell into a 15.
///
/// This is the keep-out rule for an output terminal, and it is about the
/// *caller's* cell, not REDA's: REDA cannot see what will be put there, so
/// every neighbour of `P` but the handover has to be free of signal-carrying
/// REDA cells. This test uses dust to demonstrate that requirement; inert
/// primitive support, floor, or fill remains permitted.
#[test]
fn a_conductive_block_in_the_pinned_cell_re_drives_dust_in_every_neighbour() {
    for receiver in [Receiver::Block, Receiver::Lamp] {
        for toward in HORIZONTAL {
            for (direction, strength) in
                probe_neighbours_of_the_pinned_cell(toward, Role::Output, |world| {
                    place_receiver(world, receiver);
                    place_handover(world, toward, Handover::Repeater);
                })
            {
                assert_eq!(
                    strength, 15,
                    "toward {toward:?} into a {receiver:?}: a probe in the \
                     {direction:?} neighbour reads the caller's cell at full \
                     strength"
                );
            }
        }
    }
}

/// The recorded quirk, re-measured because a fixture decision rests on it: a
/// lit lever in this simulator is `Strong` on **all six** neighbours
/// (`taxonomy::power_emitted_toward` falls through to an isotropic `full` for
/// levers, as `lever_footprint`'s doc comment records). So a lever in the
/// caller's cell does not merely drive dust beside it; it turns every
/// conductive block beside it into a source, which then drives dust one cell
/// further out again.
#[test]
fn a_lever_in_the_pinned_cell_strongly_powers_all_six_neighbours() {
    let p = pinned_cell();
    for direction in ALL_SIX {
        let mut world = empty_world();
        put(&mut world, p, lit_lever());
        let neighbour = p.offset(direction);
        put(&mut world, neighbour, stone());
        let beyond = neighbour.offset(direction);
        put(&mut world, beyond, dust());
        let world = settled(world);

        assert_eq!(
            block_signal_at(&world, neighbour),
            (BlockPower::Strong, 15),
            "a lit lever must strongly power its {direction:?} neighbour"
        );
        assert_eq!(
            world.get(beyond.x, beyond.y, beyond.z).power,
            15,
            "and that block must then re-drive dust a second cell out, which is \
             what makes a lever's keep-out two cells deep"
        );
    }
}

/// The fixture the harness should reach for instead. A redstone block drives
/// dust in all six directions just as a lever does -- so it stands in for a
/// caller's source as far as any reader is concerned -- but it powers no
/// block, so the second hop never happens and its keep-out is one cell deep.
///
/// It also needs no attachment face, which the contract does not promise: the
/// other five neighbours may hold inert primitive support, floor, or fill, but
/// no signal-carrying REDA cell. That half is not measured here and cannot be:
/// this simulator does not model placement legality, so a floating lever
/// behaves exactly like a mounted one. It follows from `world::block::Face`
/// and taxonomy's `air_supports_nothing`, and it is recorded because a fixture
/// that would pop off in the real game is not a fixture.
#[test]
fn a_redstone_block_fixture_drives_dust_without_powering_a_single_block() {
    let p = pinned_cell();
    for direction in ALL_SIX {
        let mut world = empty_world();
        put(&mut world, p, redstone_block());
        let neighbour = p.offset(direction);
        put(&mut world, neighbour, stone());
        let beyond = neighbour.offset(direction);
        put(&mut world, beyond, dust());
        let world = settled(world);

        assert_eq!(
            block_signal_at(&world, neighbour),
            (BlockPower::None, 0),
            "a redstone block must leave its {direction:?} neighbour block inert"
        );
        assert_eq!(
            world.get(beyond.x, beyond.y, beyond.z).power,
            0,
            "so nothing reaches the cell beyond it"
        );
    }

    // The half that makes it usable as a fixture at all: it still drives dust
    // directly, in every direction, at full strength.
    for direction in ALL_SIX {
        let mut world = empty_world();
        put(&mut world, p, redstone_block());
        let probe = p.offset(direction);
        put(&mut world, probe, dust());
        let world = settled(world);
        assert_eq!(
            world.get(probe.x, probe.y, probe.z).power,
            15,
            "a redstone block must still drive dust in its {direction:?} neighbour"
        );
    }
}

/// The two sides of the boundary need keep-out zones of different depths,
/// and the reason is the one asymmetry in the whole power model: a source
/// powers blocks, a powered block does not.
///
/// * REDA delivering into a conductive caller cell makes that cell a source
///   for **dust** one cell out, and stops there -- a solid block one cell out
///   stays completely inert, so nothing propagates to a second cell.
/// * The caller powering their own cell with a source makes every conductive
///   block one cell out a source in its own right, which then drives dust a
///   second cell out.
///
/// So an output terminal must keep dust out of the caller's neighbours; an
/// input terminal must keep dust *and* conductors out of them.
#[test]
fn a_delivered_pinned_cell_leaks_one_cell_and_a_caller_source_leaks_two() {
    let p = pinned_cell();
    for toward in HORIZONTAL {
        let n = handover(toward, Role::Output);
        for direction in ALL_SIX.into_iter().filter(|&d| p.offset(d) != n) {
            let neighbour = p.offset(direction);
            let beyond = neighbour.offset(direction);

            // Delivery side: REDA drives a stone in the caller's cell.
            let mut world = empty_world();
            place_receiver(&mut world, Receiver::Block);
            place_handover(&mut world, toward, Handover::Repeater);
            put(&mut world, neighbour, stone());
            put(&mut world, beyond, dust());
            let world = settled(world);
            assert_eq!(
                block_signal_at(&world, neighbour),
                (BlockPower::None, 0),
                "toward {toward:?}: a strongly powered caller cell must leave a \
                 block in its {direction:?} neighbour inert -- blocks do not \
                 power blocks"
            );
            assert_eq!(
                world.get(beyond.x, beyond.y, beyond.z).power,
                0,
                "toward {toward:?}: so the delivery cannot reach a second cell \
                 out through {direction:?}"
            );

            // Caller side: the caller's own source in the same cell.
            let mut world = empty_world();
            put(&mut world, p, lit_lever());
            put(&mut world, neighbour, stone());
            put(&mut world, beyond, dust());
            let world = settled(world);
            assert_eq!(
                block_signal_at(&world, neighbour),
                (BlockPower::Strong, 15),
                "toward {toward:?}: a caller *source* does power the block in \
                 its {direction:?} neighbour"
            );
            assert_eq!(
                world.get(beyond.x, beyond.y, beyond.z).power,
                15,
                "toward {toward:?}: and that block reaches a second cell out"
            );
        }
    }
}

/// Where that second cell actually lands, named as a coordinate rather than
/// as a depth: the neighbour of `P` perpendicular to `toward` is one step
/// from the cell beside REDA's own handover, in **either** role. So a caller
/// who builds a conductive block in a cell the contract explicitly grants
/// them, and powers `P` from their own side, re-drives any dust REDA left
/// touching the handover.
///
/// This is the constraint the keep-out has to be stated in: not "REDA keeps
/// off the caller's cell" but "REDA keeps its dust two cells away from the
/// caller's cell in every direction but the handover axis". It is unavoidable
/// on the input side, where the caller's source is the whole point; on the
/// output side it is only reachable if the caller puts a source of their own
/// in a neighbour of `P`, which the contract grants them and does not
/// forbid.
#[test]
fn a_caller_conductor_beside_the_pinned_cell_reaches_dust_beside_the_handover() {
    let p = pinned_cell();
    for toward in HORIZONTAL {
        for role in [Role::Output, Role::Input] {
            let n = handover(toward, role);
            let outward = match role {
                Role::Output => toward.opposite(),
                Role::Input => toward,
            };
            for side in perpendicular(toward) {
                let caller_block = p.offset(side);
                let redas_dust = n.offset(side);
                assert_eq!(
                    caller_block.offset(outward),
                    redas_dust,
                    "the two cells really are one step apart -- otherwise this \
                     test measures nothing"
                );

                let mut world = empty_world();
                put(&mut world, p, lit_lever());
                put(&mut world, caller_block, stone());
                put(&mut world, redas_dust, dust());
                let world = settled(world);

                assert_eq!(
                    world.get(redas_dust.x, redas_dust.y, redas_dust.z).power,
                    15,
                    "toward {toward:?}, {role:?}, side {side:?}: the caller's own \
                     block carries their source into the cell beside REDA's handover"
                );
            }
        }
    }
}

/// The limit of what this file can measure, stated so a later stage meets it
/// as a fact rather than a mystery. The contract names *a piston* among the
/// things a caller might attach, and this simulator refuses pistons outright
/// (`simulator::UNSUPPORTED_KINDS`, which also holds observers, buttons and
/// pressure plates). So every probe and every fixture the harness installs
/// has to be built from what the simulator does model -- lamp, lever, torch,
/// redstone block, dust, repeater, comparator -- and "the caller attaches a
/// piston" is a claim no test here can make.
#[test]
fn the_simulator_refuses_to_answer_for_a_piston_in_the_pinned_cell() {
    let p = pinned_cell();
    let mut world = empty_world();
    put(&mut world, p, named("minecraft:piston", BlockKind::Piston));
    place_handover(&mut world, Facing::North, Handover::Repeater);

    let mut simulator = Simulator::new(world);
    assert_eq!(
        simulator.run_until_stable(MAX_TICKS),
        Err(SimulationError::UnsupportedComponent {
            position: p,
            name: "minecraft:piston".to_string(),
        }),
        "a piston in the caller's cell is outside this simulator's model, and \
         it says so instead of guessing"
    );
}

/// And the positive statement the design can build on: with that keep-out
/// honoured -- no signal-carrying REDA cell in any neighbour of `P` but the
/// handover, and no dust of REDA's beside the handover either -- the worst
/// fixture this test constructs reaches nothing. A lit lever in `P` and
/// conductive blocks filling every non-handover neighbour leave REDA's own net
/// exactly as dark as it was; this construction does not claim those cells ship
/// empty, because inert primitive support, floor, or fill may occupy them.
#[test]
fn the_contracts_keep_out_survives_the_worst_the_caller_can_build() {
    let p = pinned_cell();
    for toward in HORIZONTAL {
        let n = handover(toward, Role::Output);
        let rear = net_cell(toward, Role::Output);
        let deeper = rear.offset(toward.opposite());

        let mut world = empty_world();
        // REDA's side of an output terminal, switched off: no feed anywhere.
        put(&mut world, n.down(), glass());
        put(&mut world, n, terminal_repeater(toward));
        put(&mut world, rear.down(), glass());
        put(&mut world, rear, dust());
        put(&mut world, deeper.down(), glass());
        put(&mut world, deeper, dust());

        // A worst-case caller fixture; the contract does not promise these
        // non-handover cells ship empty in a compiled world.
        put(&mut world, p, lit_lever());
        for direction in ALL_SIX {
            let cell = p.offset(direction);
            if cell == n {
                continue;
            }
            put(&mut world, cell, stone());
        }

        let world = settled(world);
        assert!(
            !world.get(n.x, n.y, n.z).lit,
            "toward {toward:?}: the caller is on the repeater's output side and \
             must never switch it on"
        );
        assert_eq!(
            world.get(rear.x, rear.y, rear.z).power,
            0,
            "toward {toward:?}: nothing may appear on REDA's net behind it"
        );
        assert_eq!(
            world.get(deeper.x, deeper.y, deeper.z).power,
            0,
            "toward {toward:?}: nor one cell deeper in"
        );
    }
}

/// Whatever the caller does with their cell, it cannot reach back through a
/// delivery repeater: a repeater reads only its rear, and its rear is REDA's
/// own net, two cells from the caller. Measured with the most aggressive
/// thing a caller could put there -- a lit lever, which strongly powers all
/// six of its neighbours including the repeater's cell.
#[test]
fn a_caller_source_in_the_pinned_cell_cannot_reach_back_through_a_delivery_repeater() {
    for toward in HORIZONTAL {
        let n = handover(toward, Role::Output);
        let rear = net_cell(toward, Role::Output);

        // No feed at all: REDA's side of the terminal is quiet, and the only
        // source in the world is the caller's.
        let mut world = empty_world();
        put(&mut world, pinned_cell(), lit_lever());
        put(&mut world, n.down(), glass());
        put(&mut world, n, terminal_repeater(toward));
        put(&mut world, rear.down(), glass());
        put(&mut world, rear, dust());
        let world = settled(world);

        assert!(
            !world.get(n.x, n.y, n.z).lit,
            "toward {toward:?}: the caller's source is on the repeater's \
             output side and must never switch it on"
        );
        assert_eq!(
            world.get(rear.x, rear.y, rear.z).power,
            0,
            "toward {toward:?}: and nothing may appear on REDA's net behind it"
        );
    }
}
