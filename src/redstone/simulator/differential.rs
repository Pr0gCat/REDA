//! The full-re-settle differential: a settled world against the same world
//! re-settled from scratch, cell by cell.
//!
//! # Why this exists
//!
//! `Simulator::run_until_stable` settles a world *incrementally*:
//! `propagate::recompute_dust_strengths` recomputes only the dust networks
//! that `World::take_dirty` says could have been affected since the last
//! recompute (`propagate::active_dust_networks`), and its write-back loop only
//! ever writes cells in that active set. If a cell that should have been
//! recomputed is not in the active set, it keeps whatever value it had -- a
//! **stale** cell. Measured before this module existed: a negotiated
//! `full_adder` isolation world settled with a dust cell at `(56, 1, 99)`
//! reading 0 while the cell feeding it, `(57, 2, 99)`, read 10 and
//! `dust_connections((57,2,99), West)` returned `[(56,1,99)]` in that very
//! world. Same blocks, fully re-settled, and the chain reads 9, 8, 7, 6.
//!
//! # The oracle
//!
//! The one thing in the tree that uses the same physics and none of the
//! incremental bookkeeping is a **full re-settle from a fresh `Simulator`**:
//! rebuild the same cells with every non-air cell marked dirty
//! ([`fully_redirtied`] -- exactly what `World::from_parts` does for a world
//! read from a file, for exactly this reason), hand it to `Simulator::new`,
//! and run it to stable. The first recompute then has every dust cell in its
//! active set, so nothing can be left behind by the seeding; anything the
//! settle changes *after* that first recompute goes back through the
//! incremental path, which is why [`resettle_to_fixpoint`] repeats the whole
//! re-dirty-and-settle until a pass changes nothing, and reports how many
//! passes that took.
//!
//! # What a difference means
//!
//! A truly settled world is a fixed point of the physics: recomputing every
//! dust strength from the components as they stand, and re-asking every
//! component whether its state matches its inputs, changes nothing. So any
//! cell where the settled world and its full re-settle disagree is a cell the
//! incremental bookkeeping got wrong -- there is no third possibility, because
//! both worlds hold the same blocks and the oracle starts from the settled
//! world's own component states.
//!
//! This module only measures. Nothing in the shipping compile path calls it.

use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::{SimulationError, Simulator};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// One cell the settled world and its full re-settle disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellDiff {
    pub position: Position,
    pub kind: BlockKind,
    /// `power` as `run_until_stable` left it.
    pub settled_power: u8,
    /// `power` after the full re-settle -- the oracle's answer.
    pub resettled_power: u8,
    /// `lit` as `run_until_stable` left it.
    pub settled_lit: bool,
    /// `lit` after the full re-settle.
    pub resettled_lit: bool,
}

/// The outcome of one differential.
#[derive(Debug, Clone)]
pub struct Resettle {
    /// Every cell whose `power` or `lit` moved under the full re-settle,
    /// in flat-index (YZX) order.
    pub diffs: Vec<CellDiff>,
    /// The re-settled world -- read output lamps and torches out of this to
    /// ask what the oracle thinks the circuit computes.
    pub world: World,
    /// How many full re-dirty-and-settle passes the fixpoint took. 1 means
    /// the first full re-settle was already a fixed point of itself.
    pub passes: usize,
}

/// A copy of `world` with **every** non-air cell marked dirty, so the next
/// `recompute_dust_strengths` treats the whole world as possibly affected.
///
/// Built through `World::from_parts`, which does exactly this for worlds read
/// from files -- no bookkeeping state survives except the blocks themselves.
pub fn fully_redirtied(world: &World) -> World {
    let (size_x, size_y, size_z) = world.size();
    World::from_parts(size_x, size_y, size_z, world.palette().clone(), world.cells().to_vec())
}

