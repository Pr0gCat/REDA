//! Two things: the sweep that calibrates the extractor against the simulator,
//! and the sweep that runs it over every circuit this project ships, on both of
//! `compile`'s paths.
//!
//! Order matters for reading this file. The extractor is a *structural*
//! predicate -- see the module doc for why activation has to be dropped -- and a
//! structural predicate is exactly the kind of thing this project has been
//! burned by restating. So the first sweep below builds worlds, drives them
//! through the real `Simulator`, differences each reading against a control, and
//! fails unless the extractor said the same thing. Only after that does the
//! second sweep get to claim anything about a compiled circuit.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use super::*;
use crate::circuits::and4::build_and4_netlist;
use crate::circuits::full_adder::build_full_adder_netlist;
use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
use crate::circuits::verilog;
use crate::compile::lowering::{lower, lower_optimised};
use crate::compile::planner::{self, PortPlacements};
use crate::compile::{compile, compile_legacy, PlannerKind};
use crate::redstone::simulator::component::torch_support_position;
use crate::redstone::simulator::{SimulationError, Simulator};
use crate::redstone::world::block::{BlockState, Face};

// =====================================================================
// Part 1 -- the extractor, differenced against the simulator
// =====================================================================
//
// `docs/derived/coupling-mechanisms.md` already measured *which* mechanisms
// exist. What it cannot do is prove that the walk in this module implements
// them, because it was written before this module existed and it probes a rig,
// not an extractor. So this sweep re-runs the same experiment with the
// extractor in the loop: build a rig, ask the extractor whether the emitter's
// domain reaches the receiver, then run the rig through the `Simulator` twice --
// once as built and once with the emitter written as air -- and compare.
//
// A disagreement in either direction is a failure. The extractor saying `yes`
// where the simulator says `no` is a false positive that would make every
// circuit result below meaningless noise; the extractor saying `no` where the
// simulator says `yes` is precisely the class of blindness that shipped twice.

/// Room for `RIG_ORIGIN` plus or minus four on every axis. Same shape as
/// `tests/coupling_mechanisms.rs`'s rig, deliberately: this sweep is that
/// experiment with the extractor added, not a different one.
const RIG_SIZE: (i32, i32, i32) = (17, 17, 17);
const RIG_ORIGIN: Position = Position { x: 8, y: 8, z: 8 };
const RIG_MAX_TICKS: u64 = 400;

const CALIBRATION_ARTIFACT: &str = "docs/derived/realised-graph-extraction.md";
const EXTRAS_ARTIFACT: &str = "docs/derived/realised-graph-extras.md";

fn named(kind: BlockKind, name: &str) -> BlockState {
    let mut state = BlockState::air();
    state.kind = kind;
    state.name = name.to_string();
    state
}

fn rig_stone() -> BlockState {
    named(BlockKind::Solid, "minecraft:stone")
}

fn rig_glass() -> BlockState {
    named(BlockKind::Glass, "minecraft:glass")
}

fn rig_lamp() -> BlockState {
    named(BlockKind::Lamp, "minecraft:redstone_lamp")
}

fn rig_dust() -> BlockState {
    named(BlockKind::RedstoneWire, "minecraft:redstone_wire")
}

fn rig_redstone_block() -> BlockState {
    named(BlockKind::RedstoneBlock, "minecraft:redstone_block")
}

/// What can stand between the emitter and the receiver.
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
    fn label(self) -> &'static str {
        match self {
            Fill::Air => "air",
            Fill::Stone => "stone",
            Fill::Glass => "glass",
            Fill::Lamp => "lamp",
            Fill::Dust => "dust",
        }
    }

    fn state(self) -> BlockState {
        match self {
            Fill::Air => BlockState::air(),
            Fill::Stone => rig_stone(),
            Fill::Glass => rig_glass(),
            Fill::Lamp => rig_lamp(),
            Fill::Dust => rig_dust(),
        }
    }
}

/// Everything that can stand in the emitter cell.
///
/// The three inert kinds are the negative controls. Without them a sweep of
/// all-`.` rows would be indistinguishable from an extractor that had gone
/// blind and a simulator agreeing with it by accident.
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
}

const EMITTERS: [Kind; 10] = [
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
];

impl Kind {
    fn label(self) -> &'static str {
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
        }
    }

    /// Whether this kind needs an external source before it emits anything.
    /// Button and pressure plate are deliberately absent from `EMITTERS`:
    /// `run_until_stable` refuses any world containing one
    /// (`SimulationError::UnsupportedComponent`), so no differenced reading is
    /// available for them and a row taken from `Simulator::new` alone would be
    /// a different experiment wearing the same table.
    fn needs_feed(self) -> bool {
        matches!(self, Kind::Dust | Kind::Repeater | Kind::Comparator)
    }

    fn state(self, facing: Facing) -> BlockState {
        match self {
            Kind::Stone => rig_stone(),
            Kind::Glass => rig_glass(),
            Kind::Lamp => rig_lamp(),
            Kind::RedstoneBlock => rig_redstone_block(),
            Kind::Dust => rig_dust(),
            Kind::Repeater => {
                let mut state = named(BlockKind::Repeater, "minecraft:repeater");
                state.facing = Some(facing);
                state.delay = 1;
                state.lit = false;
                state
            }
            Kind::Comparator => {
                let mut state = named(BlockKind::Comparator, "minecraft:comparator");
                state.facing = Some(facing);
                state
            }
            Kind::Torch => {
                let mut state = named(BlockKind::Torch, "minecraft:redstone_torch");
                state.lit = true;
                state
            }
            Kind::WallTorch => {
                let mut state = named(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
                state.facing = Some(facing);
                state.lit = true;
                state
            }
            Kind::Lever => {
                let mut state = named(BlockKind::Lever, "minecraft:lever");
                state.lit = true;
                state.face = Some(Face::Floor);
                state.facing = Some(Facing::North);
                state
            }
        }
    }
}

