//! The static strength walk and the running `Simulator`, put side by side
//! **cell by cell** on a realised world.
//!
//! # Why this exists
//!
//! [`super::verify_signal_strength`] is a *judge*: it refuses a plan whose net
//! never delivers a non-zero signal to one of its gate-input supports. It
//! reaches that verdict by walking the emitted blocks statically
//! ([`super::net_signal_strength`]) rather than by running them. The
//! `Simulator` is the oracle every circuit in this tree is judged against, and
//! it runs them. Those two answers had never been compared cell for cell, and
//! when they first were, they disagreed: negotiated `full_adder` is eight of
//! eight vectors right in the simulator and the walk refused it.
//!
//! One disagreement in one direction does not settle which of the two is
//! wrong, and it certainly does not license widening the walk. A judge that
//! **under**-reports refuses circuits that work; a judge that **over**-reports
//! passes circuits that do not, and this branch has already shipped two
//! circuits that passed every invariant and computed the wrong function. So
//! this module measures **both directions on every cell**, and the second
//! direction is the one that decided whether the walk was safe to widen.
//!
//! # What it found, and what changed because of it
//!
//! The walk was wrong, in the under-reporting direction, by one mechanism:
//! `docs/derived/coupling-mechanisms.md`'s mechanism 3 -- a component's output
//! strongly powering a conductive block, which then drives dust on all six of
//! its faces -- was written into `net_signal_strength` and **unreachable**,
//! because `deliver` decided whether to continue from a cell by asking
//! `own_cells`, and `own_cells` holds route anchors, which never hold a block.
//! `net_reach`, twenty lines above it in the same file, already modelled the
//! same physics correctly. The repair is that they now share one statement of
//! it (`structural_output_in_world`) and that `deliver` decides from what
//! stands at the target and which of `PowerOutput`'s two channels arrived.
//! See `planner::the_strength_verifier_follows_a_repeater_that_feeds_a_climb`.
//!
//! Widening a judge is the dangerous direction, so what holds it:
//!
//! * [`tests::the_judge_agrees_with_the_simulator_on_the_plan_it_used_to_refuse`]
//!   -- the plan that was refused now agrees with the simulator at every one of
//!   its 558 compared cells, in both directions, not merely at the sink;
//! * [`tests::genuine_decay_is_still_refused`] -- four shapes of real decay,
//!   injected into the realised world, and the judge still refuses every one
//!   that the simulator confirms went dark;
//! * [`tests::a_gate_input_its_own_net_cannot_reach_is_refused`] -- the
//!   negative control, which manufactures the unreachability a working circuit
//!   never has;
//! * and the numbers below, which are 0/0 on all six shipping circuits.
//!
//! # The comparison, stated exactly
//!
//! For one plan, one netlist, one realised world:
//!
//! * **What the walk claims.** [`walk_by_group`] reproduces
//!   `verify_signal_strength`'s own prologue -- `MergeGroups`, the group's
//!   united cells, the merge-body cells, one true origin per non-merge-sourced
//!   net -- and calls the same `net_signal_strength` the judge calls, on the
//!   same freshly emitted world. It is a *replica*, and drift is not assumed
//!   away: [`Measurement::replica_verdict`] is the refusal this replica derives
//!   and [`Measurement::shipping_verdict`] is what the real
//!   `verify_signal_strength` returns for the same plan, and
//!   `tests::the_replica_reaches_the_shipping_verdict` fails if they part.
//!
//! * **What the simulator says.** The walk assumes every origin driven at
//!   `MAX_SIGNAL_STRENGTH` regardless of the fresh world's placeholder
//!   activation. The simulator cannot be asked that directly -- a NOR's torch
//!   is lit or not according to its own inputs -- so the input space is swept:
//!   every combination of the circuit's levers, each from a fresh
//!   `Simulator::new` over the same emitted world, run to stable. Per group,
//!   the vector chosen is the one in which the most of that group's own
//!   origins are emitting. A group whose origins never emit anywhere in the
//!   sweep is **not measured** and says so ([`Measurement::unmeasured`]).
//!
//! * **Attribution, which is the whole difficulty.** "This cell carries power"
//!   is not "this group delivered to this cell" -- some other net may be
//!   driving it, and that would be a coupling finding rather than a strength
//!   one (`compile::coupling` owns that question). So every reading is
//!   differenced against a **control**: the same world with this group's own
//!   origin cells written as air, settled at the same input vector. This is
//!   `docs/derived/coupling-mechanisms.md`'s own method -- "a coupling is a
//!   reading that *changed*" -- applied per group instead of per rig. A cell is
//!   [`Reading::attributed`] when it carries power in the live world **and** a
//!   different amount in the control.
//!
//! # What a cell reading is, per kind
//!
//! [`observed`] never restates a propagation rule; it asks the simulator's own
//! taxonomy and propagation about the settled world:
//!
//! * anything that emits of its own accord -- lit dust, a lit repeater, a lit
//!   torch, a thrown lever -- reads its `taxonomy::power_emitted_by` strength;
//! * anything else that is a *powered block* -- which is what a gate's support
//!   is, and what `component::torch_should_be_lit` reads -- reads its
//!   `propagate::block_signal_at` strength, weak or strong alike, because a
//!   torch goes out on either.
//!
//! # Which cells are compared, and which are only reported
//!
//! Three classes, kept apart because they mean different things:
//!
//! * [`CellClass::Conductor`] -- the group's own routed dust and repeaters, and
//!   its merge-body cells. This is where "does the signal get there" lives.
//! * [`CellClass::Support`] -- the gate support blocks the invariant actually
//!   reads, and any other conductive block the walk recorded a value at. A
//!   refusal is issued at a sink support or nowhere.
//! * [`CellClass::Floor`] -- the stone under a conductor. **Diagnostic only,
//!   never counted as a disagreement.** Dust weakly powers the block beneath
//!   it, so every floor under live dust is "powered" in a sense the walk
//!   deliberately does not record; counting those would bury the finding in
//!   noise. They are reported with their strong/weak split, because the
//!   disputed mechanism -- a repeater refreshing into a climbing cell's floor
//!   -- happens exactly here.
//!
//! # This module only measures
//!
//! Nothing here is called by `compile`, and it is `#[cfg(test)]`. No production
//! behaviour changes because of anything in this file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::redstone::rules::taxonomy::{power_emitted_by, BlockPower, PowerOutput};
use crate::redstone::simulator::component::torch_support_position;
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::{block_signal_at, MAX_SIGNAL_STRENGTH};
use crate::redstone::simulator::Simulator;
use crate::redstone::world::block::{BlockKind, BlockState};
use crate::redstone::world::storage::World;

