//! Dynamic timing analysis: per-net arrival times, glitch counts, the
//! critical path, and the netlist's logic-depth lower bound -- all measured
//! by simulating rather than modelled statically.
//!
//! See `docs/superpowers/specs/2026-08-07-timing-analysis.md` for the design
//! this module implements. In short:
//!
//! - **Arrival time**: for one input transition, the game tick at which each
//!   watched net last changes value. Built on
//!   `redstone::simulator::observer::Observer`, which the `Simulator` gains
//!   as an optional attachment -- watching a fixed set of positions keeps the
//!   cost proportional to the number of nets, not world volume.
//! - **Glitch count**: how many times a net changes during one settle. More
//!   than once means it took a wrong value before its final one.
//! - **Critical path**: the netlist walked backwards from the
//!   latest-arriving output, taking the latest-arriving input at each gate.
//! - **Logic-depth lower bound**: the netlist's longest gate chain, computed
//!   from the netlist alone, times one redstone tick per gate -- what the
//!   circuit would cost if wire were free. This is a property of the
//!   **netlist**, true of any placement of it whatsoever.
//! - **Critical-path settle model**: a reconstruction of settle time from the
//!   *actual measured* critical path's gate count and the repeaters its real,
//!   physically routed path contains, plus the output lamp's fixed display
//!   delay. Unlike the logic-depth bound, this is a property of **this
//!   layout** -- it uses whichever path was actually slowest, which is not
//!   guaranteed to be the netlist's deepest chain once per-hop routing cost
//!   stops being uniform across the circuit (see
//!   `critical_path_settle_model_game_ticks` below for why that assumption
//!   broke).
//!
//! Everything here is plain data and pure functions over
//! `compile::{Netlist, CompiledCircuit}` and `Simulator`, so it is usable
//! equally from a test and from a front end such as the wasm viewer crate.

use std::collections::{BTreeMap, HashMap};

use crate::compile::routing_stats;
use crate::compile::{CompiledCircuit, Netlist};
use crate::redstone::simulator::component::{
    LAMP_TURN_OFF_DELAY_GAME_TICKS, LAMP_TURN_ON_DELAY_GAME_TICKS, TORCH_DELAY_GAME_TICKS,
};
use crate::redstone::simulator::observer::Observation;
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::{SimulationError, Simulator};

// ---------------------------------------------------------------------
// Per-transition measurement
// ---------------------------------------------------------------------

/// One net's watched position and its recorded value changes during a single
/// transition, each as `(game tick relative to the start of the transition,
/// new value)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetTiming {
    label: String,
    position: Position,
    changes: Vec<(u64, bool)>,
}

impl NetTiming {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn position(&self) -> Position {
        self.position
    }

    /// Every recorded change, in order.
    pub fn changes(&self) -> &[(u64, bool)] {
        &self.changes
    }

    /// The tick of this net's last change during the transition, or `None`
    /// if it never changed (already at its final value throughout).
    pub fn arrival_tick(&self) -> Option<u64> {
        self.changes.last().map(|&(tick, _)| tick)
    }

    /// How many times this net changed value during the transition.
    pub fn change_count(&self) -> usize {
        self.changes.len()
    }

    /// Whether this net took a wrong value before its final one -- true
    /// whenever it changed more than once.
    pub fn glitched(&self) -> bool {
        self.changes.len() > 1
    }
}

/// The result of measuring one input transition: how long the circuit took
/// to settle, and every watched net's timing during that settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionResult {
    /// Game ticks the simulator took to reach stability after the input
    /// change, as returned by `Simulator::run_until_stable`.
    pub settle_game_ticks: u64,
    /// Every watched net that changed at least once during this transition,
    /// keyed by label. A net with no entry did not change.
    pub nets: BTreeMap<String, NetTiming>,
}

/// Turn an observer's log into a `TransitionResult`, expressing every tick
/// relative to `start_tick` (the absolute tick the transition began at).
pub fn observations_to_result(
    log: &[Observation],
    start_tick: u64,
    settle_game_ticks: u64,
) -> TransitionResult {
    let mut nets: BTreeMap<String, NetTiming> = BTreeMap::new();
    for observation in log {
        let relative_tick = observation.tick - start_tick;
        nets.entry(observation.label.clone())
            .or_insert_with(|| NetTiming {
                label: observation.label.clone(),
                position: observation.position,
                changes: Vec::new(),
            })
            .changes
            .push((relative_tick, observation.value));
    }
    TransitionResult { settle_game_ticks, nets }
}