/// The facings each kind is worth sweeping. A kind that ignores `facing` still
/// carries one, because `facing` also names the side its feed goes on.
fn facings_of(kind: Kind) -> Vec<Facing> {
    match kind {
        Kind::Repeater | Kind::Comparator | Kind::WallTorch => {
            vec![Facing::North, Facing::South, Facing::East, Facing::West]
        }
        _ => vec![Facing::North],
    }
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

fn adjacent(a: Position, b: Position) -> bool {
    ALL_SIX.into_iter().any(|d| a.offset(d) == b)
}

/// One rig: emitter at `RIG_ORIGIN`, an optional mediator one step out, the
/// receiver one step beyond that.
#[derive(Clone, Copy)]
struct Rig {
    kind: Kind,
    facing: Facing,
    mediator: Option<Fill>,
    direction: Facing,
}

impl Rig {
    fn emitter(self) -> Position {
        RIG_ORIGIN
    }

    fn mediator_cell(self) -> Option<Position> {
        self.mediator.map(|_| RIG_ORIGIN.offset(self.direction))
    }

    fn receiver(self) -> Position {
        match self.mediator {
            Some(_) => RIG_ORIGIN.offset(self.direction).offset(self.direction),
            None => RIG_ORIGIN.offset(self.direction),
        }
    }

    /// The single redstone block that feeds an emitter which cannot power
    /// itself. A redstone block is the one source in this vocabulary that
    /// drives dust while powering **no** block
    /// (`taxonomy::power_emitted_by`'s `RedstoneBlock` arm), so the feed
    /// cannot leak into the mediator.
    fn feed(self) -> Option<Position> {
        self.kind
            .needs_feed()
            .then(|| RIG_ORIGIN.offset(self.facing))
    }

    /// Whether this rig can be built at all. The feed is a second source, so it
    /// must neither occupy nor touch the mediator or the receiver -- otherwise
    /// the control is not quiescent and nothing can be attributed. Refused
    /// structurally, before a world exists.
    fn valid(self) -> bool {
        let Some(feed) = self.feed() else {
            return true;
        };
        let receiver = self.receiver();
        if feed == receiver || adjacent(feed, receiver) {
            return false;
        }
        match self.mediator_cell() {
            Some(mediator) => feed != mediator && !adjacent(feed, mediator),
            None => true,
        }
    }

    /// `removed` writes the emitter cell as air, which is the control.
    fn build(self, removed: bool) -> World {
        let mut world = World::new(RIG_SIZE.0, RIG_SIZE.1, RIG_SIZE.2);
        let put = |world: &mut World, at: Position, state: BlockState| {
            world.set(at.x, at.y, at.z, state);
        };

        if let (Some(cell), Some(fill)) = (self.mediator_cell(), self.mediator) {
            put(&mut world, cell, fill.state());
        }
        if let Some(feed) = self.feed() {
            put(&mut world, feed, rig_redstone_block());
        }
        if removed {
            put(&mut world, self.emitter(), BlockState::air());
        } else {
            put(&mut world, self.emitter(), self.kind.state(self.facing));
        }
        // Last, so no other role's write can land on it.
        put(&mut world, self.receiver(), rig_dust());
        world
    }
}

/// What one cell of the calibration table says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// The simulator and the extractor both say coupled.
    BothCoupled,
    /// Both say not coupled.
    BothClear,
    /// The extractor claims an edge the simulator does not have.
    FalsePositive,
    /// The simulator has an edge the extractor cannot see. This is the class
    /// that shipped twice.
    Blind,
    /// The control was not quiescent, so nothing can be attributed.
    Contaminated,
    /// The rig could not be built (the feed would touch a cell under test).
    Invalid,
    /// The world never settled.
    Diverged,
}

impl Verdict {
    fn code(self) -> &'static str {
        match self {
            Verdict::BothCoupled => "J",
            Verdict::BothClear => ".",
            Verdict::FalsePositive => "+",
            Verdict::Blind => "X",
            Verdict::Contaminated => "~",
            Verdict::Invalid => "x",
            Verdict::Diverged => "!",
        }
    }

    fn is_disagreement(self) -> bool {
        matches!(self, Verdict::FalsePositive | Verdict::Blind)
    }
}

fn settle(world: World) -> Option<World> {
    let mut simulator = Simulator::new(world);
    match simulator.run_until_stable(RIG_MAX_TICKS) {
        Ok(_) => Some(simulator.world().clone()),
        Err(SimulationError::UnsupportedComponent { .. }) => Some(simulator.world().clone()),
        Err(SimulationError::Diverged { .. }) => None,
    }
}

/// Ask the extractor -- the very same [`reach_of`] the circuit sweep uses --
/// whether the emitter's power reaches the receiver cell.
fn extractor_says_coupled(rig: Rig) -> bool {
    let world = rig.build(false);
    let reach = reach_of(&world, &[rig.emitter()]);
    reach.arrival.contains_key(&rig.receiver())
}

/// Run one rig both ways and compare the extractor against the difference.
fn calibrate(rig: Rig) -> Verdict {
    if !rig.valid() {
        return Verdict::Invalid;
    }
    let (Some(driven), Some(control)) = (settle(rig.build(false)), settle(rig.build(true))) else {
        return Verdict::Diverged;
    };
    let receiver = rig.receiver();
    let read = |world: &World| world.get(receiver.x, receiver.y, receiver.z).power;
    // The receiver is bare dust with air beneath it, so its quiescent reading
    // is zero and anything else in the control means a second source is in
    // play.
    if read(&control) != 0 {
        return Verdict::Contaminated;
    }

    match (read(&driven) != 0, extractor_says_coupled(rig)) {
        (true, true) => Verdict::BothCoupled,
        (false, false) => Verdict::BothClear,
        (false, true) => Verdict::FalsePositive,
        (true, false) => Verdict::Blind,
    }
}

/// Every rig the calibration sweeps: each emitter, at each of its facings,
/// with no mediator and with each of the five mediator materials, in all six
/// directions.
fn calibration_rigs() -> Vec<Rig> {
    let mut rigs = Vec::new();
    for kind in EMITTERS {
        for facing in facings_of(kind) {
            for mediator in [None].into_iter().chain(MEDIATORS.map(Some)) {
                for direction in ALL_SIX {
                    rigs.push(Rig {
                        kind,
                        facing,
                        mediator,
                        direction,
                    });
                }
            }
        }
    }
    rigs
}