use super::{
    merge_gate_body_owners, net_name, net_signal_strength, CompileError, MergeGroups, Net, Netlist,
    Reservation, SignalSink, Source,
};

#[cfg(test)]
mod tests;

/// Game ticks any single settle is allowed before it is reported as a
/// divergence rather than waited on. The same ceiling
/// `planner::worst_settle_game_ticks` uses.
const MAX_TICKS: u64 = 2000;

/// `MAX_SIGNAL_STRENGTH`, re-exported so a report can say what "full" is.
pub const FULL: u8 = MAX_SIGNAL_STRENGTH;

/// Which of the three roles a compared cell plays, decided by **what actually
/// stands there in the realised world**, not by which list the cell came out of.
///
/// The distinction is load-bearing and was got wrong first: the legacy emitter's
/// `Reservation` claims cross-talk seal blocks as well as conductors, so
/// classifying "a reservation cell" as a conductor put 2,905 stone blocks into
/// `segment_a`'s comparison, every one of them weakly powered by the dust
/// standing on it and every one of them a cell the walk deliberately does not
/// record. See the module doc's third section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CellClass {
    /// Dust, a repeater or a comparator: something that carries a signal.
    Conductor,
    /// A gate support block the judge actually reads. **Decisive.**
    Support,
    /// Everything else -- stone, air, a floor. Reported, never a disagreement.
    Floor,
}