/// Measure one input transition end to end: re-baseline the simulator's
/// attached observer, apply `apply_input`, run to stability, and report the
/// result.
///
/// `simulator` must already have an observer attached (see
/// `Simulator::attach_observer`) watching whatever positions the caller
/// cares about -- typically `watch_all_nets`'s output.
pub fn measure_transition(
    simulator: &mut Simulator,
    max_game_ticks: u64,
    apply_input: impl FnOnce(&mut Simulator),
) -> Result<TransitionResult, SimulationError> {
    simulator.reset_observer();
    let start_tick = simulator.current_tick();
    apply_input(simulator);
    let settle_game_ticks = simulator.run_until_stable(max_game_ticks)?;
    Ok(observations_to_result(simulator.observations(), start_tick, settle_game_ticks))
}

/// Every net a compiled circuit exposes, ready to hand to
/// `Simulator::attach_observer`: every primary input's lever, and every
/// gate's actual output torch (which includes the netlist's declared
/// outputs -- a declared output's signal name is some gate's `output`).
pub fn watch_all_nets(compiled: &CompiledCircuit) -> Vec<(Position, String)> {
    let mut watched = Vec::with_capacity(
        compiled.input_positions.len() + compiled.gate_output_positions.len(),
    );
    for (name, &(x, y, z)) in &compiled.input_positions {
        watched.push((Position::new(x, y, z), name.clone()));
    }
    for (name, &(x, y, z)) in &compiled.gate_output_positions {
        watched.push((Position::new(x, y, z), name.clone()));
    }
    watched
}

// ---------------------------------------------------------------------
// Static netlist analysis
// ---------------------------------------------------------------------

/// The netlist's longest gate chain, in number of gates -- the logic-depth
/// lower bound before converting to a tick count. Computed from the netlist
/// alone, with no simulation involved.
///
/// A gate fed only by primary inputs has depth 1; a gate fed by another
/// gate has depth `1 + that gate's depth`. The netlist's depth is the
/// maximum over all gates.
pub fn logic_depth(netlist: &Netlist) -> usize {
    let Some(order) = netlist.topological_order() else {
        return 0;
    };
    let producer_of: HashMap<&str, usize> =
        netlist.gates.iter().enumerate().map(|(i, gate)| (gate.output.as_str(), i)).collect();

    let mut depth = vec![0usize; netlist.gates.len()];
    for &g in &order {
        let mut deepest = 0usize;
        for input in &netlist.gates[g].inputs {
            deepest = match producer_of.get(input.as_str()) {
                Some(&producer) => deepest.max(depth[producer] + 1),
                None => deepest.max(1),
            };
        }
        depth[g] = deepest;
    }
    depth.into_iter().max().unwrap_or(0)
}

/// The logic-depth lower bound expressed in game ticks: what the circuit
/// would cost if wire were free, one redstone tick (`TORCH_DELAY_GAME_TICKS`)
/// per gate on the longest chain.
pub fn logic_depth_bound_game_ticks(netlist: &Netlist) -> u64 {
    logic_depth(netlist) as u64 * TORCH_DELAY_GAME_TICKS
}

/// Walk the netlist backwards from `output`, taking at each gate the
/// slowest-arriving input, to build the causal chain responsible for its
/// measured arrival time. Returns the path from a primary input (or the
/// output itself, if it is not gate-driven) through to `output`.
///
/// `arrivals` maps signal name to arrival tick; a signal missing from it is
/// treated as arriving at tick 0 (i.e. already stable before the transition
/// being analysed), which is always less than any real change.
pub fn critical_path(netlist: &Netlist, arrivals: &BTreeMap<String, u64>, output: &str) -> Vec<String> {
    let producer_of: HashMap<&str, usize> =
        netlist.gates.iter().enumerate().map(|(i, gate)| (gate.output.as_str(), i)).collect();

    let mut path = vec![output.to_string()];
    let mut current = output.to_string();
    while let Some(&gate_index) = producer_of.get(current.as_str()) {
        let gate = &netlist.gates[gate_index];
        let next = gate
            .inputs
            .iter()
            .max_by_key(|input| arrivals.get(input.as_str()).copied().unwrap_or(0))
            .expect("every gate in this netlist has at least one input")
            .clone();
        path.push(next.clone());
        current = next;
    }
    path.reverse();
    path
}

