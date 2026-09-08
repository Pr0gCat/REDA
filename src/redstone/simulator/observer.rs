//! Watches a fixed set of positions for their own on/off signal changes, at
//! a cost proportional to the number of watched positions rather than to
//! world volume.
//!
//! This is the mechanism dynamic timing analysis (`crate::timing`) is built
//! on: `Simulator` samples every watched position once per advanced game
//! tick and appends an [`Observation`] only when that position's own signal
//! actually changed since the last sample. A `Simulator` with no observer
//! attached never touches this module at all, so existing behaviour is
//! unchanged.
//!
//! # Why "signal", not simply `lit`
//!
//! For a torch, lamp, lever or repeater, `BlockState::lit` already *is* the
//! on/off signal this module needs -- `propagate::recompute_dust_strengths`
//! and its neighbours only ever write those blocks' `lit` field, never
//! their `power` (`comparator.power = 7` is the one exception, and a
//! comparator is never `lit`-driven in the sense this module cares about
//! either way). A `RedstoneWire` block is the opposite: only `power` is
//! ever written for it (`recompute_dust_strengths`'s own write-back loop);
//! `lit` starts `false` at construction (`compile::dust`) and nothing in
//! this simulator ever sets it to anything else. Watching `lit` directly on
//! a dust cell -- exactly what a wire-merge OR's own junction physically is
//! (`compile::place_merge_gate`) -- would therefore never observe a single
//! change, no matter how many real transitions its power level went
//! through: not "rare", provably zero, for every dust cell that will ever
//! exist. [`signal_of`] is the one place this module reads a block's own
//! on/off state, precisely so this distinction is made once, not
//! independently at both call sites below.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::compile::fragment_synth::identity::{ObservationId, ObservationSite};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

use super::position::Position;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WireObservationPolicy {
    PowerGreaterThanZero,
}

impl WireObservationPolicy {
    fn observes(self, power: u8) -> bool {
        match self {
            WireObservationPolicy::PowerGreaterThanZero => power > 0,
        }
    }
}

pub const WIRE_OBSERVATION_POLICY: WireObservationPolicy =
    WireObservationPolicy::PowerGreaterThanZero;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WireObservationSemantics {
    pub policy: WireObservationPolicy,
    pub semantic_version: u64,
}

pub const WIRE_OBSERVATION_SEMANTICS: WireObservationSemantics = WireObservationSemantics {
    policy: WIRE_OBSERVATION_POLICY,
    semantic_version: 1,
};

/// The on/off signal `position` actually carries, in the sense this module
/// needs: `lit` for everything this simulator ever sets `lit` on, `power >
/// 0` for a `RedstoneWire` cell, which this simulator never sets `lit` on
/// at all -- see this module's own doc comment for why the distinction is
/// not optional.
fn signal_of(world: &World, position: Position) -> bool {
    let state = world.get(position.x, position.y, position.z);
    if state.kind == BlockKind::RedstoneWire {
        WIRE_OBSERVATION_POLICY.observes(state.power)
    } else {
        state.lit
    }
}

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

/// One identity-preserving observation event. Display labels are metadata:
/// neither duplicate labels nor multiple identities at one position merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedObservation {
    pub tick: u64,
    pub id: ObservationId,
    pub position: Position,
    pub display_label: Option<String>,
    pub value: bool,
}

#[derive(Debug, Clone)]
struct WatchRegistration {
    id: Option<ObservationId>,
    label: String,
    display_label: Option<String>,
}

/// A fixed set of watched positions, plus a running log of every `lit`
/// change seen at any of them.
///
/// Two positions can share a label (a declared netlist output and the gate
/// that drives it are the same net under two names in some callers), so
/// labels are not required to be unique -- lookups always go the other way,
/// from position to label.
#[derive(Clone)]
pub struct Observer {
    watched: BTreeMap<Position, Vec<WatchRegistration>>,
    last_value: HashMap<Position, bool>,
    log: Vec<Observation>,
    typed_log: Vec<TypedObservation>,
}

