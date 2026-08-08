//! Confirms, against the real `Simulator`, the exact spacing rule the
//! channel router's constants (`COLUMN_CLEARANCE`, `TRACK_SPACING`,
//! `TRACK_SHARE_GAP`, `ENTRY_OFFSET`, `SLOT_PITCH`) have always been trusted
//! to provide, but which `src/compile/mod.rs` derives from none of them:
//! `redstone::simulator::connectivity::{dust_connections, dust_reach}`.
//!
//! See `docs/superpowers/specs/2026-08-09-channel-safety-condition.md` for
//! the full derivation and the verdict on each constant; this file is that
//! document's evidence, not a restatement of it.
//!
//! Every test here builds a minimal hand-placed `World` (same style as
//! `tests/or_merge.rs`'s backflow probe) rather than going through
//! `compile()`, so each one isolates exactly one geometric fact instead of
//! depending on any particular circuit's floorplan to happen to exercise it.

use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

const MAX_TICKS: u64 = 200;

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

/// A repeater whose signal travels toward `direction` -- the same convention
/// `compile::mod`'s own private `repeater()` helper uses (not reachable from
/// here, so rebuilt: `facing` stores the opposite of the travel direction).
fn raw_repeater(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

fn floor_under(world: &mut World, x: i32, y: i32, z: i32) {
    world.set(x, y - 1, z, stone());
}

// ---------------------------------------------------------------------
// Experiments 1/2: two independent, lever-driven dust runs at growing
// Z-separation, one cell of Chebyshev distance at a time. This is the
// general shape every one of `COLUMN_CLEARANCE`, `TRACK_SPACING` and
// `TRACK_SHARE_GAP` protects -- two parallel or colinear conductor lines,
// neither terminated by anything but plain dust.
//
// `dust_connections`' same-layer rule only ever looks at the single cell one
// step away in a cardinal direction (`redstone/simulator/connectivity.rs`,
// `dust_connections`'s first block: `neighbour = from.offset(direction)`,
// unconditional). So the claim under test is exactly this: coordinate
// distance 1 (touching) must cross-talk, and distance 2 (one empty cell
// between them) must not -- for *any* larger distance the same code path
// makes the claim trivially true, so 1 vs 2 is the whole boundary.
// ---------------------------------------------------------------------

/// Builds two independent single-hop nets: lever `a` at `(1, 1, za)` driving
/// a probe cell at `(2, 1, za)`; lever `b` at `(1, 1, zb)` driving a probe
/// cell at `(2, 1, zb)`, left off. Returns `(world, probe_b_position)` --
/// `probe_a` is always `(2, 1, za)` but no test below reads it back.
fn build_two_parallel_runs(za: i32, zb: i32) -> (World, (i32, i32, i32)) {
    let mut world = World::new(8, 4, 8);

    let lever_a = (1, 1, za);
    let probe_a = (2, 1, za);
    world.set(lever_a.0, lever_a.1, lever_a.2, raw_lever(true));
    floor_under(&mut world, probe_a.0, probe_a.1, probe_a.2);
    world.set(probe_a.0, probe_a.1, probe_a.2, dust());

    let lever_b = (1, 1, zb);
    let probe_b = (2, 1, zb);
    world.set(lever_b.0, lever_b.1, lever_b.2, raw_lever(false));
    floor_under(&mut world, probe_b.0, probe_b.1, probe_b.2);
    world.set(probe_b.0, probe_b.1, probe_b.2, dust());

    (world, probe_b)
}

#[test]
fn two_bare_dust_probes_touching_directly_cross_talk() {
    // za=3, zb=4: probe_a and probe_b are Z-adjacent (coordinate distance 1,
    // the "no gap at all" case -- COLUMN_CLEARANCE/TRACK_SPACING/
    // TRACK_SHARE_GAP minus one).
    let (world, probe_b) = build_two_parallel_runs(3, 4);
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("must settle");

    let power = simulator.world().get(probe_b.0, probe_b.1, probe_b.2).power;
    assert!(
        power > 0,
        "probe_b sits one cell from lever a's own dust with lever b off -- if it reads 0 the two \
         nets did NOT cross-talk at distance 1, which would falsify the whole spacing derivation \
         this project's channel router relies on"
    );
}

#[test]
fn two_bare_dust_probes_one_cell_apart_do_not_cross_talk() {
    // za=3, zb=5: one empty cell between them (coordinate distance 2) --
    // exactly what COLUMN_CLEARANCE, TRACK_SPACING and TRACK_SHARE_GAP all
    // ultimately reduce to when a bare dust cell is what actually sits at
    // the boundary.
    let (world, probe_b) = build_two_parallel_runs(3, 5);
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("must settle");

    let power = simulator.world().get(probe_b.0, probe_b.1, probe_b.2).power;
    assert_eq!(
        power, 0,
        "probe_b sits two cells from lever a's own dust with lever b off -- a nonzero reading here \
         means distance 2 is not actually enough, which would make COLUMN_CLEARANCE/TRACK_SPACING/ \
         TRACK_SHARE_GAP == 2 an outright bug rather than the tight derived minimum this project has \
         been assuming"
    );
}

// ---------------------------------------------------------------------
// Experiment 3: the missing-terminating-repeater question, quantified.
//
// Same physical distance as the failing case above (coordinate distance 1,
// touching) -- but this time the cell foreign net `b` touches is a repeater
// terminating net `a`'s own route, not bare dust. `is_dust` in
// `connectivity.rs` checks `BlockKind::RedstoneWire` specifically, so a
// repeater can never be the *target* of `dust_connections`' same-layer
// rule -- and `verify_connectivity` only ever starts its BFS from actual
// `RedstoneWire` cells (`world.positions_of(BlockKind::RedstoneWire)`), so a
// repeater is never the *source* of a join either. A repeater's sides are
// also never a power source (`taxonomy::power_emitted_by`'s
// `Repeater | Comparator` arm only ever drives its own facing direction),
// so there is no separate side-channel for `b`'s power to leak through.
//
// If this test passes, the quantified answer is exact: the required
// clearance from a *bare* dust termination is coordinate distance 2 (one
// empty cell, confirmed above); from a *repeater* termination it is
// coordinate distance 1 (touching is fine) -- one full cell of clearance,
// on every side but the repeater's own facing axis, that a terminating
// repeater buys for free and a bare merge junction cannot.
// ---------------------------------------------------------------------

#[test]
fn a_repeater_terminated_socket_does_not_cross_talk_even_when_a_foreign_net_touches_its_side() {
    let mut world = World::new(8, 4, 8);

    // Net a: lever -> repeater (signal travels East) -> one more dust cell,
    // so a real reading exists past the repeater to check.
    let lever_a = (1, 1, 3);
    let repeater_pos = (2, 1, 3);
    let output_probe = (3, 1, 3);
    world.set(lever_a.0, lever_a.1, lever_a.2, raw_lever(false));
    floor_under(&mut world, repeater_pos.0, repeater_pos.1, repeater_pos.2);
    world.set(repeater_pos.0, repeater_pos.1, repeater_pos.2, raw_repeater(Facing::East));
    floor_under(&mut world, output_probe.0, output_probe.1, output_probe.2);
    world.set(output_probe.0, output_probe.1, output_probe.2, dust());

    // Net b: an unrelated, fully powered lever whose own dust touches the
    // repeater's *side* (Z+1 from it -- perpendicular to the repeater's own
    // East-West travel), the exact geometry that cross-talked in
    // `two_bare_dust_probes_touching_directly_cross_talk` above.
    let lever_b = (1, 1, 4);
    let probe_b = (2, 1, 4);
    world.set(lever_b.0, lever_b.1, lever_b.2, raw_lever(true));
    floor_under(&mut world, probe_b.0, probe_b.1, probe_b.2);
    world.set(probe_b.0, probe_b.1, probe_b.2, dust());

    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("must settle");

    let output_power = simulator.world().get(output_probe.0, output_probe.1, output_probe.2).power;
    assert_eq!(
        output_power, 0,
        "lever a is off and lever b is fully on, touching the repeater's own side -- a nonzero \
         reading past the repeater means b's power leaked through, which is exactly the bare-merge \
         failure this project hit on wip/wired-or-genlib, reproduced here with a repeater standing \
         where a bare merge junction would have been instead"
    );

    // Confirm the rig actually works (net a's own signal still gets through
    // once it is genuinely driven), so the assertion above is "isolated",
    // not "broken".
    let mut on_state = simulator.world().get(lever_a.0, lever_a.1, lever_a.2).clone();
    on_state.lit = true;
    simulator.world_mut().set(lever_a.0, lever_a.1, lever_a.2, on_state);
    simulator.run_until_stable(MAX_TICKS).expect("must settle");
    let output_power = simulator.world().get(output_probe.0, output_probe.1, output_probe.2).power;
    assert!(output_power > 0, "net a's own signal must still reach past its own repeater once driven");
}