/// What role the block standing at `cell` plays.
fn classify(world: &World, cell: Position, is_sink: bool) -> CellClass {
    if is_sink {
        return CellClass::Support;
    }
    match world.get(cell.x, cell.y, cell.z).kind {
        BlockKind::RedstoneWire | BlockKind::Repeater | BlockKind::Comparator => {
            CellClass::Conductor
        }
        _ => CellClass::Floor,
    }
}

/// One cell, as both answers see it.
#[derive(Debug, Clone)]
pub struct Reading {
    pub cell: Position,
    pub class: CellClass,
    pub kind: BlockKind,
    /// Whether this cell is one of the supports the judge actually reads.
    pub is_sink: bool,
    /// `net_signal_strength`'s recorded value; 0 when it recorded nothing.
    pub walk: u8,
    /// [`observed`] in the settled live world at this group's chosen vector.
    pub live: u8,
    /// [`observed`] in the same world with this group's origins written air.
    pub control: u8,
    /// The **best any single origin of this group can do**: the maximum, over
    /// this group's origins, of [`observed`] in that origin's own isolation
    /// world (every other source in the circuit deleted, this one driven).
    /// This is the decisive reading -- see [`Reading::attributed`].
    pub solo: u8,
    /// Which origin produced [`Reading::solo`], when any did.
    pub solo_origin: Option<Position>,
    /// Whether the live reading is *strong* block power -- the only kind that
    /// re-drives dust (`coupling-mechanisms.md` mechanism 5 does not exist).
    pub live_strong: bool,
}

impl Reading {
    /// The simulator's answer to the walk's own question: **does this group,
    /// on its own, deliver power to this cell.**
    ///
    /// Read out of the isolation world, not out of the live one. The live/
    /// control difference was the first method tried and it is measurably
    /// unable to answer for a **shared** cell: a NOR's several inputs all land
    /// on one support block, so deleting one net's origin leaves the other net
    /// holding that block at the same strength and the difference vanishes.
    /// Measured on negotiated `full_adder`: `(40, 1, 132)` reads `live 10,
    /// control 10` for both `g0` and `g1`, which under the difference rule
    /// looks like two false passes and under isolation is neither.
    ///
    /// Isolation has no such blind spot: in each of these worlds exactly one
    /// component in the whole circuit is emitting, so any power at the cell came
    /// from that origin and nothing can mask it -- and taking the max over the
    /// group's origins is exactly the max-of-sources the walk itself computes by
    /// seeding every branch into one shared pass.
    ///
    /// **One origin at a time, not all of them at once.** Keeping a whole
    /// group's origins standing together was the second method tried and it is
    /// also wrong: `verilog:seven_segment`'s merge closure is a single group of
    /// eighteen nets whose gates feed one another, so with all fourteen origins
    /// left in place the group's own torches power each other's supports and
    /// most of them settle *out*. That reads as "the group delivers nothing"
    /// when what happened is that the group turned itself off.
    pub fn attributed(&self) -> bool {
        self.solo > 0
    }

    /// Whether the *live/control difference* -- the weaker, maskable test --
    /// also sees this group here. Kept because a cell where the two methods
    /// disagree is worth looking at.
    pub fn attributed_by_difference(&self) -> bool {
        self.live > 0 && self.live != self.control
    }

    /// The walk's answer.
    pub fn walk_reaches(&self) -> bool {
        self.walk > 0
    }
}

/// Which way a disagreement runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Direction {
    /// The walk says zero, the simulator delivers. A **false refusal**: the
    /// judge refuses a circuit that works.
    FalseRefusal,
    /// The walk says powered, the simulator does not deliver. A **false pass**:
    /// the judge would let a broken circuit through.
    FalsePass,
}

/// One group's whole comparison.
#[derive(Debug, Clone)]
pub struct GroupReport {
    pub root: usize,
    /// Every net in this declared-merge group, by name.
    pub nets: Vec<String>,
    /// The origins the walk seeds, and whether each was emitting at the chosen
    /// vector. A `false` here is a partial measurement: cells fed only by that
    /// branch are not attributable at this vector.
    pub origins: Vec<(Position, bool)>,
    /// The input combination the readings were taken at.
    pub vector: Vec<(String, bool)>,
    pub readings: Vec<Reading>,
}