impl Observer {
    /// Watch exactly these positions, each carrying a human-readable label.
    pub fn new(watched: impl IntoIterator<Item = (Position, String)>) -> Self {
        let mut registrations = BTreeMap::<Position, Vec<WatchRegistration>>::new();
        for (position, label) in watched {
            registrations.entry(position).or_default().push(WatchRegistration {
                id: None,
                display_label: Some(label.clone()),
                label,
            });
        }
        Self {
            watched: registrations,
            last_value: HashMap::new(),
            log: Vec::new(),
            typed_log: Vec::new(),
        }
    }

    /// Watch typed sites without using coordinates or labels as identity.
    pub fn typed(sites: impl IntoIterator<Item = ObservationSite>) -> Self {
        let mut registrations = BTreeMap::<Position, Vec<WatchRegistration>>::new();
        for site in sites {
            let position = Position::new(site.at.x, site.at.y, site.at.z);
            let label = site
                .display_label
                .clone()
                .unwrap_or_else(|| format!("{:?}", site.id));
            registrations.entry(position).or_default().push(WatchRegistration {
                id: Some(site.id),
                label,
                display_label: site.display_label,
            });
        }
        for registrations in registrations.values_mut() {
            registrations.sort_by_key(|registration| registration.id);
        }
        Self {
            watched: registrations,
            last_value: HashMap::new(),
            log: Vec::new(),
            typed_log: Vec::new(),
        }
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
        self.typed_log.clear();
        self.last_value.clear();
        for &position in self.watched.keys() {
            self.last_value.insert(position, signal_of(world, position));
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
        for (&position, registrations) in &self.watched {
            let signal = signal_of(world, position);
            if self.last_value.get(&position) != Some(&signal) {
                self.last_value.insert(position, signal);
                for registration in registrations {
                    self.log.push(Observation {
                        tick,
                        position,
                        label: registration.label.clone(),
                        value: signal,
                    });
                    if let Some(id) = registration.id {
                        self.typed_log.push(TypedObservation {
                            tick,
                            id,
                            position,
                            display_label: registration.display_label.clone(),
                            value: signal,
                        });
                    }
                }
            }
        }
    }

    /// Every change observed since the last `reset`, in the order it was
    /// seen (ticks are non-decreasing; within one tick, insertion order
    /// follows the watched set's iteration order).
    pub fn log(&self) -> &[Observation] {
        &self.log
    }

    pub fn typed_log(&self) -> &[TypedObservation] {
        &self.typed_log
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::identity::{
        InstanceId, ObservationId, PrimitiveId, TopologyNodeId,
    };
    use crate::compile::geometry::Anchor;
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

    #[test]
    fn typed_observer_preserves_same_position_identities_and_duplicate_labels() {
        let mut world = World::new(3, 3, 3);
        let position = Position::new(1, 1, 1);
        world.set(position.x, position.y, position.z, torch(false));
        let primitive = ObservationId::PrimitiveOutput(PrimitiveId {
            instance: InstanceId(7),
            node: TopologyNodeId(0),
        });
        let instance = ObservationId::InstanceOutput(InstanceId(7));
        let sites = [primitive, instance].map(|id| ObservationSite {
            id,
            at: Anchor { x: 1, y: 1, z: 1 },
            logical_owner: Some(InstanceId(7)),
            display_label: Some("same".to_string()),
        });
        let mut observer = Observer::typed(sites);
        observer.reset(&world);

        world.set(position.x, position.y, position.z, torch(true));
        observer.sample(&world, 9);

        assert_eq!(observer.watched_count(), 1, "the physical cell is sampled once");
        assert_eq!(observer.typed_log().len(), 2, "both typed identities survive");
        assert_eq!(observer.typed_log()[0].id, primitive);
        assert_eq!(observer.typed_log()[1].id, instance);
        assert!(observer
            .typed_log()
            .iter()
            .all(|event| event.display_label.as_deref() == Some("same")));
        assert_eq!(observer.log().len(), 2, "the compatibility projection also stays lossless");
    }
}