/// Fully re-settle `world` until a whole re-dirty-and-settle pass changes
/// nothing, capped at `max_passes`.
///
/// One pass is: [`fully_redirtied`], `Simulator::new` (which recomputes every
/// dust network, since everything is dirty), `run_until_stable`. A second
/// pass exists because everything *after* the first recompute of a pass runs
/// back through the incremental path; repeating until two consecutive passes
/// agree makes the answer a fixed point of the full recompute rather than a
/// single application of it. Every world measured so far reaches the fixpoint
/// in one extra confirming pass.
pub fn resettle_to_fixpoint(
    world: &World,
    max_game_ticks: u64,
    max_passes: usize,
) -> Result<(World, usize), SimulationError> {
    let mut current = world.clone();
    for pass in 1..=max_passes {
        let mut simulator = Simulator::new(fully_redirtied(&current));
        simulator.run_until_stable(max_game_ticks)?;
        let next = simulator.world().clone();
        if worlds_agree(&current, &next) && pass > 1 {
            return Ok((next, pass));
        }
        if worlds_agree(&current, &next) {
            // The very first pass already changed nothing: the input was a
            // true fixed point.
            return Ok((next, pass));
        }
        current = next;
    }
    // Did not reach a fixpoint within the cap -- report the last state with
    // the cap as the pass count; the caller sees `passes == max_passes` and
    // knows the answer is not a proven fixpoint.
    Ok((current, max_passes))
}

/// Whether two same-sized worlds agree on `power` and `lit` at every cell.
fn worlds_agree(a: &World, b: &World) -> bool {
    debug_assert_eq!(a.size(), b.size());
    (0..a.cells().len()).all(|flat| {
        let (x, y, z) = a.decode(flat);
        let before = a.get(x, y, z);
        let after = b.get(x, y, z);
        before.power == after.power && before.lit == after.lit
    })
}

