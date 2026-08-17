//! What the parse is checked against, and the one measurement the artifacts do
//! not already contain.
//!
//! Two kinds of test here, and the split matters. Most of them assert that
//! [`super::energises`] says what `docs/derived/coupling-mechanisms.md` says --
//! they are about the *reading*, and they go red if the parse drifts or the
//! file's shape moves. One of them is not about reading at all: Tables 3 and 3b
//! sweep the mediator's remaining faces for a mediator **above** and **below**
//! the emitter and never for one beside it, so the "every face it has left"
//! fan-out this module applies in all six directions is measured for two of
//! them and extrapolated to four. `the_fan_out_holds_for_a_horizontal_mediator_
//! too` runs those four through the `Simulator`, which is rule 6 rather than a
//! restatement of it.

use std::collections::BTreeSet;

use super::*;
use crate::redstone::simulator::position::{Position, ALL_SIX};
use crate::redstone::simulator::{SimulationError, Simulator};
use crate::redstone::world::block::{BlockState, Face};
use crate::redstone::world::storage::World;

// ---------------------------------------------------------------------
// The reading
// ---------------------------------------------------------------------

#[test]
fn the_column_order_is_all_six() {
    let from_simulator: Vec<Offset> = ALL_SIX
        .into_iter()
        .map(|facing| {
            let at = Position::new(0, 0, 0).offset(facing);
            (at.x, at.y, at.z)
        })
        .collect();
    let from_artifact: Vec<Offset> = SIX.iter().map(|(_, offset)| *offset).collect();
    assert_eq!(
        from_artifact, from_simulator,
        "the tables print their columns in `ALL_SIX` order; if that order ever \
         changes, every offset this module derives is silently rotated"
    );
}

/// The four rows the whole exercise turns on, read back verbatim.
///
/// A repeater is the "too big" case, a lever the "too small" one, dust is the
/// case where `keep_out` is exactly right, and stone is the negative control
/// without which an all-empty parse would look like a working one.
#[test]
fn the_parse_is_the_artifact() {
    let north = Some(Facing::North);

    let repeater = energises(BlockKind::Repeater, north);
    assert_eq!(
        repeater.hop1,
        BTreeSet::from([(0, 0, 1)]),
        "Table 1 gives a repeater exactly one driven face, `facing.opposite()`"
    );
    assert_eq!(repeater.hop2.len(), 5, "Table 2 gives it one mediator, five faces");
    assert_eq!(
        repeater.unmeasured,
        BTreeSet::from([
            (0, 0, -1),
            (0, 0, -2),
            (1, 0, -1),
            (-1, 0, -1),
            (0, 1, -1),
            (0, -1, -1)
        ]),
        "and its rear is `x` in both tables -- the rig's own feed has to stand \
         there, so the artifact cannot answer it"
    );

    let lever = energises(BlockKind::Lever, None);
    assert_eq!(lever.hop1.len(), 6, "a lever drives all six faces");
    assert_eq!(
        lever.hop2.len(),
        18,
        "and strongly powers all six neighbouring blocks, each of which drives \
         its own five remaining faces -- every offset at L1 distance two"
    );
    assert!(lever.unmeasured.is_empty(), "no rig for a lever is invalid");

    for kind in [BlockKind::Solid, BlockKind::Glass, BlockKind::Lamp] {
        let inert = energises(kind, None);
        assert_eq!(
            (inert.hop1.len(), inert.hop2.len(), inert.unmeasured.len()),
            (0, 0, 0),
            "{kind:?} is the negative control and the artifact measures it inert"
        );
    }
}

/// The one row that decides whether any of this buys anything at a dust pair.
#[test]
fn the_dust_join_artifact_says_all_twelve_of_keep_out_s_cells_join() {
    let joins = dust_join_offsets();
    assert_eq!(
        joins.len(),
        12,
        "`docs/derived/dust-join-relation.md`'s own summary: 12 of `keep_out`'s \
         12 cells really join in the shape a compiled world presents"
    );
    // The twelve `keep_out` writes: each horizontal neighbour, and the cell
    // above and below each.
    let mut expected = BTreeSet::new();
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        for dy in [-1, 0, 1] {
            expected.insert((dx, dy, dz));
        }
    }
    assert_eq!(joins, expected, "and they are exactly `keep_out`'s twelve");
}

#[test]
fn the_unconditional_joins_are_the_four_same_layer_cells() {
    assert_eq!(
        unconditional_dust_joins(),
        BTreeSet::from([(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1)]),
        "no content of any cell anywhere separates a same-layer dust pair, so \
         a same-layer dust offender is refused by any correct rule and \
         component-awareness cannot buy it back"
    );
}

/// A record, not an assertion about physics: `state` is absent from
/// [`energises`]'s signature on purpose.
#[test]
fn activation_is_deliberately_not_a_parameter() {
    // `compile` writes torches pre-lit, levers off, and every dust at power 0,
    // because nothing has settled the world yet. So a lit/unlit parameter would
    // answer `no` for exactly the lever that shipped a wrong circuit.
    let off_lever_would_still_be_asked_structurally = energises(BlockKind::Lever, None);
    assert_eq!(off_lever_would_still_be_asked_structurally.hop1.len(), 6);
}