impl GroupReport {
    /// Every cell the two answers disagree about, floors excluded.
    pub fn disagreements(&self) -> Vec<(Direction, &Reading)> {
        self.readings
            .iter()
            .filter(|reading| reading.class != CellClass::Floor)
            .filter_map(|reading| match (reading.walk_reaches(), reading.attributed()) {
                (false, true) => Some((Direction::FalseRefusal, reading)),
                (true, false) => Some((Direction::FalsePass, reading)),
                _ => None,
            })
            .collect()
    }

    /// The disagreements at a cell the judge **actually reads**. These are the
    /// only ones that can change a verdict; everything else is diagnostic.
    pub fn decisive(&self) -> Vec<(Direction, &Reading)> {
        self.disagreements()
            .into_iter()
            .filter(|(_, reading)| reading.is_sink)
            .collect()
    }

    pub fn every_origin_live(&self) -> bool {
        self.origins.iter().all(|&(_, live)| live)
    }
}

/// One circuit's whole differential.
#[derive(Debug, Clone)]
pub struct Measurement {
    pub groups: Vec<GroupReport>,
    /// Groups skipped, and why. Every one of these is a **NOT MEASURED**.
    pub unmeasured: Vec<String>,
    /// The refusal this replica walk derives; `None` means it passes.
    pub replica_verdict: Option<String>,
    /// What the real `verify_signal_strength` says about the same plan.
    pub shipping_verdict: Option<String>,
    /// How many settles the sweep and the controls cost.
    pub settles: usize,
}

impl Measurement {
    pub fn disagreements(&self) -> Vec<(&GroupReport, Direction, &Reading)> {
        let mut out = Vec::new();
        for group in &self.groups {
            for (direction, reading) in group.disagreements() {
                out.push((group, direction, reading));
            }
        }
        out
    }

    pub fn decisive(&self) -> Vec<(&GroupReport, Direction, &Reading)> {
        self.disagreements()
            .into_iter()
            .filter(|(_, _, reading)| reading.is_sink)
            .collect()
    }

    /// Cells where the walk recorded a value into **air** -- a dead end it
    /// documents recording and never reads. Counted apart so they cannot be
    /// mistaken for a hazard.
    pub fn recorded_into_air(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|group| group.readings.iter())
            .filter(|reading| reading.kind == BlockKind::Air && reading.walk > 0)
            .count()
    }

    pub fn cells_compared(&self) -> usize {
        self.groups
            .iter()
            .map(|group| {
                group
                    .readings
                    .iter()
                    .filter(|reading| reading.class != CellClass::Floor)
                    .count()
            })
            .sum()
    }

    pub fn floors_reported(&self) -> usize {
        self.groups
            .iter()
            .map(|group| {
                group
                    .readings
                    .iter()
                    .filter(|reading| reading.class == CellClass::Floor)
                    .count()
            })
            .sum()
    }
}

/// `super::net_name`, reachable from this module's own tests.
pub(crate) fn net_name_for(netlist: &Netlist, nets: &[Net], index: usize) -> String {
    net_name(netlist, nets, index)
}

/// What the settled simulator says this cell carries.
///
/// Never a restatement of a propagation rule: the first arm is
/// `taxonomy::power_emitted_by` (what this block, in the state the simulator
/// left it in, sends out) and the second is `propagate::block_signal_at` (what
/// a *reader* of this block would see). `strength.max(1)` guards only the
/// degenerate case of a kind reported powered at a nominal zero.
pub fn observed(world: &World, pos: Position) -> u8 {
    let state = world.get(pos.x, pos.y, pos.z);
    let emitted = power_emitted_by(state);
    if emitted != PowerOutput::INERT {
        return emitted.strength.max(1);
    }
    match block_signal_at(world, pos) {
        (BlockPower::None, _) => 0,
        (_, strength) => strength.max(1),
    }
}

/// Whether the settled world powers this cell *strongly* -- the only kind that
/// re-drives dust.
pub fn observed_strong(world: &World, pos: Position) -> bool {
    block_signal_at(world, pos).0 == BlockPower::Strong
}