// ---------------------------------------------------------------------
// Critical-path settle model: reconstructs settle time from the actual
// measured critical path, not the netlist's static depth.
// ---------------------------------------------------------------------

/// Sum the repeater blocks physically routed along `critical_path`'s edges,
/// by looking each hop up in `compile::routing_stats::analyze` -- the same
/// per-edge breakdown `routing_cost_report` uses, read back from the
/// *already compiled* world rather than assumed from a uniform per-hop cost.
///
/// For each consecutive pair of signals in the path, the second is some
/// gate's output; the first must be one of that gate's inputs, which
/// identifies which socket (and so which routed edge) carries this hop. A
/// path shorter than two signals (a primary input with no gates behind it)
/// has no edges and so no repeaters.
///
/// Panics if `compiled` is not really the result of compiling `netlist`, or
/// if `critical_path` was not produced by walking `netlist` (both are
/// programming errors in the caller, not something a valid circuit can
/// trigger -- `compile::routing_stats`'s own tests already establish that
/// `analyze` succeeds on every reference circuit `compile` accepts).
pub fn critical_path_repeaters(netlist: &Netlist, compiled: &CompiledCircuit, critical_path: &[String]) -> usize {
    if critical_path.len() < 2 {
        return 0;
    }

    let producer_of: HashMap<&str, usize> =
        netlist.gates.iter().enumerate().map(|(i, gate)| (gate.output.as_str(), i)).collect();
    let report = routing_stats::analyze(netlist, compiled)
        .expect("a netlist that has already compiled must also analyze");

    critical_path
        .windows(2)
        .map(|hop| {
            let (source, sink_output) = (&hop[0], &hop[1]);
            let &gate_index = producer_of
                .get(sink_output.as_str())
                .unwrap_or_else(|| panic!("critical path signal `{sink_output}` must be some gate's output"));
            let gate = &netlist.gates[gate_index];
            let input_index = gate
                .inputs
                .iter()
                .position(|input| input == source)
                .unwrap_or_else(|| panic!("critical path signal `{source}` must feed `{sink_output}`"));
            let sink_label = format!("{sink_output}.in[{input_index}]");
            report
                .edges
                .iter()
                .find(|edge| &edge.source == source && edge.sink == sink_label)
                .unwrap_or_else(|| panic!("no routed edge found for critical-path hop {source} -> {sink_label}"))
                .total()
                .repeaters
        })
        .sum()
}

/// The corrected settle-time model: predicts settle time from the *actual
/// measured* critical path (`critical_path_gate_count` gates, from
/// `critical_path`; `critical_path_repeaters` repeaters, from the function
/// above) rather than the netlist's static logic-depth bound.
///
/// Each gate on the path costs one `TORCH_DELAY_GAME_TICKS`; each repeater
/// its real, physically routed edges contain costs the same (every repeater
/// this compiler places is set to the minimum one-redstone-tick delay, see
/// `compile::repeater`); the terminating lamp delay accounts for the output
/// lamp's own display delay, which every declared output pays once, after its
/// driving gate has already settled. That delay is not symmetric -- a lamp
/// lights immediately but goes dark one redstone tick later (see
/// `LAMP_TURN_ON_DELAY_GAME_TICKS` / `LAMP_TURN_OFF_DELAY_GAME_TICKS`'s doc
/// comments) -- so `lamp_turns_on` must say which direction the critical
/// path's own transition actually was; the caller (`summarize_worst_case`)
/// reads that off the worst transition's own recorded final value, not off
/// the netlist.
///
/// Before dust-staircase ramps (`compile`'s repeater ramps added a uniform,
/// fixed repeater tax to every hop), the deepest gate chain and the
/// most-repeater-laden chain were always the same path, so substituting the
/// netlist's static `logic_depth` for `critical_path_gate_count` produced the
/// same answer. Dust staircases add zero repeaters per hop, so a shorter
/// chain that happens to route through more repeaters (more channel crossings
/// or gate-entry corners) can now be slower than a longer chain that routes
/// cleanly -- this model must be fed the path that was *actually* slowest,
/// not the deepest one.
pub fn critical_path_settle_model_game_ticks(
    critical_path_gate_count: usize,
    critical_path_repeaters: usize,
    lamp_turns_on: bool,
) -> u64 {
    let lamp_delay = if lamp_turns_on { LAMP_TURN_ON_DELAY_GAME_TICKS } else { LAMP_TURN_OFF_DELAY_GAME_TICKS };
    TORCH_DELAY_GAME_TICKS * (critical_path_gate_count as u64 + critical_path_repeaters as u64) + lamp_delay
}

