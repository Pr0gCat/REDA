//! Watches a fixed set of positions for `lit` changes, at a cost proportional
//! to the number of watched positions rather than to world volume.
//!
//! This is the mechanism dynamic timing analysis (`crate::timing`) is built
//! on: `Simulator` samples every watched position once per advanced game
//! tick and appends an [`Observation`] only when that position's `lit` value
//! actually changed since the last sample. A `Simulator` with no observer
//! attached never touches this module at all, so existing behaviour is
//! unchanged.

use std::collections::HashMap;

use crate::redstone::world::storage::World;

use super::position::Position;

/// One recorded change to a watched position's `lit` state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// Absolute game tick (`Simulator::current_tick`) at which this change
    /// was observed -- i.e. the tick just reached when the sample ran.
    pub tick: u64,
    pub position: Position,
    /// The human-readable name this position was registered under (typically
    /// a netlist signal name).
    pub label: String,
    pub value: bool,
}

/// A fixed set of watched positions, plus a running log of every `lit`
/// change seen at any of them.
///
/// Two positions can share a label (a declared netlist output and the gate
/// that drives it are the same net under two names in some callers), so
/// labels are not required to be unique -- lookups always go the other way,
/// from position to label.
pub struct Observer {
    watched: HashMap<Position, String>,
    last_value: HashMap<Position, bool>,
    log: Vec<Observation>,
}

impl Observer {
    /// Watch exactly these positions, each carrying a human-readable label.
    pub fn new(watched: impl IntoIterator<Item = (Position, String)>) -> Self {
        Observer { watched: watched.into_iter().collect(), last_value: HashMap::new(), log: Vec::new() }
    }

    /// How many positions this observer watches.
    pub fn watched_count(&self) -> usize {
        self.watched.len()
    }

    /// Clear the log and re-baseline every watched position's current value
    /// from `world`, without emitting observations for that baseline.
    ///
    /// Call this right before the input transition you want to measure, so
    /// the resulting log holds only the changes caused by that transition.
    pub fn reset(&mut self, world: &World) {
        self.log.clear();
        self.last_value.clear();
        for &position in self.watched.keys() {
            let lit = world.get(position.x, position.y, position.z).lit;
            self.last_value.insert(position, lit);
        }
    }

    /// Compare every watched position's current value in `world` against
    /// what was last seen, and log any that changed, at `tick`.
    ///
    /// Cost is `O(watched positions)`, never `O(world volume)` -- this is
    /// the whole point of watching a fixed set rather than scanning the
    /// world, the same principle that keeps redstone propagation itself
    /// sparse (see `propagate::recompute_dust_strengths`).
    pub(super) fn sample(&mut self, world: &World, tick: u64) {
        for (&position, label) in &self.watched {
            let lit = world.get(position.x, position.y, position.z).lit;
            if self.last_value.get(&position) != Some(&lit) {
                self.last_value.insert(position, lit);
                self.log.push(Observation { tick, position, label: label.clone(), value: lit });
            }
        }
    }

    /// Every change observed since the last `reset`, in the order it was
    /// seen (ticks are non-decreasing; within one tick, insertion order
    /// follows the watched set's iteration order).
    pub fn log(&self) -> &[Observation] {
        &self.log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState};

    fn torch(lit: bool) -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::Torch;
        state.name = "minecraft:redstone_torch".to_string();
        state.lit = lit;
        state
    }

    #[test]
    fn a_fresh_observer_has_an_empty_log() {
        let observer = Observer::new(vec![(Position::new(0, 0, 0), "x".to_string())]);
        assert!(observer.log().is_empty());
        assert_eq!(observer.watched_count(), 1);
    }

    #[test]
    fn reset_baselines_without_logging_anything() {
        let mut world = World::new(3, 3, 3);
        let pos = Position::new(1, 1, 1);
        world.set(pos.x, pos.y, pos.z, torch(true));

        let mut observer = Observer::new(vec![(pos, "x".to_string())]);
        observer.reset(&world);

        assert!(observer.log().is_empty(), "reset must not itself produce an observation");
    }

    #[test]
    fn sample_logs_only_positions_whose_value_actually_changed() {
        let mut world = World::new(3, 3, 3);
        let watched = Position::new(1, 1, 1);
        let quiet = Position::new(2, 2, 2);
        world.set(watched.x, watched.y, watched.z, torch(true));
        world.set(quiet.x, quiet.y, quiet.z, torch(false));

        let mut observer =
            Observer::new(vec![(watched, "watched".to_string()), (quiet, "quiet".to_string())]);
        observer.reset(&world);

        // Flip only `watched`.
        world.set(watched.x, watched.y, watched.z, torch(false));
        observer.sample(&world, 7);

        assert_eq!(
            observer.log(),
            &[Observation { tick: 7, position: watched, label: "watched".to_string(), value: false }]
        );
    }

    #[test]
    fn repeated_sampling_with_no_change_logs_nothing_new() {
        let mut world = World::new(3, 3, 3);
        let pos = Position::new(0, 0, 0);
        world.set(pos.x, pos.y, pos.z, torch(true));

        let mut observer = Observer::new(vec![(pos, "x".to_string())]);
        observer.reset(&world);

        observer.sample(&world, 1);
        observer.sample(&world, 2);
        observer.sample(&world, 3);

        assert!(observer.log().is_empty(), "nothing changed, so nothing should be logged");
    }
}