fn calibration_table() -> String {
    let mut out = String::new();
    out.push_str(
        "# The realised-graph extractor, differenced against the simulator\n\n\
         **Generated. Do not edit by hand.** Regenerate with\n\n```\n\
         cargo test --release --lib -- --ignored \
         compile::coupling::tests::regenerate_the_calibration_table\n```\n\n\
         and `the_extractor_agrees_with_the_simulator` in \
         `src/compile/coupling/tests.rs` fails if this text and the extractor \
         ever disagree.\n\n\
         Each row is one emitter, at one facing, with one mediator material \
         between it and a bare dust receiver. Each column is the direction the \
         receiver lies in. The emitter is driven, the world is run to stable, \
         the receiver's dust strength is read, and the whole thing is repeated \
         with the emitter cell written as **air** as a control; a coupling is a \
         reading that changed. `compile::coupling::reach_of` -- the same walk \
         the circuit sweep uses -- is then asked whether it reaches the same \
         cell, and the two answers are compared.\n\n\
         `J` both say coupled \u{b7} `.` both say clear \u{b7} \
         `+` **extractor claims an edge the simulator does not have** \u{b7} \
         `X` **simulator has an edge the extractor cannot see** \u{b7} \
         `~` contaminated (the control was not quiescent) \u{b7} \
         `x` rig invalid (the feed would touch a cell under test) \u{b7} \
         `!` diverged.\n\n\
         Button and pressure plate are absent on purpose: `run_until_stable` \
         refuses any world containing one, so no differenced reading exists for \
         them. `docs/derived/coupling-mechanisms.md` reports their load-only \
         rows.\n\n```\n",
    );
    out.push_str("emitter          facing  mediator  N S E W U D\n");

    let mut rows: BTreeMap<(usize, usize, usize), Vec<Verdict>> = BTreeMap::new();
    for rig in calibration_rigs() {
        let kind_index = EMITTERS.iter().position(|&k| k == rig.kind).unwrap();
        let facing_index = facings_of(rig.kind)
            .iter()
            .position(|&f| f == rig.facing)
            .unwrap();
        let mediator_index = match rig.mediator {
            None => 0,
            Some(fill) => 1 + MEDIATORS.iter().position(|&m| m == fill).unwrap(),
        };
        rows.entry((kind_index, facing_index, mediator_index))
            .or_default()
            .push(calibrate(rig));
    }

    for ((kind_index, facing_index, mediator_index), verdicts) in &rows {
        let kind = EMITTERS[*kind_index];
        let facing = facings_of(kind)[*facing_index];
        let mediator = match mediator_index {
            0 => "-",
            other => MEDIATORS[other - 1].label(),
        };
        let facing_label = if facings_of(kind).len() > 1 {
            short(facing)
        } else {
            "-"
        };
        let codes: Vec<&str> = verdicts.iter().map(|v| v.code()).collect();
        let _ = writeln!(
            out,
            "{:<16} {:<7} {:<9} {}",
            kind.label(),
            facing_label,
            mediator,
            codes.join(" ")
        );
    }
    out.push_str("```\n");

    let all: Vec<Verdict> = rows.values().flatten().copied().collect();
    let coupled = all.iter().filter(|v| **v == Verdict::BothCoupled).count();
    let clear = all.iter().filter(|v| **v == Verdict::BothClear).count();
    let disagreements = all.iter().filter(|v| v.is_disagreement()).count();
    let _ = write!(
        out,
        "\n## Summary, computed from the runs above\n\n\
         * {} rigs measured, of which **{coupled} couple** and {clear} do not.\n\
         * **{disagreements} disagreements** between the extractor and the \
         simulator.\n\
         * {} rigs were refused as invalid (the feed would have touched a cell \
         under test) and {} were contaminated.\n",
        all.len(),
        all.iter().filter(|v| **v == Verdict::Invalid).count(),
        all.iter().filter(|v| **v == Verdict::Contaminated).count(),
    );
    out
}

/// **This is what makes every number below it mean anything.**
///
/// The extractor is structural: it drops the activation gates, because a
/// compiled world is not settled and its levers are written off. A structural
/// predicate cannot be read off a settled world, so it is checked the only way
/// left -- against the settled worlds it is supposed to summarise, over a full
/// sweep, differenced against a control each time.
#[test]
fn the_extractor_agrees_with_the_simulator() {
    let mut disagreements = Vec::new();
    let mut coupled = 0usize;
    for rig in calibration_rigs() {
        let verdict = calibrate(rig);
        if verdict == Verdict::BothCoupled {
            coupled += 1;
        }
        if verdict.is_disagreement() {
            disagreements.push(format!(
                "  {:?} facing {} across {:?} toward {}: {verdict:?}",
                rig.kind,
                short(rig.facing),
                rig.mediator.map(|m| m.label()),
                short(rig.direction),
            ));
        }
    }
    assert!(
        disagreements.is_empty(),
        "the extractor and the simulator disagree on {} rig(s):\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
    // A sweep that couples nothing agrees with a blind extractor trivially.
    assert!(
        coupled > 50,
        "the calibration sweep found only {coupled} couplings, which is too few \
         for its agreement to mean anything -- the rig is broken"
    );
}

#[test]
fn the_committed_calibration_table_is_what_runs_today() {
    let fresh = calibration_table();
    let committed = std::fs::read_to_string(CALIBRATION_ARTIFACT)
        .unwrap_or_else(|e| panic!("{CALIBRATION_ARTIFACT} must exist: {e}"));
    if fresh != committed {
        let first = fresh
            .lines()
            .zip(committed.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}: fresh `{a}` vs committed `{b}`", i + 1))
            .unwrap_or_else(|| "the files differ in length".to_string());
        panic!("{CALIBRATION_ARTIFACT} is stale -- {first}");
    }
}

#[test]
#[ignore = "writes docs/derived/realised-graph-extraction.md"]
fn regenerate_the_calibration_table() {
    std::fs::write(CALIBRATION_ARTIFACT, calibration_table()).expect("artifact must be writable");
}

// =====================================================================
// Part 2 -- the two shipped bugs, reproduced against this checker
// =====================================================================
//
// `super`'s own test module already has these two geometries, where they prove
// `verify_connectivity` cannot see them. Here they prove the converse for the
// widened relation, and they pin the mechanism number as well as the fact: a
// checker that found the edge and called it mechanism 1 would be reporting a
// coincidence.
//
// Both are hand-built worlds. They say the checker sees the mechanism; they do
// not say the router still produces the geometry. What says *that* is the
// footprint injection recorded in this file's commit message, which reverts the
// real fix and rebuilds the real circuit.

/// A minimal two-net world: `driver` at `emitter`, a conductive block above it,
/// and a foreign net's dust on top of that block.
fn leak_over_a_conductor(driver: BlockState) -> (World, Position, Position, Position) {
    let mut world = World::new(9, 9, 9);
    let emitter = Position::new(4, 1, 4);
    let mediator = emitter.up();
    let foreign = mediator.up();

    world.set(emitter.x, emitter.y - 1, emitter.z, rig_stone());
    world.set(emitter.x, emitter.y, emitter.z, driver);
    world.set(mediator.x, mediator.y, mediator.z, rig_stone());
    world.set(foreign.x, foreign.y, foreign.z, rig_dust());
    (world, emitter, mediator, foreign)
}

#[test]
fn the_lever_bug_is_an_extra_edge_of_mechanism_three() {
    let mut lever = named(BlockKind::Lever, "minecraft:lever");
    lever.lit = true;
    lever.face = Some(Face::Floor);
    lever.facing = Some(Facing::North);
    let (world, emitter, mediator, foreign) = leak_over_a_conductor(lever);

    let reach = reach_of(&world, &[emitter]);
    let arrival = reach
        .arrival
        .get(&foreign)
        .expect("a lit lever strongly powers the block above it, and that block drives the dust on top");
    assert_eq!(arrival.mechanism, Mechanism::StrongBlockToDust);
    assert_eq!(arrival.via, Some(mediator));
    assert_eq!(arrival.from, emitter);

    // The control that says the mediator is what carries it: glass is a full
    // cube that does not conduct, and the same rig goes dark.
    let mut insulated = world.clone();
    insulated.set(mediator.x, mediator.y, mediator.z, rig_glass());
    assert!(
        !reach_of(&insulated, &[emitter])
            .arrival
            .contains_key(&foreign),
        "with a non-conductive mediator there is no edge, so the stone is what carried it"
    );
}

#[test]
fn the_torch_bug_is_an_extra_edge_of_mechanism_three() {
    let mut torch = named(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
    torch.facing = Some(Facing::North);
    torch.lit = true;
    let (world, emitter, mediator, foreign) = leak_over_a_conductor(torch);

    let reach = reach_of(&world, &[emitter]);
    let arrival = reach
        .arrival
        .get(&foreign)
        .expect("a lit torch strongly powers the block above it, and that block drives the dust on top");
    assert_eq!(arrival.mechanism, Mechanism::StrongBlockToDust);
    assert_eq!(arrival.via, Some(mediator));

    // And the asymmetry that makes a torch invert is still respected: the
    // torch's own support is *not* in its reach, at any facing.
    for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let mut turned = world.clone();
        let mut state = named(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
        state.facing = Some(facing);
        state.lit = true;
        turned.set(emitter.x, emitter.y, emitter.z, state.clone());
        let support = emitter.offset(facing.opposite());
        turned.set(support.x, support.y, support.z, rig_stone());
        let mut probe = turned.clone();
        let beyond = support.offset(facing.opposite());
        probe.set(beyond.x, beyond.y, beyond.z, rig_dust());
        assert!(
            !reach_of(&probe, &[emitter]).arrival.contains_key(&beyond),
            "a torch facing {facing:?} must not power its own support, so nothing beyond it is reached"
        );
    }
}

/// The weak-power class no dust probe can find: a wire ending against a block
/// powers it weakly, which reaches a torch attached to that block and nothing
/// else. `reach_of` must record the block as readable even though it drives no
/// dust.
#[test]
fn weak_power_into_a_block_is_recorded_even_though_no_dust_moves() {
    let mut world = World::new(9, 9, 9);
    let wire = Position::new(4, 4, 4);
    let block = wire.down();
    // Directly *under* the block, two layers below the wire. Beside it would be
    // wrong and the first version of this test was: a dust cell one step out and
    // one layer down is a `dust_connections` descend, so the probe would be
    // reached by mechanism 1 and this would pass for the wrong reason. Two
    // layers is out of that relation's range entirely, and only the block can
    // bridge it.
    let probe = block.down();

    world.set(block.x, block.y, block.z, rig_stone());
    world.set(wire.x, wire.y, wire.z, rig_dust());
    world.set(probe.x, probe.y, probe.z, rig_dust());
    world.set(probe.x, probe.y - 1, probe.z, rig_stone());

    let reach = reach_of(&world, &[wire]);
    assert!(
        reach.readable.contains_key(&block),
        "dust weakly powers the block beneath it, and mechanism 4's whole class \
         lives in that reading"
    );
    assert!(
        !reach.arrival.contains_key(&probe),
        "a weakly powered block re-drives no dust (mechanism 5 does not exist), \
         so the probe must stay out of reach"
    );

    // And the control that says the probe is reachable at all: swap the wire
    // for a lever, which powers the same block *strongly*, and the same probe
    // is driven.
    let mut strongly = world.clone();
    let mut lever = named(BlockKind::Lever, "minecraft:lever");
    lever.lit = true;
    lever.face = Some(Face::Floor);
    lever.facing = Some(Facing::North);
    strongly.set(wire.x, wire.y, wire.z, lever);
    assert!(
        reach_of(&strongly, &[wire]).arrival.contains_key(&probe),
        "with strong power on the same block the same probe is driven, so the \
         negative above is about the power being weak and not about the geometry"
    );
}

// =====================================================================
// Part 3 -- every circuit, on both of `compile`'s paths
// =====================================================================

/// Which of `compile`'s two placers produced a world.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Path {
    /// Relaxation places, A* routes -- `compile`'s first choice, at the trial's
    /// own rip-up budget so the candidate is byte-identical to the one it would
    /// have shipped.
    Relaxation,
    /// The row/channel/track emitter places, and the planner realises from that
    /// seed. `compile`'s fallback.
    Legacy,
}

impl Path {
    fn label(self) -> &'static str {
        match self {
            Path::Relaxation => "relaxation",
            Path::Legacy => "legacy",
        }
    }

    fn kind(self) -> PlannerKind {
        match self {
            Path::Relaxation => PlannerKind::Unified3d,
            Path::Legacy => PlannerKind::Legacy,
        }
    }
}