// ---------------------------------------------------------------------
// Worst case across a sweep
// ---------------------------------------------------------------------

/// Time conversions for reporting: 1 redstone tick = 2 game ticks, and
/// Minecraft runs 20 game ticks per real-time second.
pub fn game_ticks_to_redstone_ticks(game_ticks: u64) -> f64 {
    game_ticks as f64 / TORCH_DELAY_GAME_TICKS as f64
}

pub fn game_ticks_to_seconds(game_ticks: u64) -> f64 {
    game_ticks as f64 / 20.0
}

/// The worst case across a sweep of transitions: the slowest settle, the
/// logic-depth bound and the ratio to it, the critical path behind the
/// slowest-arriving output, and how many transitions produced a glitch on
/// each declared output.
#[derive(Debug, Clone, PartialEq)]
pub struct TimingSummary {
    pub worst_settle_game_ticks: u64,
    /// Index into the `transitions` slice `summarize_worst_case` was given.
    pub worst_transition_index: usize,
    /// Whichever declared output arrived latest in the worst transition.
    pub critical_output: String,
    pub critical_path: Vec<String>,
    /// The netlist's static longest gate chain -- a lower bound true of any
    /// placement of this netlist whatsoever, not just this one. Kept
    /// alongside the layout-specific numbers below because `ratio`
    /// (measured / this bound) is this project's headline self-metric: the
    /// fraction of settle time that is routing rather than computation.
    pub logic_depth: usize,
    pub logic_depth_bound_game_ticks: u64,
    /// `worst_settle_game_ticks / logic_depth_bound_game_ticks`.
    pub ratio: f64,
    /// Gates on the *actual measured* critical path (`critical_path.len() -
    /// 1`), which is not guaranteed to equal `logic_depth` -- see
    /// `critical_path_settle_model_game_ticks`.
    pub critical_path_gate_count: usize,
    /// Repeaters physically routed along the actual measured critical path's
    /// edges, read back from the compiled world.
    pub critical_path_repeater_count: usize,
    /// The corrected settle-time model's prediction **for this layout**:
    /// reconstructed from `critical_path_gate_count` and
    /// `critical_path_repeater_count`. Exact agreement with
    /// `worst_settle_game_ticks` is the exactness check this project runs on
    /// every reference circuit -- see `critical_path_settle_model_game_ticks`.
    pub critical_path_model_game_ticks: u64,
    /// output name -> number of transitions in which it glitched.
    pub glitch_counts: BTreeMap<String, usize>,
}

