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
//!
//! Everything here is plain data and pure functions over
//! `compile::{Netlist, CompiledCircuit}` and `Simulator`, so it is usable
//! equally from a test and from a front end such as the wasm viewer crate.

use std::collections::{BTreeMap, HashMap};

use crate::compile::{CompiledCircuit, Netlist};
use crate::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;
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
    TransitionResult {
        settle_game_ticks,
        nets,
    }
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
    Ok(observations_to_result(
        simulator.observations(),
        start_tick,
        settle_game_ticks,
    ))
}

/// Every net a compiled circuit exposes, ready to hand to
/// `Simulator::attach_observer`: every primary input's lever, and every
/// gate's actual output torch (which includes the netlist's declared
/// outputs -- a declared output's signal name is some gate's `output`).
pub fn watch_all_nets(compiled: &CompiledCircuit) -> Vec<(Position, String)> {
    let mut watched: Vec<(Position, String)> = compiled
        .input_positions
        .iter()
        .chain(&compiled.gate_output_positions)
        .map(|(name, &(x, y, z))| (Position::new(x, y, z), name.clone()))
        .collect();
    let mut labels = std::collections::BTreeSet::new();
    watched.retain(|(_, label)| labels.insert(label.clone()));
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
    let producer_of: HashMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(i, gate)| (gate.output.as_str(), i))
        .collect();

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
///
/// # A known miss, measured 2026-08-26 and deliberately not patched here
///
/// "Slowest-arriving input" is the wrong tie-break. What gates a signal is
/// the input whose arrival **plus its own edge's delay** is largest, and two
/// inputs that arrive on the same tick over routes of different length are
/// not equally responsible. `max_by_key` keeps the last of a tie, so it can
/// name the wrong predecessor.
///
/// Caught in the act on full_adder, all-pairs transition #40: `g19` and `g20`
/// both arrive at tick 34 and `g21` at 42. `g19 -> g21.in[0]` carries three
/// repeaters, `2 * (3 + 1) = 8`, and `34 + 8 = 42`; `g20 -> g21.in[1]`
/// carries none, `34 + 2 = 36`, which is not when `g21` moved. The gating
/// input is `g19`; this walk reports `g20`, and
/// [`summarize_worst_case`] then models 42 against a measured 46.
/// `seven_segment` shows the same class: model 96, measured 98.
///
/// It is invisible to every sweep in this tree because they all flip one
/// lever at a time, which is also why nothing here is currently red. The fix
/// is not a different tie-break -- **this function cannot see edge delays at
/// all.** It is handed a netlist and a map of arrival ticks, and how long a
/// hop takes is a fact about the *routed world*, which lives in the
/// `CompiledCircuit` its caller has and it does not. Correcting it means
/// threading that world in, which changes this signature and every caller;
/// inventing a tie-break without the delays would be a guess dressed as one.
///
/// `planner::critical_path_delay` does have the per-edge weights and is
/// exact on all six reference circuits, including these two. Where the two
/// disagree, that one is the one to believe.
pub fn critical_path(
    netlist: &Netlist,
    arrivals: &BTreeMap<String, u64>,
    output: &str,
) -> Vec<String> {
    let producer_of: HashMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(i, gate)| (gate.output.as_str(), i))
        .collect();

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

/// How many of `critical_path`'s own hops are driven by a **non-merge**
/// gate. `path[0]` is a primary input, never a gate's output, so only
/// `path[1..]` is checked.
fn critical_path_non_merge_gate_count(netlist: &Netlist, path: &[String]) -> usize {
    if path.len() < 2 {
        return 0;
    }
    let producer_of: HashMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(i, gate)| (gate.output.as_str(), i))
        .collect();
    path[1..]
        .iter()
        .filter(|signal| {
            let &gate_index = producer_of.get(signal.as_str()).unwrap_or_else(|| {
                panic!("critical path signal `{signal}` must be some gate's output")
            });
            !netlist.gates[gate_index].is_merge()
        })
        .count()
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
    /// Every index tied at the worst settle time, in input order.
    pub worst_transition_indices: Vec<usize>,
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
    /// **Non-merge** gates on the measured critical path: an OR on it
    /// costs no torch delay of its own.
    pub critical_path_gate_count: usize,
    /// output name -> number of transitions in which it glitched.
    pub glitch_counts: BTreeMap<String, usize>,
}

/// Reduce a sweep of already-measured transitions (typically one per input
/// vector's lever flip, exactly the sweep a truth-table test already runs)
/// into the worst-case numbers this project reports about a circuit.
///
/// `outputs` should be the netlist's declared output signal names.
pub fn summarize_worst_case(
    netlist: &Netlist,
    outputs: &[String],
    transitions: &[TransitionResult],
) -> TimingSummary {
    assert!(
        !transitions.is_empty(),
        "summarize_worst_case needs at least one transition"
    );
    assert!(
        !outputs.is_empty(),
        "summarize_worst_case needs at least one output"
    );

    let mut glitch_counts: BTreeMap<String, usize> = BTreeMap::new();
    let worst_ticks = transitions
        .iter()
        .map(|transition| transition.settle_game_ticks)
        .max()
        .unwrap_or(0);
    let worst_transition_indices = transitions
        .iter()
        .enumerate()
        .filter_map(|(index, transition)| {
            (transition.settle_game_ticks == worst_ticks).then_some(index)
        })
        .collect::<Vec<_>>();
    let worst_index = worst_transition_indices[0];
    for transition in transitions {
        for output in outputs {
            if transition.nets.get(output).is_some_and(NetTiming::glitched) {
                *glitch_counts.entry(output.clone()).or_insert(0) += 1;
            }
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

    let critical_path_gate_count = critical_path_non_merge_gate_count(netlist, &path);

    TimingSummary {
        worst_settle_game_ticks: worst_ticks,
        worst_transition_index: worst_index,
        worst_transition_indices,
        critical_output,
        critical_path: path,
        logic_depth: depth,
        logic_depth_bound_game_ticks: bound,
        ratio,
        critical_path_gate_count,
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
            kind: crate::compile::topology::GateKind::Nor(inputs.len()),
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
        assert_eq!(
            logic_depth_bound_game_ticks(&netlist),
            3 * TORCH_DELAY_GAME_TICKS
        );
    }

    #[test]
    fn logic_depth_of_an_empty_netlist_is_zero() {
        let netlist = Netlist {
            inputs: vec![],
            outputs: vec![],
            gates: vec![],
        };
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
        let arrivals: BTreeMap<String, u64> = [
            ("p".to_string(), 5),
            ("q".to_string(), 10),
            ("out".to_string(), 12),
        ]
        .into_iter()
        .collect();

        let path = critical_path(&netlist, &arrivals, "out");
        assert_eq!(
            path,
            vec!["y".to_string(), "q".to_string(), "out".to_string()]
        );
    }

    #[test]
    fn critical_path_of_a_primary_input_is_just_itself() {
        let netlist = Netlist {
            inputs: vec!["x".to_string()],
            outputs: vec![],
            gates: vec![],
        };
        let arrivals = BTreeMap::new();
        assert_eq!(
            critical_path(&netlist, &arrivals, "x"),
            vec!["x".to_string()]
        );
    }

    #[test]
    fn tick_conversions_use_the_documented_ratios() {
        assert_eq!(game_ticks_to_redstone_ticks(158), 79.0);
        assert_eq!(game_ticks_to_seconds(20), 1.0);
    }
}