struct Realisation {
    world: World,
    reservation: Reservation,
    nets: Vec<Net>,
    ports: planner::CandidatePorts,
}

/// Build `netlist` on one path, keeping the ownership the four physical
/// invariants ran against.
///
/// `None` when that path cannot build this circuit -- segment_a and
/// seven_segment place by relaxation and then fail to route, which is exactly
/// why `compile` falls back to the emitter for them.
fn realise(netlist: &Netlist, path: Path) -> Option<Realisation> {
    let (candidate, size) = match path {
        Path::Relaxation => {
            // The same call, with the same budget, `compile` makes first.
            let candidate = planner::plan_from_netlist_within(
                netlist,
                &PortPlacements::default(),
                planner::TRIAL_RIP_UP_ROUNDS,
            )
            .ok()?;
            let size = planner::candidate_world_size(&candidate);
            (candidate, size)
        }
        Path::Legacy => {
            // `compile_legacy` runs the emitter and then realises from its own
            // seed; re-deriving the seed from the emission it kept is what
            // `seed_from_legacy_parts` is for, and the world equality assert
            // below is what says the re-derivation landed on the same circuit.
            let compiled = compile_legacy(netlist).ok()?;
            let emission = compiled
                .legacy_emission()
                .expect("compile_legacy always keeps its emission");
            let seed = planner::seed_from_legacy_parts(netlist, emission).ok()?;
            (seed, compiled.world.size())
        }
    };

    let verified = planner::verify_and_expose(&candidate, netlist, size).ok()?;
    Some(Realisation {
        world: verified.realised.world,
        reservation: verified.reservation,
        nets: verified.nets,
        ports: verified.realised.ports,
    })
}

fn worlds_agree(a: &World, b: &World) -> bool {
    if a.size() != b.size() {
        return false;
    }
    let (size_x, size_y, size_z) = a.size();
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                if a.get(x, y, z) != b.get(x, y, z) {
                    return false;
                }
            }
        }
    }
    true
}