/// Reduce a sweep of already-measured transitions (typically one per input
/// vector's lever flip, exactly the sweep a truth-table test already runs)
/// into the worst-case numbers this project reports about a circuit.
///
/// `compiled` must be the result of compiling `netlist` -- it is needed to
/// count the actual measured critical path's repeaters
/// (`critical_path_repeaters`), which reads the physically routed world, not
/// just the netlist's graph shape.
///
/// `outputs` should be the netlist's declared output signal names.
pub fn summarize_worst_case(
    netlist: &Netlist,
    compiled: &CompiledCircuit,
    outputs: &[String],
    transitions: &[TransitionResult],
) -> TimingSummary {
    assert!(!transitions.is_empty(), "summarize_worst_case needs at least one transition");
    assert!(!outputs.is_empty(), "summarize_worst_case needs at least one output");

    let mut glitch_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut worst_index = 0usize;
    let mut worst_ticks = 0u64;
    for (index, transition) in transitions.iter().enumerate() {
        for output in outputs {
            if transition.nets.get(output).is_some_and(NetTiming::glitched) {
                *glitch_counts.entry(output.clone()).or_insert(0) += 1;
            }
        }
        if transition.settle_game_ticks >= worst_ticks {
            worst_ticks = transition.settle_game_ticks;
            worst_index = index;
        }
    }

    let worst = &transitions[worst_index];
    let arrivals: BTreeMap<String, u64> = worst
        .nets
        .iter()
        .filter_map(|(name, timing)| timing.arrival_tick().map(|tick| (name.clone(), tick)))
        .collect();
    let critical_output = outputs
        .iter()
        .max_by_key(|name| arrivals.get(name.as_str()).copied().unwrap_or(0))
        .cloned()
        .expect("outputs was checked non-empty above");
    let path = critical_path(netlist, &arrivals, &critical_output);

    let depth = logic_depth(netlist);
    let bound = depth as u64 * TORCH_DELAY_GAME_TICKS;
    let ratio = worst_ticks as f64 / bound.max(1) as f64;

    let critical_path_gate_count = path.len().saturating_sub(1);
    let critical_path_repeater_count = critical_path_repeaters(netlist, compiled, &path);
    // The critical output's own net (the driving gate's output torch, not the
    // physical lamp -- see `watch_all_nets`) carries the lamp's own final
    // value: the lamp sits directly under that torch's output pin with
    // nothing inverting in between (`compile::mod`'s output-wiring doc
    // comment), so torch.lit == lamp.lit. Its last recorded change is
    // therefore exactly whether this transition turned the lamp on or off.
    // No recorded change means the output never moved during the worst
    // transition (an edge case that should not arise for any transition that
    // is actually the slowest); default to the cheaper, immediate direction
    // since there is nothing to time in that case.
    let lamp_turns_on = worst
        .nets
        .get(&critical_output)
        .and_then(|timing| timing.changes().last())
        .map(|&(_, value)| value)
        .unwrap_or(true);
    let critical_path_model_game_ticks =
        critical_path_settle_model_game_ticks(critical_path_gate_count, critical_path_repeater_count, lamp_turns_on);

    TimingSummary {
        worst_settle_game_ticks: worst_ticks,
        worst_transition_index: worst_index,
        critical_output,
        critical_path: path,
        logic_depth: depth,
        logic_depth_bound_game_ticks: bound,
        ratio,
        critical_path_gate_count,
        critical_path_repeater_count,
        critical_path_model_game_ticks,
        glitch_counts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::Gate;

    fn nor(output: &str, inputs: &[&str]) -> Gate {
        Gate {
            name: output.to_string(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            output: output.to_string(),
        }
    }

    #[test]
    fn logic_depth_of_a_single_gate_fed_only_by_primary_inputs_is_one() {
        let netlist = Netlist {
            inputs: vec!["x".to_string(), "y".to_string()],
            outputs: vec!["g0".to_string()],
            gates: vec![nor("g0", &["x", "y"])],
        };
        assert_eq!(logic_depth(&netlist), 1);
    }

    #[test]
    fn logic_depth_counts_the_longest_chain_by_hand() {
        // g0 = NOR(x)         depth 1
        // g1 = NOR(g0)        depth 2
        // g2 = NOR(y)         depth 1
        // g3 = NOR(g1, g2)    depth 3  <- the longest chain: x -> g0 -> g1 -> g3
        let netlist = Netlist {
            inputs: vec!["x".to_string(), "y".to_string()],
            outputs: vec!["g3".to_string()],
            gates: vec![
                nor("g0", &["x"]),
                nor("g1", &["g0"]),
                nor("g2", &["y"]),
                nor("g3", &["g1", "g2"]),
            ],
        };
        assert_eq!(logic_depth(&netlist), 3);
        assert_eq!(logic_depth_bound_game_ticks(&netlist), 3 * TORCH_DELAY_GAME_TICKS);
    }

    #[test]
    fn logic_depth_of_an_empty_netlist_is_zero() {
        let netlist = Netlist { inputs: vec![], outputs: vec![], gates: vec![] };
        assert_eq!(logic_depth(&netlist), 0);
    }

    #[test]
    fn critical_path_follows_the_slower_input_at_each_gate() {
        // out = NOR(p, q); p = NOR(x); q = NOR(y).
        // q arrives after p, so the critical path must run through q, not p.
        let netlist = Netlist {
            inputs: vec!["x".to_string(), "y".to_string()],
            outputs: vec!["out".to_string()],
            gates: vec![nor("p", &["x"]), nor("q", &["y"]), nor("out", &["p", "q"])],
        };
        let arrivals: BTreeMap<String, u64> =
            [("p".to_string(), 5), ("q".to_string(), 10), ("out".to_string(), 12)]
                .into_iter()
                .collect();

        let path = critical_path(&netlist, &arrivals, "out");
        assert_eq!(path, vec!["y".to_string(), "q".to_string(), "out".to_string()]);
    }

    #[test]
    fn critical_path_of_a_primary_input_is_just_itself() {
        let netlist = Netlist { inputs: vec!["x".to_string()], outputs: vec![], gates: vec![] };
        let arrivals = BTreeMap::new();
        assert_eq!(critical_path(&netlist, &arrivals, "x"), vec!["x".to_string()]);
    }

    #[test]
    fn tick_conversions_use_the_documented_ratios() {
        assert_eq!(game_ticks_to_redstone_ticks(158), 79.0);
        assert_eq!(game_ticks_to_seconds(20), 1.0);
    }

    // -------------------------------------------------------------------
    // Regression: the slowest path is not always the deepest one.
    //
    // This is what broke the old model (see the module doc comment and
    // `critical_path_settle_model_game_ticks`'s doc): it substituted the
    // netlist's static logic depth for the actual critical path's gate
    // count, which is only correct while a fixed per-hop cost makes the
    // deepest chain and the most-repeater-laden chain the same path. Two
    // independent chains fed by the same lever, each ending in its own
    // lamp, break that coincidence on purpose:
    //
    //   - Chain A: 1 NOT gate, then 4 repeaters in series -- shallow but
    //     repeater-heavy.
    //   - Chain B: 3 NOT gates in series, then 1 repeater -- deeper, but far
    //     lighter on repeaters.
    //
    // Every component here reads its input via direct block adjacency (a
    // torch's own weak power, or a repeater's rear reading the block it
    // faces), exactly like `tests/timing.rs`'s hand-derived circuits -- so
    // every tick below is derived by hand, not read off a run:
    //
    //   Chain A: lever -(0gt)-> support_a0 -(0gt)-> torch_a@2 -> repeater_a1@4
    //            -> repeater_a2@6 -> repeater_a3@8 -> repeater_a4@10 -> lamp_a@14
    //   Chain B: lever -(0gt)-> support_b1 -(0gt)-> torch_b1@2 -> support_b2@2
    //            -> torch_b2@4 -> support_b3@4 -> torch_b3@6
    //            -> repeater_b1@8 -> lamp_b@12
    //
    // (dust/solid-block conduction is instant -- only torches, repeaters and
    // lamps are ever scheduled, at TORCH_DELAY_GAME_TICKS / repeater delay /
    // LAMP_TURN_OFF_DELAY_GAME_TICKS respectively -- both lamps here are
    // turning off, since both chains have an odd inversion count.)
    //
    // Chain A's own numbers (1 gate, 4 repeaters) predict 2*1 + 2*4 + 4 = 14.
    // Chain B's own numbers (3 gates, 1 repeater) predict 2*3 + 2*1 + 4 = 12.
    // Chain B is the "deepest chain" (more gates) -- the netlist-depth
    // analogue of the old model would have used chain B's numbers. But the
    // circuit's actual worst-case settle time is chain A's 14 ticks: the
    // slowest path is the shallow, repeater-heavy one, not the deep one.
    #[test]
    fn critical_path_settle_model_uses_the_slowest_path_not_the_deepest_one() {
        use crate::redstone::world::block::{BlockKind, BlockState, Facing};
        use crate::redstone::world::storage::World;

        fn named(name: &str, kind: BlockKind) -> BlockState {
            let mut state = BlockState::air();
            state.kind = kind;
            state.name = name.to_string();
            state
        }
        fn lever() -> BlockState {
            named("minecraft:lever", BlockKind::Lever)
        }
        fn stone() -> BlockState {
            named("minecraft:stone", BlockKind::Solid)
        }
        fn wall_torch(facing: Facing) -> BlockState {
            let mut state = named("minecraft:redstone_wall_torch", BlockKind::WallTorch);
            state.facing = Some(facing);
            state
        }
        fn repeater(facing: Facing) -> BlockState {
            let mut state = named("minecraft:repeater", BlockKind::Repeater);
            state.facing = Some(facing);
            state.delay = 1; // 1 redstone tick = 2 game ticks, same as `compile::repeater`
            state
        }
        fn lamp() -> BlockState {
            named("minecraft:redstone_lamp", BlockKind::Lamp)
        }

        let mut world = World::new(10, 3, 11);
        let lever_pos = Position::new(1, 1, 1);
        world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever());

        // Chain A: East of the lever -- 1 inversion, then 4 repeaters.
        let support_a0 = lever_pos.offset(Facing::East);
        let torch_a = support_a0.offset(Facing::East);
        world.set(support_a0.x, support_a0.y, support_a0.z, stone());
        world.set(torch_a.x, torch_a.y, torch_a.z, wall_torch(Facing::East));

        let mut previous = torch_a;
        for _ in 0..4 {
            let next = previous.offset(Facing::East);
            // facing=West -> input west, output east: continues the chain
            world.set(next.x, next.y, next.z, repeater(Facing::West));
            previous = next;
        }
        let lamp_a = previous.offset(Facing::East);
        world.set(lamp_a.x, lamp_a.y, lamp_a.z, lamp());

        // Chain B: South of the lever -- 3 inversions, then 1 repeater.
        let support_b1 = lever_pos.offset(Facing::South);
        let torch_b1 = support_b1.offset(Facing::South);
        world.set(support_b1.x, support_b1.y, support_b1.z, stone());
        world.set(torch_b1.x, torch_b1.y, torch_b1.z, wall_torch(Facing::South));

        let support_b2 = torch_b1.offset(Facing::South);
        let torch_b2 = support_b2.offset(Facing::South);
        world.set(support_b2.x, support_b2.y, support_b2.z, stone());
        world.set(torch_b2.x, torch_b2.y, torch_b2.z, wall_torch(Facing::South));

        let support_b3 = torch_b2.offset(Facing::South);
        let torch_b3 = support_b3.offset(Facing::South);
        world.set(support_b3.x, support_b3.y, support_b3.z, stone());
        world.set(torch_b3.x, torch_b3.y, torch_b3.z, wall_torch(Facing::South));

        let repeater_b1 = torch_b3.offset(Facing::South);
        // facing=North -> input north, output south: continues the chain
        world.set(repeater_b1.x, repeater_b1.y, repeater_b1.z, repeater(Facing::North));
        let lamp_b = repeater_b1.offset(Facing::South);
        world.set(lamp_b.x, lamp_b.y, lamp_b.z, lamp());

        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(200).expect("the lever-off steady state must settle");
        // Both chains have an odd number of inversions (1 and 3), so with the
        // lever off (unpowered support -> torch lit -> ... -> lamp lit) both
        // lamps rest lit, the same way a single NOT gate would.
        assert!(simulator.world().get(lamp_a.x, lamp_a.y, lamp_a.z).lit, "lamp_a must start lit (lever off)");
        assert!(simulator.world().get(lamp_b.x, lamp_b.y, lamp_b.z).lit, "lamp_b must start lit (lever off)");

        let mut on_lever = simulator.world().get(lever_pos.x, lever_pos.y, lever_pos.z).clone();
        on_lever.lit = true;
        simulator.world_mut().set(lever_pos.x, lever_pos.y, lever_pos.z, on_lever);
        let settle = simulator.run_until_stable(200).expect("this feedforward circuit must settle");

        assert!(!simulator.world().get(lamp_a.x, lamp_a.y, lamp_a.z).lit, "lamp_a must end up dark (lever on)");
        assert!(!simulator.world().get(lamp_b.x, lamp_b.y, lamp_b.z).lit, "lamp_b must end up dark (lever on)");
        assert_eq!(
            settle, 14,
            "see the hand-derived timeline above -- chain A (1 gate, 4 repeaters) is the true \
             critical path, not chain B (3 gates, 1 repeater)"
        );

        // Both lamps are turning off (lit -> dark) in this transition, so the
        // model must be fed `lamp_turns_on = false` to use
        // `LAMP_TURN_OFF_DELAY_GAME_TICKS` -- the same delay the hand-derived
        // timeline above already accounts for.
        //
        // The netlist-depth analogue -- the deepest chain's own numbers --
        // gets this circuit wrong. This is exactly the bug: static depth
        // assumed the deepest chain is always the slowest.
        let deepest_chain_prediction = critical_path_settle_model_game_ticks(3, 1, false);
        assert_ne!(
            deepest_chain_prediction, settle,
            "the deepest chain's own numbers must NOT predict this circuit's settle time"
        );

        // Fed the *actual* critical path's numbers instead -- chain A's 1
        // gate and 4 repeaters -- the model matches exactly.
        let actual_critical_path_prediction = critical_path_settle_model_game_ticks(1, 4, false);
        assert_eq!(actual_critical_path_prediction, settle);
    }
}