/// One group's static walk, exactly as `verify_signal_strength` builds it.
pub struct GroupWalk {
    pub root: usize,
    pub nets: Vec<usize>,
    pub cells: HashSet<Position>,
    pub origins: Vec<Position>,
    pub strength: HashMap<Position, u8>,
}

/// Reproduce `verify_signal_strength`'s grouping and run the same walk.
///
/// Line for line the judge's own prologue. A replica rather than a refactor,
/// because this phase changes no production code;
/// `tests::the_replica_reaches_the_shipping_verdict` is what stops it drifting
/// away from the thing it replicates.
pub(crate) fn walk_by_group(
    world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Vec<GroupWalk> {
    let groups = MergeGroups::build(netlist, nets);

    let mut net_cells: Vec<HashSet<Position>> = vec![HashSet::new(); nets.len()];
    for (&pos, &owner) in reservation {
        net_cells[owner].insert(pos);
    }

    let mut group_cells: HashMap<usize, HashSet<Position>> = HashMap::new();
    for (n, cells) in net_cells.iter().enumerate() {
        group_cells
            .entry(groups.root(n))
            .or_default()
            .extend(cells.iter().copied());
    }
    for (&position, &root) in &merge_gate_body_owners(netlist, nets, gate_output_positions) {
        group_cells.entry(root).or_default().insert(position);
    }

    let mut group_sources: HashMap<usize, Vec<(Position, &BlockState)>> = HashMap::new();
    for (n, net) in nets.iter().enumerate() {
        if matches!(net.source, Source::Gate(g) if netlist.gates[g].is_merge()) {
            continue;
        }
        let (source, source_state) = match net.source {
            Source::Lever(i) => {
                let &(x, y, z) = input_positions
                    .get(&netlist.inputs[i])
                    .expect("emit records a lever position for every input");
                (Position::new(x, y, z), world.get(x, y, z))
            }
            Source::Gate(g) => {
                let &(x, y, z) = gate_output_positions
                    .get(&netlist.gates[g].output)
                    .expect("emit records a torch position for every gate");
                (Position::new(x, y, z), world.get(x, y, z))
            }
        };
        group_sources
            .entry(groups.root(n))
            .or_default()
            .push((source, source_state));
    }

    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for n in 0..nets.len() {
        members.entry(groups.root(n)).or_default().push(n);
    }

    let empty: Vec<(Position, &BlockState)> = Vec::new();
    let mut out = Vec::new();
    for (&root, cells) in &group_cells {
        let sources = group_sources.get(&root).unwrap_or(&empty);
        out.push(GroupWalk {
            root,
            nets: members.get(&root).cloned().unwrap_or_default(),
            cells: cells.clone(),
            origins: sources.iter().map(|&(pos, _)| pos).collect(),
            strength: net_signal_strength(world, cells, sources),
        });
    }
    out.sort_by_key(|walk| walk.root);
    out
}

/// The refusal this replica would issue, in `verify_signal_strength`'s own
/// words and its own order.
///
/// Only the gate-input arm. The judge's declared-output arm is, for a non-merge
/// gate, a purely structural single-hop test that never consults the walk, so a
/// differential against the simulator has nothing to say about it; a
/// merge-sourced output does consult the walk, and whether any circuit measured
/// here has one is recorded rather than assumed.
fn replica_refusal(
    world: &World,
    netlist: &Netlist,
    nets: &[Net],
    walks: &[GroupWalk],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Option<String> {
    let groups = MergeGroups::build(netlist, nets);
    let by_root: HashMap<usize, &GroupWalk> = walks.iter().map(|walk| (walk.root, walk)).collect();

    for (n, net) in nets.iter().enumerate() {
        let strength = &by_root[&groups.root(n)].strength;
        for &(gate, _input) in net.sinks.iter().flatten() {
            if netlist.gates[gate].is_merge() {
                continue;
            }
            let &(tx, ty, tz) = gate_output_positions
                .get(&netlist.gates[gate].output)
                .expect("emit records a torch position for every gate");
            let torch_pos = Position::new(tx, ty, tz);
            let Some(support) = torch_support_position(world.get(tx, ty, tz), torch_pos) else {
                continue;
            };
            if strength.get(&support).copied().unwrap_or(0) == 0 {
                return Some(
                    CompileError::SignalStrengthViolation {
                        net: net_name(netlist, nets, n),
                        sink: SignalSink::GateInput {
                            gate: netlist.gates[gate].name.clone(),
                            support: (support.x, support.y, support.z),
                        },
                    }
                    .to_string(),
                );
            }
        }
    }
    None
}

/// Every gate support block the judge reads, keyed by the group whose refusal
/// it would be.
fn sink_supports(
    world: &World,
    netlist: &Netlist,
    nets: &[Net],
    groups: &MergeGroups,
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> BTreeMap<usize, BTreeSet<Position>> {
    let mut out: BTreeMap<usize, BTreeSet<Position>> = BTreeMap::new();
    for (n, net) in nets.iter().enumerate() {
        for &(gate, _input) in net.sinks.iter().flatten() {
            if netlist.gates[gate].is_merge() {
                continue;
            }
            let &(tx, ty, tz) = gate_output_positions
                .get(&netlist.gates[gate].output)
                .expect("emit records a torch position for every gate");
            let torch_pos = Position::new(tx, ty, tz);
            if let Some(support) = torch_support_position(world.get(tx, ty, tz), torch_pos) {
                out.entry(groups.root(n)).or_default().insert(support);
            }
        }
    }
    out
}

/// A copy of `base` with every lever thrown to `bits`.
///
/// Refuses rather than guesses if a recorded input position does not hold a
/// lever: the whole sweep would then be measuring a different circuit.
fn with_levers(
    base: &World,
    levers: &[(String, (i32, i32, i32))],
    bits: &[bool],
) -> Result<World, String> {
    let mut world = base.clone();
    for ((name, (x, y, z)), &bit) in levers.iter().zip(bits.iter()) {
        let mut state = world.get(*x, *y, *z).clone();
        if state.kind != BlockKind::Lever {
            return Err(format!("input `{name}` is not a lever at ({x}, {y}, {z})"));
        }
        state.lit = bit;
        world.set(*x, *y, *z, state);
    }
    Ok(world)
}

/// Run `world` to a settled state and hand back what the simulator left.
fn settle(world: World, what: &str) -> Result<World, String> {
    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .map_err(|error| format!("{what} did not settle: {error:?}"))?;
    Ok(simulator.world().clone())
}

/// Every source component in the circuit: each primary input's lever, and each
/// **non-merge** gate's own output torch.
///
/// A merge gate's "output position" is plain dust (`place_merge_gate`), not a
/// component, so it is not a source and deleting it would delete the junction
/// itself.
pub(crate) fn every_source(
    netlist: &Netlist,
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> BTreeSet<Position> {
    let mut out = BTreeSet::new();
    for name in &netlist.inputs {
        if let Some(&(x, y, z)) = input_positions.get(name) {
            out.insert(Position::new(x, y, z));
        }
    }
    for gate in &netlist.gates {
        if gate.is_merge() {
            continue;
        }
        if let Some(&(x, y, z)) = gate_output_positions.get(&gate.output) {
            out.insert(Position::new(x, y, z));
        }
    }
    out
}

/// The **isolation** world for one origin: every other source component in the
/// circuit deleted, and this one driven.
///
/// This is what makes an attribution decisive rather than differential. Exactly
/// one thing in the whole world emits, so any power anywhere came from it, and
/// nothing can mask it -- which the live/control difference measurably cannot
/// say for a support block two nets both land on, and which a whole-group
/// isolation measurably cannot say for a merge closure whose gates feed each
/// other.
///
/// Only *emitters* are removed. Every conductor, floor, support and route cell
/// stays exactly where it was, so the geometry this origin propagates through is
/// the geometry it propagates through in the shipping world.
///
/// A lever origin is thrown on. A gate-torch origin needs nothing: its own
/// support is now fed by nothing at all, so `component::torch_should_be_lit`
/// settles it lit on its own -- and `OriginIsolation::emits` records whether it
/// really did rather than assuming it.
pub(crate) fn isolation_world(base: &World, all_sources: &BTreeSet<Position>, keep: Position) -> World {
    let mut world = base.clone();
    for &source in all_sources {
        if source == keep {
            continue;
        }
        world.set(source.x, source.y, source.z, BlockState::air());
    }
    let mut state = world.get(keep.x, keep.y, keep.z).clone();
    if state.kind == BlockKind::Lever {
        state.lit = true;
        world.set(keep.x, keep.y, keep.z, state);
    }
    world
}

/// One origin's isolation world, settled, and whether the origin actually ended
/// up emitting in it.
struct OriginIsolation {
    origin: Position,
    emits: bool,
    world: World,
}

/// The strongest reading any one isolated, actually-emitting origin produces at
/// `cell`.
fn best_solo(isolations: &[OriginIsolation], cell: Position) -> Option<(Position, u8)> {
    isolations
        .iter()
        .filter(|isolation| isolation.emits)
        .map(|isolation| (isolation.origin, observed(&isolation.world, cell)))
        .filter(|&(_, value)| value > 0)
        .max_by_key(|&(_, value)| value)
}

/// The whole differential for one plan.
#[allow(clippy::too_many_arguments)]
pub(crate) fn measure(
    realised_world: &World,
    reservation: &Reservation,
    netlist: &Netlist,
    nets: &[Net],
    gate_output_positions: &BTreeMap<String, (i32, i32, i32)>,
    input_positions: &BTreeMap<String, (i32, i32, i32)>,
    output_positions: &BTreeMap<String, (i32, i32, i32)>,
) -> Result<Measurement, String> {
    let walks = walk_by_group(
        realised_world,
        reservation,
        netlist,
        nets,
        gate_output_positions,
        input_positions,
    );
    let groups = MergeGroups::build(netlist, nets);
    let supports = sink_supports(realised_world, netlist, nets, &groups, gate_output_positions);

    let replica_verdict =
        replica_refusal(realised_world, netlist, nets, &walks, gate_output_positions);
    let shipping_verdict = super::verify_signal_strength(
        realised_world,
        reservation,
        netlist,
        nets,
        gate_output_positions,
        input_positions,
        output_positions,
    )
    .err()
    .map(|error| error.to_string());

    let levers: Vec<(String, (i32, i32, i32))> = netlist
        .inputs
        .iter()
        .map(|name| {
            let &(x, y, z) = input_positions
                .get(name)
                .expect("emit records a lever position for every input");
            (name.clone(), (x, y, z))
        })
        .collect();
    if levers.len() > 12 {
        return Err(format!("{} inputs is too many to sweep", levers.len()));
    }

    // The sweep, shared by every group: one settled world per input vector.
    let mut sweep: Vec<(Vec<bool>, World)> = Vec::new();
    for combination in 0..(1usize << levers.len()) {
        let bits: Vec<bool> = (0..levers.len())
            .map(|index| (combination >> (levers.len() - 1 - index)) & 1 == 1)
            .collect();
        let world = with_levers(realised_world, &levers, &bits)?;
        let world = settle(world, &format!("the live world at {bits:?}"))?;
        sweep.push((bits, world));
    }
    let mut settles = sweep.len();
    let all_sources = every_source(netlist, gate_output_positions, input_positions);

    let mut reports = Vec::new();
    let mut unmeasured = Vec::new();
    for walk in &walks {
        let names: Vec<String> = walk
            .nets
            .iter()
            .map(|&n| net_name(netlist, nets, n))
            .collect();

        if walk.origins.is_empty() {
            unmeasured.push(format!(
                "group {} ({}) seeds no origin at all -- merge-sourced only",
                walk.root,
                names.join(", ")
            ));
            continue;
        }

        // The vector in which the most of this group's origins are emitting.
        let mut best: Option<(usize, usize)> = None;
        for (index, (_, world)) in sweep.iter().enumerate() {
            let live = walk
                .origins
                .iter()
                .filter(|&&origin| observed(world, origin) > 0)
                .count();
            if best.map(|(count, _)| live > count).unwrap_or(true) {
                best = Some((live, index));
            }
        }
        let (live_origins, index) = best.expect("the sweep is never empty");
        if live_origins == 0 {
            unmeasured.push(format!(
                "group {} ({}) has no input vector in which any of its {} origin(s) emit",
                walk.root,
                names.join(", "),
                walk.origins.len()
            ));
            continue;
        }
        let (bits, live_world) = &sweep[index];

        // The control: the same vector with this group's own origins deleted.
        // The levers are thrown **first** and the origins deleted **after**, so
        // a group whose own origin *is* a lever still has that lever removed
        // rather than re-thrown -- which is the point of the control.
        let mut control_base = with_levers(realised_world, &levers, bits)?;
        for &origin in &walk.origins {
            control_base.set(origin.x, origin.y, origin.z, BlockState::air());
        }
        let control_world = settle(
            control_base,
            &format!("the control for group {} at {bits:?}", walk.root),
        )?;
        settles += 1;

        // One isolation world per origin: in each, exactly one component in the
        // whole circuit emits.
        let mut isolations: Vec<OriginIsolation> = Vec::new();
        for &origin in &walk.origins {
            let world = settle(
                isolation_world(realised_world, &all_sources, origin),
                &format!("the isolation world for origin {origin:?}"),
            )?;
            settles += 1;
            let emits = observed(&world, origin) > 0;
            if !emits {
                unmeasured.push(format!(
                    "group {} origin ({}, {}, {}) does not emit even as the only component left in the world -- nothing it feeds is attributable",
                    walk.root, origin.x, origin.y, origin.z
                ));
            }
            isolations.push(OriginIsolation {
                origin,
                emits,
                world,
            });
        }

        let sinks_here: BTreeSet<Position> = supports.get(&walk.root).cloned().unwrap_or_default();
        let mut readings: Vec<Reading> = Vec::new();
        let mut seen: BTreeSet<Position> = BTreeSet::new();
        let push = |readings: &mut Vec<Reading>, seen: &mut BTreeSet<Position>, cell: Position| {
            if !seen.insert(cell) {
                return;
            }
            let is_sink = sinks_here.contains(&cell);
            readings.push(Reading {
                cell,
                class: classify(realised_world, cell, is_sink),
                kind: live_world.get(cell.x, cell.y, cell.z).kind,
                is_sink,
                walk: walk.strength.get(&cell).copied().unwrap_or(0),
                live: observed(live_world, cell),
                control: observed(&control_world, cell),
                solo: best_solo(&isolations, cell).map(|(_, value)| value).unwrap_or(0),
                solo_origin: best_solo(&isolations, cell).map(|(origin, _)| origin),
                live_strong: observed_strong(live_world, cell),
            });
        };

        for &support in &sinks_here {
            push(&mut readings, &mut seen, support);
        }
        let mut own: Vec<Position> = walk.cells.iter().copied().collect();
        own.sort_by_key(|cell| (cell.x, cell.y, cell.z));
        for cell in &own {
            push(&mut readings, &mut seen, *cell);
        }
        // Every other cell the walk recorded a value at, whatever it is: this is
        // where a false pass outside the route's own cells would show.
        let mut recorded: Vec<Position> = walk.strength.keys().copied().collect();
        recorded.sort_by_key(|cell| (cell.x, cell.y, cell.z));
        for cell in &recorded {
            push(&mut readings, &mut seen, *cell);
        }
        for cell in &own {
            push(
                &mut readings,
                &mut seen,
                Position::new(cell.x, cell.y - 1, cell.z),
            );
        }

        reports.push(GroupReport {
            root: walk.root,
            nets: names,
            origins: walk
                .origins
                .iter()
                .map(|&origin| (origin, observed(live_world, origin) > 0))
                .collect(),
            vector: levers
                .iter()
                .map(|(name, _)| name.clone())
                .zip(bits.iter().copied())
                .collect(),
            readings,
        });
    }

    Ok(Measurement {
        groups: reports,
        unmeasured,
        replica_verdict,
        shipping_verdict,
        settles,
    })
}