fn non_air(world: &World) -> usize {
    let (size_x, size_y, size_z) = world.size();
    let mut count = 0;
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

/// The six circuits the Stage 3 condition names, each with the lowering its own
/// acceptance test uses -- the hand-written four need none (they are pure NOR
/// already, and `tests/reference_circuits.rs` compiles them raw for the block
/// counts it pins), `verilog:and4` takes plain `lower` and
/// `verilog:seven_segment` the production `lower_optimised`, matching
/// `tests/verilog_frontend.rs`.
///
/// The Verilog netlists come from `VerilogCircuit::baked_netlist`, not from
/// Yosys: this test runs inside `cargo test`, which must not need `python` and
/// `yowasp-yosys`. `tests/verilog_frontend.rs`'s
/// `the_baked_netlists_match_fresh_synthesis` is what keeps the baked copy from
/// drifting away from what synthesis produces.
fn circuits() -> Vec<(String, Netlist)> {
    let mut all: Vec<(String, Netlist)> = vec![
        ("and4".to_string(), build_and4_netlist().0),
        ("full_adder".to_string(), build_full_adder_netlist().0),
        ("segment_a".to_string(), build_single_segment_netlist(0).0),
        (
            "seven_segment".to_string(),
            build_seven_segment_netlist().0,
        ),
    ];
    for circuit in verilog::CIRCUITS {
        let (gate_level, _labels) = circuit.baked_netlist();
        let lowered = match circuit.name {
            "verilog:seven_segment" => lower_optimised(&gate_level),
            _ => lower(&gate_level),
        }
        .unwrap_or_else(|e| panic!("{} must lower: {e}", circuit.name));
        all.push((circuit.name.to_string(), lowered));
    }
    all
}

/// **The deliverable.** Every circuit, both compile paths, the complete
/// electrical graph against the graph the netlist intends.
///
/// The result is a *record*, not a pass/fail: this phase was told to measure and
/// to report what it finds without fixing it, so the artifact at
/// [`EXTRAS_ARTIFACT`] carries every finding with its address, both nets and its
/// mechanism, and this compares against it byte for byte. That makes both
/// directions loud -- a **new** extra edge fails, and so does an edge that
/// *stops* existing, which is the notice that somebody closed one and the record
/// is now out of date.
fn sweep_report() -> String {
    let mut out = String::new();
    out.push_str(
        "# Every edge realisation adds, per circuit, per compile path\n\n\
         **Generated. Do not edit by hand.** Regenerate with\n\n```\n\
         cargo test --release --lib -- --ignored \
         compile::coupling::tests::regenerate_the_extras_record\n```\n\n\
         and `the_realised_graph_of_every_circuit_is_what_is_recorded` in \
         `src/compile/coupling/tests.rs` fails if this text and the compiler ever \
         disagree -- in **either** direction. A new extra edge fails it; so does \
         an extra edge that stops existing, which is how somebody closing one \
         finds out this file needs updating.\n\n\
         What an extra edge is, and how it is extracted, lives in \
         `src/compile/coupling.rs`'s module doc. In short: a **domain** is one \
         electrical source plus everything the netlist says it drives, and an \
         **extra edge** is a hop that leaves a domain's own territory and lands \
         on a cell some other net owns, with nothing in the netlist joining the \
         two. `contaminating N cell(s)` is how far that one hop then spreads by \
         ordinary conduction; it is the size of the damage, not a second cause.\n\n\
         The extractor is differenced against the `Simulator` on a sweep of \
         three-cell rigs (`docs/derived/realised-graph-extraction.md`), and the \
         findings below are put back through the `Simulator` inside the circuit \
         by `the_extra_edges_are_real_when_the_simulator_runs_the_circuit`, which \
         prints, for each, an input vector where the contaminated net is \
         genuinely low and its own wire reads 15 -- plus a one-block control that \
         puts it back to 0.\n\n",
    );

    let mut sections = Vec::new();
    let mut all_edges: Vec<ExtraEdge> = Vec::new();
    let mut all_reads: Vec<ForeignReader> = Vec::new();
    let mut shipping_edges = 0usize;
    let mut through_a_gate_support = 0usize;
    let mut between_two_inputs_of_that_gate = 0usize;
    for (name, netlist) in circuits() {
        let shipped = compile(&netlist).unwrap_or_else(|e| panic!("{name} must compile: {e}"));
        let shipped_kind = shipped.planner_kind();
        let _ = writeln!(
            out,
            "* `{name}`: {} gates, {} blocks, `compile()` takes the **{:?}** path.",
            netlist.gates.len(),
            non_air(&shipped.world),
            shipped_kind,
        );

        for path in [Path::Relaxation, Path::Legacy] {
            let ships = shipped_kind == path.kind();
            let Some(realisation) = realise(&netlist, path) else {
                sections.push(format!(
                    "## `{name}` / {}\n\nThis path cannot build this circuit at all: relaxation \
                     places it and then fails to route, which is why `compile` falls back.\n",
                    path.label()
                ));
                continue;
            };

            // The anchor. When this is the path `compile` took, the world just
            // measured must be the world that ships, cell for cell -- otherwise a
            // clean result could be a clean result about some other circuit.
            if ships {
                assert!(
                    worlds_agree(&realisation.world, &shipped.world),
                    "{name} / {}: the measured world is not the one compile() ships",
                    path.label()
                );
            }

            let report = extra_edges(
                &realisation.world,
                &realisation.reservation,
                &netlist,
                &realisation.nets,
                &realisation.ports.gate_output_positions,
                &realisation.ports.input_positions,
            );
            assert!(
                report.domains > 0 && report.reached_cells > 0,
                "{name} / {}: nothing was measured at all",
                path.label()
            );

            let mut section = format!(
                "## `{name}` / {}{}\n\n{} blocks, {} domains, {} cells reached, \
                 **{} extra edge(s)** contaminating {} cell(s), {} foreign read(s).\n",
                path.label(),
                if ships { " -- **SHIPS TODAY**" } else { "" },
                non_air(&realisation.world),
                report.domains,
                report.reached_cells,
                report.extra_edges.len(),
                report.contaminated_cells,
                report.foreign_readers.len(),
            );
            if report.is_clean() {
                section.push_str("\nNo edge the netlist did not ask for.\n");
            } else {
                section.push_str("\n```\n");
                section.push_str(&report.describe());
                section.push_str("\n```\n");
            }
            sections.push(section);

            // Every cell some gate's output torch hangs on -- the gate's own
            // input node. Collected **per realisation**, not globally: two
            // circuits have two coordinate spaces, and a set shared across them
            // would count a `via` in one circuit as a support because a
            // different circuit happens to have a gate at the same address.
            let mut supports: HashMap<(i32, i32, i32), usize> = HashMap::new();
            for (gate, definition) in netlist.gates.iter().enumerate() {
                let Some(&(x, y, z)) = realisation
                    .ports
                    .gate_output_positions
                    .get(&definition.output)
                else {
                    continue;
                };
                let torch = Position::new(x, y, z);
                if let Some(support) =
                    torch_support_position(realisation.world.get(x, y, z), torch)
                {
                    supports.insert((support.x, support.y, support.z), gate);
                }
            }
            // A net "feeds" a gate when the netlist says one of that gate's own
            // declared inputs is driven by it -- which for a route means its
            // terminal sits in one of that gate's own sockets.
            let feeds = |wanted: &str, gate: usize| -> bool {
                realisation.nets.iter().any(|net| {
                    crate::compile::net_source_name(&netlist, net) == wanted
                        && net
                            .sinks
                            .iter()
                            .flatten()
                            .any(|&(sink_gate, _)| sink_gate == gate)
                })
            };
            for edge in &report.extra_edges {
                let Some(gate) = edge.via.and_then(|via| supports.get(&via).copied()) else {
                    continue;
                };
                through_a_gate_support += 1;
                if feeds(&edge.from_domain, gate) && feeds(&edge.to_net, gate) {
                    between_two_inputs_of_that_gate += 1;
                }
            }
            if ships {
                shipping_edges += report.extra_edges.len();
            }
            all_edges.extend(report.extra_edges.iter().cloned());
            all_reads.extend(report.foreign_readers.iter().cloned());
        }
    }

    let by_mechanism_three = all_edges
        .iter()
        .filter(|edge| edge.mechanism == Mechanism::StrongBlockToDust)
        .count();
    let reads_on_an_unowned_cell = all_reads
        .iter()
        .filter(|read| read.read_cell_net.is_none())
        .count();

    let _ = write!(
        out,
        "\n## Summary, computed from the runs below\n\n\
         * **{} extra edge(s) across all of it**, {shipping_edges} of them in a world \
         `compile` ships today.\n\
         * **{by_mechanism_three} of {} are mechanism 3** -- component, strongly powered \
         block, foreign dust. That is the mechanism both shipped bugs were, and here it \
         is the *only* one that occurs.\n\
         * **{through_a_gate_support} of {} cross through a cell that is some gate's own \
         support block**, and in **{between_two_inputs_of_that_gate}** of those the two nets \
         are two *declared inputs of that same gate*. So the shape is one thing, over and \
         over: a NOR's support is strongly powered by one input route's terminal, and \
         re-drives another input route's own terminal dust on a different face of the same \
         block -- and from there back up that route until a repeater stops it. The netlist \
         joins those two nets nowhere; they merely arrive at the same gate.\n\
         * **{reads_on_an_unowned_cell} of {} foreign reads land on a cell no route owns.** \
         A support block is owned by no route, so this counts the case \
         `TorchMergeFailure::ForeignNetReachesSupport` already refuses -- an independent \
         confirmation that `verify_torch_merge` is doing its half. Every other foreign \
         read is on a cell some *other net* owns, which means it is downstream of a \
         crossing listed above rather than a new coupling; what it adds is that the \
         contamination does not merely sit on a wire, it reaches a diode that forwards \
         it.\n\
         * `and4` and `verilog:and4` are clean on both paths. Every other circuit is not, \
         on every path that can build it.\n\n\
         **NOT MEASURED here: whether any of this changes what a circuit computes.** \
         Every one of these circuits passes its truth table today \
         (`tests/reference_circuits.rs`, `tests/seven_segment.rs`, \
         `tests/verilog_frontend.rs`). An extra edge is a fact about the realised graph; \
         whether a given one is load-bearing depends on where the contaminated run's next \
         repeater is and what branches off it before then, and that was not derived.\n",
        all_edges.len(),
        all_edges.len(),
        all_edges.len(),
        all_reads.len(),
    );

    out.push('\n');
    out.push_str(&sections.join("\n"));
    out
}

#[test]
fn the_realised_graph_of_every_circuit_is_what_is_recorded() {
    let fresh = sweep_report();
    let committed = std::fs::read_to_string(EXTRAS_ARTIFACT)
        .unwrap_or_else(|e| panic!("{EXTRAS_ARTIFACT} must exist: {e}"));
    if fresh != committed {
        let first = fresh
            .lines()
            .zip(committed.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("line {}:\n  fresh     `{a}`\n  committed `{b}`", i + 1))
            .unwrap_or_else(|| {
                format!(
                    "the files differ in length ({} fresh lines against {} committed)",
                    fresh.lines().count(),
                    committed.lines().count()
                )
            });
        panic!(
            "{EXTRAS_ARTIFACT} no longer describes this compiler -- {first}\n\n\
             If an edge appeared, that is a regression. If one disappeared, somebody closed it \
             and this record wants regenerating."
        );
    }
}

#[test]
#[ignore = "writes docs/derived/realised-graph-extras.md"]
fn regenerate_the_extras_record() {
    std::fs::write(EXTRAS_ARTIFACT, sweep_report()).expect("artifact must be writable");
}

/// The one thing the record above cannot say on its own: that `and4` and
/// `verilog:and4` are clean on **both** paths.
///
/// A byte comparison against a file makes every circuit's result equally loud,
/// which is right for a record and wrong for a headline. These two are the only
/// circuits this compiler builds today with no extra edge anywhere, so they are
/// the ones whose cleanliness is worth an assertion of its own.
#[test]
fn and4_realises_no_extra_edge_on_either_path() {
    for (name, netlist) in [
        ("and4".to_string(), build_and4_netlist().0),
        {
            let circuit = verilog::find("verilog:and4").expect("the catalog has verilog:and4");
            let (gate_level, _labels) = circuit.baked_netlist();
            (
                circuit.name.to_string(),
                lower(&gate_level).expect("verilog:and4 must lower"),
            )
        },
    ] {
        for path in [Path::Relaxation, Path::Legacy] {
            let realisation = realise(&netlist, path)
                .unwrap_or_else(|| panic!("{name} builds on both paths, including {:?}", path));
            let report = extra_edges(
                &realisation.world,
                &realisation.reservation,
                &netlist,
                &realisation.nets,
                &realisation.ports.gate_output_positions,
                &realisation.ports.input_positions,
            );
            assert!(
                report.is_clean(),
                "{name} / {} is supposed to be clean:\n{}",
                path.label(),
                report.describe()
            );
            assert!(
                report.reached_cells > 100,
                "{name} / {}: only {} cells were reached, so `clean` may mean `blind`",
                path.label(),
                report.reached_cells
            );
        }
    }
}

/// A guard on the guard: the sweep above is only worth its runtime if the
/// checker it runs actually rejects a world with an extra edge in it. This
/// builds one -- and4's own realised world with one stone block dropped under a
/// route so the route's dust lands directly over a lever -- and asserts the
/// checker names that cell, that mechanism and both nets, while
/// `verify_connectivity` on the very same world returns `Ok`.
#[test]
fn the_checker_rejects_a_world_a_route_was_dropped_onto_a_lever_in() {
    let (netlist, _output) = build_and4_netlist();
    let realisation = realise(&netlist, Path::Relaxation)
        .expect("and4 is one of the circuits relaxation can build");

    let clean = extra_edges(
        &realisation.world,
        &realisation.reservation,
        &netlist,
        &realisation.nets,
        &realisation.ports.gate_output_positions,
        &realisation.ports.input_positions,
    );
    assert!(
        clean.is_clean(),
        "the premise of this test is that and4 is clean before the injection:\n{}",
        clean.describe()
    );

    // Put a lever's own footprint back under a route: take the first primary
    // input's lever, write stone into the cell above it, and dust above that --
    // exactly the shape `lever_footprint`'s "the cell above" paragraph
    // describes, and exactly what shipped before that cell was claimed.
    let lever_name = netlist.inputs.first().expect("and4 has primary inputs");
    let &(lx, ly, lz) = realisation
        .ports
        .input_positions
        .get(lever_name)
        .expect("every declared input has a lever");
    let lever = Position::new(lx, ly, lz);
    let floor = lever.up();
    let stolen = floor.up();

    // The foreign dust has to belong to some *other* net for this to be an
    // extra edge rather than a fatter version of the same one, so it is given
    // to a net whose source is a different lever.
    let victim = realisation
        .nets
        .iter()
        .position(|net| !matches!(net.source, Source::Lever(input) if netlist.inputs[input] == *lever_name))
        .expect("and4 has more than one net");

    let mut world = realisation.world.clone();
    world.set(floor.x, floor.y, floor.z, rig_stone());
    world.set(stolen.x, stolen.y, stolen.z, rig_dust());
    let mut reservation = realisation.reservation.clone();
    reservation.insert(stolen, victim);

    let report = extra_edges(
        &world,
        &reservation,
        &netlist,
        &realisation.nets,
        &realisation.ports.gate_output_positions,
        &realisation.ports.input_positions,
    );
    let named = report.extra_edges.iter().find(|edge| {
        edge.to_cell == (stolen.x, stolen.y, stolen.z)
            && edge.mechanism == Mechanism::StrongBlockToDust
            && edge.via == Some((floor.x, floor.y, floor.z))
    });
    assert!(
        named.is_some(),
        "the checker must name the injected cell, its mediator and mechanism 3; it found:\n{}",
        report.describe()
    );

    // And the point of the whole exercise: the shipping connectivity invariant
    // is blind to the very same world.
    assert!(
        crate::compile::verify_connectivity(
            &world,
            &reservation,
            &netlist,
            &realisation.nets,
            &realisation.ports.gate_output_positions,
        )
        .is_ok(),
        "verify_connectivity walks dust to dust only, so it must still pass -- if this \
         ever fails, the gap closed and this test is the notice"
    );
}

// =====================================================================
// Part 4 -- the findings, confirmed by the simulator itself
// =====================================================================
//
// The extractor is structural, and part 1 proves it agrees with the simulator
// on a sweep of three-cell rigs. That is not the same as proving a finding
// inside a 16,000-block circuit is real, so the findings are put back through
// the simulator too, by the same differencing method the derivation used:
// drive the circuit into a state where the contaminated net is genuinely low,
// read its own wire, and then delete the one block the extractor names as the
// emitter and read it again.

/// Put every mechanism-3 crossing this circuit reports back through the
/// `Simulator`, and return the ones an input vector could expose.
///
/// A crossing is confirmed when some input vector leaves the contaminated net
/// **low** while its own wire reads non-zero at the named cell, and deleting
/// the named emitter -- one block, nothing else -- puts that cell back to zero
/// under the same vector. Both halves are needed: the first says the wire
/// carries a signal that is not its own, the second says which source put it
/// there.
fn confirm_with_the_simulator(
    label: &str,
    netlist: &Netlist,
    realisation: &Realisation,
    report: &Report,
    max_ticks: u64,
) -> Vec<String> {
    let levers: Vec<(i32, i32, i32)> = netlist
        .inputs
        .iter()
        .map(|name| realisation.ports.input_positions[name])
        .collect();
    let vectors = 1u32 << levers.len();

    // `net_source_name` names a net after its lever or its gate, so a net is
    // "low" exactly when its source component is off -- a lever's `lit`, or a
    // NOR's output torch's `lit`.
    let source_of = |name: &str| -> Option<(i32, i32, i32)> {
        realisation
            .ports
            .input_positions
            .get(name)
            .or_else(|| realisation.ports.gate_output_positions.get(name))
            .copied()
    };

    let settle_with = |world: &World, vector: u32| -> World {
        let mut simulator = Simulator::new(world.clone());
        simulator
            .run_until_stable(max_ticks)
            .unwrap_or_else(|e| panic!("{label} must settle before the first reading: {e:?}"));
        for (bit, &(x, y, z)) in levers.iter().enumerate() {
            let mut state = simulator.world().get(x, y, z).clone();
            state.lit = (vector >> bit) & 1 == 1;
            simulator.world_mut().set(x, y, z, state);
        }
        simulator
            .run_until_stable(max_ticks)
            .unwrap_or_else(|e| panic!("{label} must settle after an input change: {e:?}"));
        simulator.world().clone()
    };

    let settled: Vec<World> = (0..vectors)
        .map(|vector| settle_with(&realisation.world, vector))
        .collect();

    let mut confirmations = Vec::new();
    for edge in &report.extra_edges {
        if edge.mechanism != Mechanism::StrongBlockToDust {
            continue;
        }
        let (Some(victim_source), Some(emitter_source)) =
            (source_of(&edge.to_net), source_of(&edge.from_domain))
        else {
            continue;
        };
        let victim = Position::new(edge.to_cell.0, edge.to_cell.1, edge.to_cell.2);
        let emitter = Position::new(edge.from_cell.0, edge.from_cell.1, edge.from_cell.2);

        let mut control_world = realisation.world.clone();
        control_world.set(emitter.x, emitter.y, emitter.z, BlockState::air());

        for vector in 0..vectors {
            let world = &settled[vector as usize];
            let victim_high = world
                .get(victim_source.0, victim_source.1, victim_source.2)
                .lit;
            let emitter_high = world
                .get(emitter_source.0, emitter_source.1, emitter_source.2)
                .lit;
            let carried = world.get(victim.x, victim.y, victim.z).power;
            if victim_high || !emitter_high || carried == 0 {
                continue;
            }

            let control = settle_with(&control_world, vector);
            let control_carried = control.get(victim.x, victim.y, victim.z).power;
            let control_victim_high = control
                .get(victim_source.0, victim_source.1, victim_source.2)
                .lit;
            if control_carried != 0 || control_victim_high {
                // The control did not isolate the emitter, so this vector proves
                // nothing. Skipped rather than counted.
                continue;
            }
            confirmations.push(format!(
                "  {label}: vector {vector:0width$b} -- {victim_net} is low, {emitter_net} is \
                 high, and {victim_net}'s own wire at {to:?} reads {carried}; delete the emitter \
                 at {from:?} and the same cell reads {control_carried}",
                victim_net = edge.to_net,
                emitter_net = edge.from_domain,
                to = edge.to_cell,
                from = edge.from_cell,
                width = levers.len(),
            ));
            break;
        }
    }
    confirmations
}

/// **The result of this phase, measured rather than modelled.**
///
/// Every extra edge these circuits realise is the same shape: a gate's own
/// support block, strongly powered by one input route's terminal repeater,
/// re-driving a *different* input route's directed-dust terminal on another
/// face of the same block -- and from there back up that route's own run until
/// a repeater stops it. The netlist joins those two nets nowhere.
///
/// `full_adder` is confirmed on both paths and `segment_a` on the one it ships,
/// which covers a relaxation-placed circuit and an emitter-placed one. The two
/// decoders are extracted but **not** simulator-confirmed here; that is stated
/// in the report rather than implied, and the reason is runtime, not doubt.
#[test]
fn the_extra_edges_are_real_when_the_simulator_runs_the_circuit() {
    let cases: [(&str, Netlist, Path, usize); 3] = [
        (
            "full_adder / relaxation (SHIPS)",
            build_full_adder_netlist().0,
            Path::Relaxation,
            1,
        ),
        (
            "full_adder / legacy",
            build_full_adder_netlist().0,
            Path::Legacy,
            4,
        ),
        (
            "segment_a / legacy (SHIPS)",
            build_single_segment_netlist(0).0,
            Path::Legacy,
            9,
        ),
    ];

    let mut all = Vec::new();
    for (label, netlist, path, expected_edges) in cases {
        let realisation = realise(&netlist, path).unwrap_or_else(|| panic!("{label} must build"));
        let report = extra_edges(
            &realisation.world,
            &realisation.reservation,
            &netlist,
            &realisation.nets,
            &realisation.ports.gate_output_positions,
            &realisation.ports.input_positions,
        );
        assert_eq!(
            report.extra_edges.len(),
            expected_edges,
            "{label}: the recorded edge count moved -- see \
             no_circuit_realises_an_edge_the_netlist_did_not_ask_for for the whole record"
        );
        let confirmed = confirm_with_the_simulator(label, &netlist, &realisation, &report, 4000);
        assert!(
            !confirmed.is_empty(),
            "{label}: the extractor reports {} crossing(s) and the simulator confirms none of \
             them -- either they are false positives, or no input vector reaches the state that \
             would show them",
            report.extra_edges.len()
        );
        eprintln!(
            "{label}: {} of {} reported crossing(s) confirmed by the simulator",
            confirmed.len(),
            report.extra_edges.len()
        );
        all.extend(confirmed);
    }

    eprintln!(
        "extra edges confirmed against the simulator:\n{}",
        all.join("\n")
    );
}

// =====================================================================
// Part 5 -- the two shipped bugs, asked of the router as it stands
// =====================================================================

/// Both bugs that shipped were the same sentence: **a route stands on the cell
/// directly above a source component.** Above a lit lever
/// (`compile::lever_footprint`), and above a lit gate torch
/// (`compile::gate_footprint`). Both claim that cell today; this asserts the
/// claim from the outside, in the only vocabulary that can see it -- no extra
/// edge anywhere may be carried by a mediator whose own floor is a lever or a
/// torch.
///
/// It runs over every circuit on both paths, plus one pinned geometry: relaxation
/// does not put a route over a lever on its own, and the original measurement
/// (`lever_footprint`'s doc comment) reproduced it by pinning `full_adder`'s
/// `cin` at (37, 1, 126). That pin is kept here for exactly that reason.
///
/// **Measured both ways on 2026-08-17.**
///
/// * Delete `gate_footprint`'s `above` claim and this goes red on `and4 /
///   relaxation` with `g1 at (36, 1, 53) -> g2 at (36, 3, 53) across
///   (36, 2, 53)` -- (36,1,53) being `g1`'s own wall torch and (36,2,53) the
///   route's floor. That is the second shipped bug, at a new address.
/// * Delete `lever_footprint`'s `above` claim and this goes red on the pinned
///   case with `cin at (37, 1, 126) -> g5 at (37, 3, 126) across (37, 2, 126)`
///   -- the lever, its floor, and a foreign route one storey up. That is the
///   first shipped bug, at its original address. Restoring the claim removes
///   exactly that edge and leaves the other findings unchanged.
#[test]
fn no_extra_edge_is_carried_by_the_cell_above_a_lever_or_a_torch() {
    // The two halves are asserted separately, and the circuits first, because
    // the injections land in different halves: deleting `gate_footprint`'s claim
    // shows up in the ordinary circuits, and deleting `lever_footprint`'s only
    // shows up in the pinned case. Asserting once at the end would have let the
    // first injection fail on the *second* half's setup -- measured, it does:
    // with `gate_footprint`'s claim gone the pinned plan stops routing at all
    // and its `expect` fires before the finding is ever printed.
    let mut checked = 0usize;
    let mut offenders = Vec::new();
    for (name, netlist) in circuits() {
        for path in [Path::Relaxation, Path::Legacy] {
            let Some(realisation) = realise(&netlist, path) else {
                continue;
            };
            checked += 1;
            offenders.extend(mediators_standing_on_a_source(
                &format!("{name} / {}", path.label()),
                &netlist,
                &realisation,
            ));
        }
    }
    // The pinned geometry the lever bug was originally reproduced in.
    let (netlist, _outputs) = build_full_adder_netlist();
    let mut placements = PortPlacements::default();
    placements.pin(
        "cin",
        planner::Anchor {
            x: 37,
            y: 1,
            z: 126,
        },
    );
    let mut lost_coverage = Vec::new();
    match planner::plan_from_netlist_within(&netlist, &placements, planner::TRIAL_RIP_UP_ROUNDS)
        .and_then(|candidate| {
            let size = planner::candidate_world_size(&candidate);
            planner::verify_and_expose(&candidate, &netlist, size)
        }) {
        Ok(verified) => {
            let pinned = Realisation {
                world: verified.realised.world,
                reservation: verified.reservation,
                nets: verified.nets,
                ports: verified.realised.ports,
            };
            assert_eq!(
                pinned.ports.input_positions["cin"],
                (37, 1, 126),
                "the pin must actually have taken, or this case measures nothing"
            );
            checked += 1;
            offenders.extend(mediators_standing_on_a_source(
                "full_adder / relaxation, cin pinned",
                &netlist,
                &pinned,
            ));
        }
        Err(error) => lost_coverage.push(format!(
            "the pinned `cin` geometry -- the only case in this project where a route flies over \
             a lever -- no longer builds: {error}"
        )),
    }

    // **Findings first, coverage second, and nothing panics before either.**
    // The obvious structure -- `expect` the pinned build, assert the count, then
    // report -- reads better and hides the result: an injection that removes a
    // footprint claim also changes what routes, so a circuit stops building in
    // the same breath as another grows the offending edge. Measured both ways
    // on 2026-08-17: each of the two reverts drops exactly one realisation from
    // ten to nine, and with the count asserted first the panic said `only 9
    // realisations were inspected` instead of naming the edge.
    report_offenders(&offenders);
    // Nine circuit/path pairs build today -- six circuits times two paths, less
    // the three relaxation paths that cannot route their circuit -- plus the
    // pinned case, so ten.
    assert!(
        checked >= 10 && lost_coverage.is_empty(),
        "only {checked} realisations were inspected, which is fewer than there are{}",
        if lost_coverage.is_empty() {
            String::new()
        } else {
            format!(" -- and {}", lost_coverage.join("; "))
        }
    );
}

/// Every extra edge whose mediating block sits directly on top of a lever or a
/// torch -- the shape of both shipped bugs.
fn mediators_standing_on_a_source(
    label: &str,
    netlist: &Netlist,
    realisation: &Realisation,
) -> Vec<String> {
    let report = extra_edges(
        &realisation.world,
        &realisation.reservation,
        netlist,
        &realisation.nets,
        &realisation.ports.gate_output_positions,
        &realisation.ports.input_positions,
    );
    let mut offenders = Vec::new();
    for edge in &report.extra_edges {
        let Some((mx, my, mz)) = edge.via else {
            continue;
        };
        let below = realisation.world.get(mx, my - 1, mz);
        if matches!(
            below.kind,
            BlockKind::Lever | BlockKind::Torch | BlockKind::WallTorch
        ) {
            offenders.push(format!(
                "  {label}: {edge}\n    the mediator at ({mx}, {my}, {mz}) stands on a {:?} at \
                 ({mx}, {}, {mz})",
                below.kind,
                my - 1,
            ));
        }
    }
    offenders
}

fn report_offenders(offenders: &[String]) {
    assert!(
        offenders.is_empty(),
        "{} extra edge(s) are carried by the cell directly above a source component -- \
         this is the class that shipped twice:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