/// Settle-vs-full-re-settle, the differential itself.
///
/// `settled` is a world some `run_until_stable` claims is stable. The result
/// lists every cell whose `power` or `lit` the full re-settle disagrees with.
/// An empty list means the incremental settle told the truth about this
/// world; a non-empty one names exactly the cells it lied about.
pub fn resettle_differential(
    settled: &World,
    max_game_ticks: u64,
) -> Result<Resettle, SimulationError> {
    let (oracle, passes) = resettle_to_fixpoint(settled, max_game_ticks, 8)?;

    let mut diffs = Vec::new();
    for flat in 0..settled.cells().len() {
        let (x, y, z) = settled.decode(flat);
        let before = settled.get(x, y, z);
        let after = oracle.get(x, y, z);
        if before.power != after.power || before.lit != after.lit {
            diffs.push(CellDiff {
                position: Position::new(x, y, z),
                kind: before.kind,
                settled_power: before.power,
                resettled_power: after.power,
                settled_lit: before.lit,
                resettled_lit: after.lit,
            });
        }
    }

    Ok(Resettle { diffs, world: oracle, passes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::BlockState;

    fn named(name: &str, kind: BlockKind) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = name.to_string();
        state
    }

    fn stone() -> BlockState {
        named("minecraft:stone", BlockKind::Solid)
    }

    fn dust() -> BlockState {
        named("minecraft:redstone_wire", BlockKind::RedstoneWire)
    }

    fn lever_on() -> BlockState {
        let mut state = named("minecraft:lever", BlockKind::Lever);
        state.lit = true;
        state
    }

    /// A lever-driven straight wire, settled the normal way.
    fn settled_wire() -> World {
        let mut world = World::new(10, 3, 3);
        world.set(0, 1, 0, lever_on());
        for x in 1..=5 {
            world.set(x, 0, 0, stone());
            world.set(x, 1, 0, dust());
        }
        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(50).expect("a straight wire settles");
        simulator.world().clone()
    }

    #[test]
    fn a_healthy_settle_shows_no_differences() {
        let world = settled_wire();
        let result = resettle_differential(&world, 50).expect("the oracle settles");
        assert!(
            result.diffs.is_empty(),
            "a single-settle world with no incremental history must be a fixed point, got {:?}",
            result.diffs
        );
    }

    /// The instrument can see the thing it exists to see: a dust cell whose
    /// recorded strength disagrees with what the world's own physics says.
    /// Manufactured directly rather than through the incremental settle, so
    /// this test stays meaningful whether or not the incremental settle is
    /// currently capable of producing one.
    #[test]
    fn a_manufactured_stale_cell_is_reported_with_both_values() {
        let mut world = settled_wire();
        // (3,1,0) really carries 13. Record a lie in the snapshot.
        assert_eq!(world.get(3, 1, 0).power, 13, "the rig must be what it claims");
        let mut lied = world.get(3, 1, 0).clone();
        lied.power = 0;
        world.set(3, 1, 0, lied);

        let result = resettle_differential(&world, 50).expect("the oracle settles");
        let diff = result
            .diffs
            .iter()
            .find(|diff| diff.position == Position::new(3, 1, 0))
            .expect("the stale cell must be reported");
        assert_eq!(diff.settled_power, 0, "the snapshot's lie");
        assert_eq!(diff.resettled_power, 13, "the oracle's correction");
        assert_eq!(
            result.world.get(3, 1, 0).power,
            13,
            "the returned world is the corrected one"
        );
    }

    /// **The mechanism, in five blocks.** The measured stale chain in the
    /// negotiated `full_adder` isolation world sits behind a dust edge that
    /// exists in one direction only: `(57,2,99)` *descends* into `(56,1,99)`,
    /// but the climb back is refused because the cell the climb would step on,
    /// `(57,1,99)`, holds wire, and wire does not support a dust step. This
    /// test is that shape reduced to its minimum:
    ///
    /// ```text
    ///   y=2            U  R      U = dust, fed 15 by the redstone block R
    ///   y=1   lever L  M         L, M = dust; M sits directly under U
    ///   y=0         #  #         stone floors under L and M only
    /// ```
    ///
    /// `U -> L` is a descent (the cell above L is air); `L -> U` is a climb
    /// and is refused (`supports_dust_step(M)` is false, M is wire). So when
    /// the lever flips off, `take_dirty` holds only the lever's cell,
    /// `active_dust_networks`' two-hop seeding finds L and M, and the flood
    /// from them follows **outgoing** edges only -- U is never added. The
    /// recompute then sees no source for {L, M} (dust-to-dust feed only
    /// happens through the BFS, and U is not in it) and writes both to 0,
    /// while U still reads 15 and still, by `dust_connections`' own answer,
    /// feeds L at 14.
    ///
    /// **This test asserts the defective behaviour on purpose** -- it is the
    /// pinned reproduction. The fix to `recompute_dust_strengths` must flip
    /// every assertion below marked STALE, at which point this test goes red
    /// and should be rewritten to assert `power == 14` / `13` and an empty
    /// differential.
    #[test]
    fn a_one_way_descent_edge_leaves_the_lower_run_stale() {
        let mut world = World::new(8, 4, 3);
        world.set(0, 1, 0, lever_on()); // beside L
        world.set(1, 0, 0, stone());
        world.set(1, 1, 0, dust()); // L
        world.set(2, 0, 0, stone());
        world.set(2, 1, 0, dust()); // M
        world.set(2, 2, 0, dust()); // U, directly above M -- the measured shape
        world.set(3, 2, 0, named("minecraft:redstone_block", BlockKind::RedstoneBlock));

        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(50).expect("it settles with the lever on");
        assert_eq!(simulator.world().get(1, 1, 0).power, 15, "L beside the lit lever");
        assert_eq!(simulator.world().get(2, 2, 0).power, 15, "U beside the redstone block");

        // Flip the lever off. Only the lever's cell is dirty; the seeding
        // finds L and M; the directed flood never reaches U.
        let mut off = simulator.world().get(0, 1, 0).clone();
        off.lit = false;
        simulator.world_mut().set(0, 1, 0, off);
        simulator.run_until_stable(50).expect("it settles with the lever off");

        let settled = simulator.world().clone();
        assert_eq!(
            settled.get(2, 2, 0).power,
            15,
            "U is untouched and still fed by the redstone block"
        );
        // STALE: the physics says 14 (one step of decay down the descent).
        assert_eq!(
            settled.get(1, 1, 0).power,
            0,
            "L is left stale at 0 -- if this just became 14, the defect is fixed: \
             rewrite this pin to assert the true strengths and an empty differential"
        );
        assert_eq!(settled.get(2, 1, 0).power, 0, "M is left stale at 0");

        // The differential names both cells and the oracle's answer.
        let result = resettle_differential(&settled, 50).expect("the oracle settles");
        let by_position = |x: i32, y: i32, z: i32| {
            result
                .diffs
                .iter()
                .find(|diff| diff.position == Position::new(x, y, z))
                .unwrap_or_else(|| panic!("({x}, {y}, {z}) must be reported stale"))
        };
        assert_eq!(by_position(1, 1, 0).resettled_power, 14);
        assert_eq!(by_position(2, 1, 0).resettled_power, 13);
    }

    #[test]
    fn the_oracle_reaches_a_fixpoint_and_says_how_fast() {
        let world = settled_wire();
        let (oracle, passes) = resettle_to_fixpoint(&world, 50, 8).expect("it settles");
        assert!(passes <= 2, "a healthy wire must fixpoint immediately, took {passes}");
        assert!(worlds_agree(&world, &oracle));
    }
}