// ---------------------------------------------------------------------
// The one thing the artifacts do not contain
// ---------------------------------------------------------------------

fn named(kind: BlockKind, name: &str) -> BlockState {
    let mut state = BlockState::air();
    state.kind = kind;
    state.name = name.to_string();
    state
}

fn rig_lever() -> BlockState {
    let mut state = named(BlockKind::Lever, "minecraft:lever");
    state.lit = true;
    state.face = Some(Face::Floor);
    state.facing = Some(Facing::North);
    state
}

fn settled(world: World) -> Option<World> {
    let mut simulator = Simulator::new(world);
    match simulator.run_until_stable(400) {
        Ok(_) => Some(simulator.world().clone()),
        Err(SimulationError::UnsupportedComponent { .. }) => Some(simulator.world().clone()),
        Err(SimulationError::Diverged { .. }) => None,
    }
}

/// Whether a lever at the origin, through a stone mediator one step out in
/// `into`, drives a bare dust cell on the mediator's `face` side.
///
/// The same method as both artifacts: build it, settle it, read the receiver,
/// then rebuild with the emitter cell written as **air** and read again. A
/// coupling is a reading that changed.
fn mediated_face_couples(into: Facing, face: Facing) -> Option<bool> {
    let origin = Position::new(8, 8, 8);
    let mediator = origin.offset(into);
    let receiver = mediator.offset(face);
    assert_ne!(receiver, origin, "the emitter's own cell is not a receiver");
    // If the receiver touched the emitter this would measure hop 1 instead.
    assert!(
        !ALL_SIX
            .into_iter()
            .any(|direction| origin.offset(direction) == receiver),
        "a receiver adjacent to the emitter would be a direct drive, not a \
         mediated one"
    );

    let build = |removed: bool| {
        let mut world = World::new(17, 17, 17);
        world.set(
            mediator.x,
            mediator.y,
            mediator.z,
            named(BlockKind::Solid, "minecraft:stone"),
        );
        if !removed {
            world.set(origin.x, origin.y, origin.z, rig_lever());
        }
        world.set(
            receiver.x,
            receiver.y,
            receiver.z,
            named(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
        );
        world
    };

    let (driven, control) = (settled(build(false))?, settled(build(true))?);
    let read = |world: &World| world.get(receiver.x, receiver.y, receiver.z).power;
    // A bare dust cell with air beneath it reads zero when nothing drives it;
    // anything else in the control means a second source is in play and
    // nothing can be attributed.
    (read(&control) == 0).then_some(read(&driven) != 0)
}

/// **The extrapolation, measured.**
///
/// [`energises`] applies "a strongly powered conductive block drives dust on
/// every face it has left" in all six directions. Table 3 measures that for a
/// mediator directly **above** the emitter and Table 3b for one directly
/// **below**; no table in either artifact puts the mediator beside it. So the
/// four horizontal directions are, in the artifacts, an extrapolation -- and
/// this is the run that turns them into a measurement.
///
/// Rule 2: injecting `face != into.opposite()` into the fan-out (that is,
/// claiming the mediator drives only the four faces that are not the straight-
/// through one) makes this go red on 4 of the 4 straight-through columns;
/// removing the mediator write makes it go red on all 20.
#[test]
fn the_fan_out_holds_for_a_horizontal_mediator_too() {
    let mut measured = Vec::new();
    for into in [Facing::North, Facing::South, Facing::East, Facing::West] {
        for face in ALL_SIX {
            if face == into.opposite() {
                // The emitter's own cell.
                continue;
            }
            let verdict = mediated_face_couples(into, face)
                .unwrap_or_else(|| panic!("{into:?}/{face:?} did not settle, or was contaminated"));
            measured.push((into, face, verdict));
        }
    }
    assert_eq!(measured.len(), 20, "four directions, five remaining faces each");
    let coupled = measured.iter().filter(|(_, _, yes)| *yes).count();
    assert_eq!(
        coupled, 20,
        "every one of the five remaining faces of a horizontally placed \
         strongly powered stone mediator drives dust, exactly as Tables 3 and \
         3b measure for a mediator above and below: {measured:?}"
    );
}

/// The headline of the whole exercise, in one assertion.
#[test]
fn a_repeater_energises_far_fewer_cells_than_keep_out_reserves_and_a_lever_far_more() {
    let repeater = energises(BlockKind::Repeater, Some(Facing::North));
    assert_eq!(
        repeater.measured().len(),
        6,
        "a repeater's whole measured range is six cells against `keep_out`'s twelve"
    );
    assert_eq!(
        repeater.conservative().len(),
        12,
        "though with its unanswerable rear kept it is twelve again -- a \
         *different* twelve, not a smaller one"
    );

    let lever = energises(BlockKind::Lever, None);
    assert_eq!(
        lever.measured().len(),
        24,
        "a lever reaches twenty-four cells, twice what `keep_out` reserves for it"
    );

    let torch = energises(BlockKind::Torch, None);
    assert_eq!(torch.measured().len(), 10);
    let wall = energises(BlockKind::WallTorch, Some(Facing::North));
    assert_eq!(wall.measured().len(), 10);
    let block = energises(BlockKind::RedstoneBlock, None);
    assert_eq!(
        block.measured().len(),
        6,
        "a redstone block drives dust on all six faces and powers no block, so \
         it has no second hop at all"
    );
}

/// Dust has no second hop, which is why every one of the 41 extra edges needs a
/// *diode* at one end.
#[test]
fn dust_does_not_reach_across_a_block() {
    let dust = energises(BlockKind::RedstoneWire, None);
    assert!(
        dust.hop2.is_empty(),
        "Table 2's `dust` row is all `.`: a wire powers the block it stands on \
         weakly, and weak power never re-drives dust"
    );
}

/// **Mechanism 4 beside a block, which neither artifact measures.**
///
/// `docs/derived/coupling-mechanisms.md`'s Table 5 puts the weak driver
/// *standing on* the mediator (`across`) and never beside it, so "a foreign
/// dust cell one cardinal step from a gate's support block turns that gate's
/// torch off" -- the reason `compile::gate_footprint` calls a NOR's support a
/// conductor -- is nowhere in the derived range. And it cannot be: it is the
/// *reverse* direction. The offender is inert stone, the newcomer is the
/// emitter, and [`energises`] asked of an inert offender answers nothing at
/// all. A keep-out rule built from [`energises`] alone would hand that cell
/// back, and the circuit would come out wrong.
///
/// So it is measured here, in both geometries, and the answer is committed as
/// [`BESIDE_A_SUPPORT_IS_READ`].
///
/// Rule 2: with the redstone-block feed never written (so the dust is present
/// but dead) both readings go `true` and the two `assert`s below go red -- the
/// control arm above is what says the rig is live rather than the torch simply
/// being off in every world.
#[test]
fn a_powered_dust_against_a_gate_support_puts_its_torch_out() {
    let stone = || named(BlockKind::Solid, "minecraft:stone");
    let dust = || named(BlockKind::RedstoneWire, "minecraft:redstone_wire");
    let source = || named(BlockKind::RedstoneBlock, "minecraft:redstone_block");

    // Geometry one: a standing torch on top of a support block, with the dust
    // one cardinal step from the support at the same layer -- a route running
    // past a gate, which is the shape `gate_footprint` refuses.
    let support = Position::new(8, 8, 8);
    let torch_at = support.up();
    let beside = support.offset(Facing::East);
    let feed = beside.offset(Facing::East);
    let beside_torch = |driven: bool| {
        let mut world = World::new(17, 17, 17);
        world.set(support.x, support.y, support.z, stone());
        let mut torch = named(BlockKind::Torch, "minecraft:redstone_torch");
        torch.lit = true;
        world.set(torch_at.x, torch_at.y, torch_at.z, torch);
        let floor = beside.down();
        world.set(floor.x, floor.y, floor.z, stone());
        world.set(beside.x, beside.y, beside.z, dust());
        if driven {
            world.set(feed.x, feed.y, feed.z, source());
        }
        settled(world)
            .expect("the rig settles")
            .get(torch_at.x, torch_at.y, torch_at.z)
            .lit
    };
    assert!(
        beside_torch(false),
        "control: with nothing driving the dust the torch stays lit, so a `false`          below is the dust and not the rig"
    );
    let beside_kills = !beside_torch(true);

    // Geometry two: Table 5's own `across dust (weak)` row -- a wire standing
    // *on* a block a wall torch is attached to.
    let wall_support = Position::new(8, 8, 12);
    let wall_at = wall_support.offset(Facing::North);
    let on_top = wall_support.up();
    let standing = |driven: bool| {
        let mut world = World::new(17, 17, 17);
        world.set(wall_support.x, wall_support.y, wall_support.z, stone());
        let mut wall = named(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
        wall.facing = Some(Facing::North);
        wall.lit = true;
        world.set(wall_at.x, wall_at.y, wall_at.z, wall);
        world.set(on_top.x, on_top.y, on_top.z, dust());
        if driven {
            let feed = on_top.offset(Facing::East);
            world.set(feed.x, feed.y, feed.z, source());
        }
        settled(world)
            .expect("the rig settles")
            .get(wall_at.x, wall_at.y, wall_at.z)
            .lit
    };
    assert!(standing(false), "control: the wall torch starts lit");
    let standing_kills = !standing(true);

    assert!(
        standing_kills,
        "Table 5's `across dust (weak)` row, reproduced: a wire standing on a block          weakly powers it and a torch attached to that block goes out"
    );
    assert_eq!(
        beside_kills, BESIDE_A_SUPPORT_IS_READ,
        "the committed answer for the geometry neither artifact measures: a powered          dust one cardinal step from a support block is read through it"
    );
}
