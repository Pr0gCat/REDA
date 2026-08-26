//! MEASUREMENT HARNESS, with one assertion in it.
//!
//! Prints, for every reference circuit and on whichever path `compile()`
//! ships, the two delay numbers side by side and hop by hop:
//!
//!  * what `planner::critical_path_delay` believes -- the path its own
//!    longest-path walk picks over `RouteTerminal::repeaters`, re-derived
//!    here from the candidate's *public* routes and cross-checked against
//!    `PlanCandidate::cost().delay` so the re-derivation cannot drift;
//!  * what the circuit actually does -- `timing::summarize_worst_case` on a
//!    full input sweep through the real simulator, and the repeaters
//!    `routing_stats::analyze` finds physically on that path, decomposed
//!    into Column / Ramp / Track / GateEntry / Bypass.
//!
//! This is where the 2026-08-26 repair to `RouteTerminal::repeaters` was
//! measured, and `terminal_repeaters_are_every_repeater_on_the_path` is the
//! assertion it left behind: what the emitter counted as it wrote has to
//! equal what `routing_stats` reads back out of the finished blocks, on every
//! routed edge of every reference circuit. Neither side can be derived from
//! the other, which is what makes it worth running.
//!
//! Two sweeps are used, and they give different answers: SINGLE-LEVER (one
//! lever per transition -- what every other test in this tree runs) and
//! ALL-PAIRS (every ordered pair of complete input vectors, applied at once).
//! A static longest path is an upper bound over all input behaviour, and
//! three of the six circuits have a slowest path no single lever flip
//! sensitises, so only the all-pairs sweep can tell a mispriced path from an
//! unexercised one.
//!
//! Run with:
//!   cargo test --release --test delay_model_reconciliation -- --nocapture --test-threads=1

use std::collections::{BTreeMap, BTreeSet, HashMap};

use reda::circuits::and4::{build_and4_netlist, INPUT_NAMES as AND4_INPUTS};
use reda::circuits::full_adder::{build_full_adder_netlist, INPUT_NAMES as ADDER_INPUTS};
use reda::circuits::seven_segment::{
    build_seven_segment_netlist, build_single_segment_netlist, INPUT_NAMES as DECODER_INPUTS,
};
use reda::circuits::verilog;
use reda::compile::lowering::{lower, lower_optimised};
use reda::compile::planner::{seed_from_legacy, NodeRealisation, PlanCandidate};
use reda::compile::routing_stats::{self, PartTotals, RoutePart, ALL_PARTS};
use reda::compile::{compile, compile_legacy, CompiledCircuit, Netlist, PlannerKind};
use reda::redstone::simulator::component::TORCH_DELAY_GAME_TICKS;
use reda::redstone::simulator::Simulator;
use reda::timing::{
    observations_to_result, summarize_worst_case, watch_all_nets, TransitionResult,
};

const MAX_TICKS: u64 = 4000;

// ---------------------------------------------------------------------
// A re-derivation of planner::critical_path_delay from public data.
//
// Same shape as the private original: one weight per (owner -> sink) edge,
// weight = terminal.repeaters + (sink is not a wire merge), longest path
// over the resulting DAG, times TORCH_DELAY_GAME_TICKS. Every caller below
// asserts the total it produces equals `candidate.cost().delay`, so if the
// original ever changes this harness fails rather than lying.
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PlannerEdge {
    sink: String,
    input_index: usize,
    repeaters: u64,
    gate_cost: u64,
}

fn planner_edges(candidate: &PlanCandidate) -> BTreeMap<String, Vec<PlannerEdge>> {
    let mut is_merge: BTreeMap<&str, bool> = BTreeMap::new();
    for node in candidate.primitive_nodes() {
        if let Some(name) = node.id.strip_prefix("gate:") {
            is_merge.insert(name, node.realisation == NodeRealisation::WireMerge);
        }
    }

    let mut edges: BTreeMap<String, Vec<PlannerEdge>> = BTreeMap::new();
    for route in candidate.routes() {
        let Some(owner) = route.owner() else { continue };
        for terminal in route.terminals() {
            let sink = terminal.sink.gate.as_str();
            let gate_cost = u64::from(!is_merge.get(sink).copied().unwrap_or(false));
            edges.entry(owner.to_string()).or_default().push(PlannerEdge {
                sink: sink.to_string(),
                input_index: terminal.sink.input_index,
                repeaters: terminal.repeaters,
                gate_cost,
            });
        }
    }
    edges
}

/// Longest path from `signal`, memoised, returning (weight, chosen next edge).
fn longest(
    signal: &str,
    edges: &BTreeMap<String, Vec<PlannerEdge>>,
    memo: &mut BTreeMap<String, (u64, Option<usize>)>,
    visiting: &mut BTreeSet<String>,
) -> u64 {
    if let Some(&(known, _)) = memo.get(signal) {
        return known;
    }
    if !visiting.insert(signal.to_string()) {
        return 0;
    }
    let mut best = 0u64;
    let mut best_index = None;
    if let Some(outgoing) = edges.get(signal) {
        for (index, edge) in outgoing.iter().enumerate() {
            let weight = edge.repeaters + edge.gate_cost + longest(&edge.sink, edges, memo, visiting);
            // `Iterator::max` keeps the LAST maximum; mirror that with `>=`.
            if weight >= best || best_index.is_none() {
                best = weight;
                best_index = Some(index);
            }
        }
    }
    visiting.remove(signal);
    memo.insert(signal.to_string(), (best, best_index));
    best
}

/// The planner's own critical path: the argmax start, then the argmax edge at
/// each step. Returns (total weight in units, the hops taken).
fn planner_critical_path(candidate: &PlanCandidate) -> (u64, Vec<PlannerEdge>, String) {
    walk(planner_edges(candidate))
}

fn walk(edges: BTreeMap<String, Vec<PlannerEdge>>) -> (u64, Vec<PlannerEdge>, String) {
    let mut memo = BTreeMap::new();
    let mut visiting = BTreeSet::new();

    let mut worst = 0u64;
    let mut start = String::new();
    for signal in edges.keys() {
        let weight = longest(signal, &edges, &mut memo, &mut visiting);
        if weight >= worst {
            worst = weight;
            start = signal.clone();
        }
    }

    let mut hops = Vec::new();
    let mut current = start.clone();
    while let Some(&(_, Some(index))) = memo.get(&current) {
        let edge = edges[&current][index].clone();
        current = edge.sink.clone();
        hops.push(edge);
    }
    (worst, hops, start)
}

// ---------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------

fn sweep(compiled: &CompiledCircuit, input_names: &[&str]) -> Vec<TransitionResult> {
    let watched = watch_all_nets(compiled);
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(MAX_TICKS).expect("settles before the first reading");
    simulator.attach_observer(watched);

    let levers: HashMap<&str, (i32, i32, i32)> = input_names
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).expect("declared input")))
        .collect();

    let mut transitions = Vec::new();
    let bits = input_names.len();
    for combination in 0u32..(1u32 << bits) {
        for (index, &name) in input_names.iter().enumerate() {
            let on = (combination >> (bits - 1 - index)) & 1 == 1;
            let at = levers[name];
            simulator.reset_observer();
            let start_tick = simulator.current_tick();
            let mut state = simulator.world().get(at.0, at.1, at.2).clone();
            state.lit = on;
            simulator.world_mut().set(at.0, at.1, at.2, state);
            simulator.run_until_stable(MAX_TICKS).expect("settles after a lever move");
            let settle = simulator.current_tick() - start_tick;
            transitions.push(observations_to_result(simulator.observations(), start_tick, settle));
        }
    }
    transitions
}

/// Every *ordered pair* of input vectors, applied as one simultaneous change
/// rather than one lever at a time. The one-lever-at-a-time sweep every
/// existing test runs cannot sensitise a path whose gates need two inputs to
/// move together, so a static longest path that never shows up there is not
/// yet a false path -- this is what tells the two apart.
fn sweep_all_pairs(compiled: &CompiledCircuit, input_names: &[&str]) -> Vec<TransitionResult> {
    let watched = watch_all_nets(compiled);
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(MAX_TICKS).expect("settles before the first reading");
    simulator.attach_observer(watched);

    let levers: Vec<(i32, i32, i32)> = input_names
        .iter()
        .map(|&name| *compiled.input_positions.get(name).expect("declared input"))
        .collect();
    let bits = input_names.len();
    let apply = |simulator: &mut Simulator, vector: u32| {
        for (index, &at) in levers.iter().enumerate() {
            let on = (vector >> (bits - 1 - index)) & 1 == 1;
            let mut state = simulator.world().get(at.0, at.1, at.2).clone();
            state.lit = on;
            simulator.world_mut().set(at.0, at.1, at.2, state);
        }
    };

    let mut transitions = Vec::new();
    for from in 0u32..(1u32 << bits) {
        for to in 0u32..(1u32 << bits) {
            if from == to {
                continue;
            }
            apply(&mut simulator, from);
            simulator.run_until_stable(MAX_TICKS).expect("settles");
            simulator.reset_observer();
            let start_tick = simulator.current_tick();
            apply(&mut simulator, to);
            simulator.run_until_stable(MAX_TICKS).expect("settles");
            let settle = simulator.current_tick() - start_tick;
            transitions.push(observations_to_result(simulator.observations(), start_tick, settle));
        }
    }
    transitions
}

fn false_path_check(label: &str, netlist: &Netlist, input_names: &[&str]) {
    eprintln!("\n################ {label}: all-pairs sweep ################");
    let legacy = compile_legacy(netlist).expect("legacy compiles");
    let transitions = sweep_all_pairs(&legacy, input_names);
    let outputs: Vec<String> = netlist.outputs.clone();
    let summary = summarize_worst_case(netlist, &legacy, &outputs, &transitions);
    eprintln!(
        "{label}: all-pairs worst settle = {} game ticks, model {:?} ({} gates + {:?} repeaters)",
        summary.worst_settle_game_ticks,
        summary.critical_path_model_game_ticks,
        summary.critical_path_gate_count,
        summary.critical_path_repeater_count,
    );
    eprintln!("{label}: all-pairs critical path = {}", summary.critical_path.join(" -> "));

    let mut seen: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    let mut detail: Vec<(u64, String)> = Vec::new();
    for (index, transition) in transitions.iter().enumerate() {
        let ticks: BTreeMap<String, u64> = transition
            .nets
            .iter()
            .filter_map(|(name, timing)| timing.arrival_tick().map(|t| (name.clone(), t)))
            .collect();
        let output = netlist
            .outputs
            .iter()
            .max_by_key(|name| ticks.get(name.as_str()).copied().unwrap_or(0))
            .cloned()
            .unwrap_or_default();
        let path = reda::timing::critical_path(netlist, &ticks, &output).join("->");
        let entry = seen.entry(path.clone()).or_insert((0, 0));
        entry.0 = entry.0.max(transition.settle_game_ticks);
        entry.1 += 1;

        let latest_net = ticks.iter().max_by_key(|&(name, tick)| (*tick, name.clone()));
        let outputs_at: Vec<String> = netlist
            .outputs
            .iter()
            .map(|name| format!("{name}@{:?}", ticks.get(name.as_str())))
            .collect();
        detail.push((
            transition.settle_game_ticks,
            format!(
                "t#{index} settle={} outputs[{}] latest_net={:?} path={path}",
                transition.settle_game_ticks,
                outputs_at.join(" "),
                latest_net,
            ),
        ));
    }
    detail.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    eprintln!("--- {label}: the slowest transitions in detail ---");
    for (_, line) in detail.iter().take(8) {
        eprintln!("  {line}");
    }

    // Full arrival dump for the transition `summarize_worst_case` chose, so
    // the backward walk's choice at the last gate can be checked by hand
    // against what each predecessor's own edge delay implies.
    let worst = &transitions[summary.worst_transition_index];
    let mut sorted: Vec<(&String, u64)> = worst
        .nets
        .iter()
        .filter_map(|(name, timing)| timing.arrival_tick().map(|t| (name, t)))
        .collect();
    sorted.sort_by_key(|&(name, tick)| (tick, name.clone()));
    eprintln!(
        "--- {label}: arrivals in the chosen worst transition #{} ---\n  {}",
        summary.worst_transition_index,
        sorted
            .iter()
            .map(|(name, tick)| format!("{name}@{tick}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let glitched: Vec<String> = worst
        .nets
        .iter()
        .filter(|(_, timing)| timing.glitched())
        .map(|(name, timing)| format!("{name}({} changes)", timing.change_count()))
        .collect();
    eprintln!("  glitched nets in that transition: {glitched:?}");
    let mut distinct: Vec<_> = seen.into_iter().collect();
    distinct.sort_by_key(|entry| std::cmp::Reverse(entry.1 .0));
    for (path, (settle, count)) in distinct.iter().take(15) {
        eprintln!("  worst settle {settle} ({count} transitions): {path}");
    }
}

// ---------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------

fn parts_line(parts: &BTreeMap<RoutePart, PartTotals>) -> String {
    ALL_PARTS
        .iter()
        .map(|part| {
            let totals = parts.get(part).copied().unwrap_or_default();
            format!("{part:?}={}r/{}c", totals.repeaters, totals.length)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn report(label: &str, netlist: &Netlist, input_names: &[&str]) {
    eprintln!("\n################ {label} ################");
    let shipped = compile(netlist).expect("reference circuits compile");
    eprintln!("{label}: compile() ships {:?}", shipped.planner_kind());
    let legacy = compile_legacy(netlist).expect("legacy compiles");
    let same_world = legacy.world.size() == shipped.world.size()
        && (0..legacy.world.cells().len()).all(|flat| {
            let (x, y, z) = legacy.world.decode(flat);
            legacy.world.get(x, y, z) == shipped.world.get(x, y, z)
        });
    eprintln!("{label}: compile_legacy world == shipped world? {same_world}");

    report_on(&format!("{label}/legacy"), netlist, &legacy, input_names);
    if !same_world {
        report_on(&format!("{label}/shipped"), netlist, &shipped, input_names);
    }
}

fn report_on(label: &str, netlist: &Netlist, world: &CompiledCircuit, input_names: &[&str]) {
    eprintln!("\n================ {label} ================");
    let shipped = world;

    // --- the simulator, on this world --------------------------------
    let transitions = sweep(shipped, input_names);
    let outputs: Vec<String> = netlist.outputs.clone();
    let summary = summarize_worst_case(netlist, shipped, &outputs, &transitions);
    eprintln!(
        "{label}: MEASURED worst settle = {} game ticks (transition #{}, output `{}`)",
        summary.worst_settle_game_ticks, summary.worst_transition_index, summary.critical_output
    );
    eprintln!("{label}: MEASURED critical path = {}", summary.critical_path.join(" -> "));
    eprintln!(
        "{label}: timing model = {} non-merge gates + {:?} repeaters -> {:?} predicted",
        summary.critical_path_gate_count,
        summary.critical_path_repeater_count,
        summary.critical_path_model_game_ticks
    );
    eprintln!(
        "{label}: glitches by output = {:?}",
        summary.glitch_counts
    );

    // The bare (no-lamp) quantity the planner's delay term is comparable to.
    if let Some(repeaters) = summary.critical_path_repeater_count {
        eprintln!(
            "{label}: timing's bare gates+repeaters term = {} * ({} + {}) = {}",
            TORCH_DELAY_GAME_TICKS,
            summary.critical_path_gate_count,
            repeaters,
            TORCH_DELAY_GAME_TICKS * (summary.critical_path_gate_count as u64 + repeaters as u64)
        );
    }

    // --- the planner's own number, on the same layout ----------------
    let Ok(seed) = seed_from_legacy(netlist, shipped) else {
        eprintln!("{label}: no legacy emission on this world -- planner cost NOT MEASURED here");
        return;
    };
    let (units, hops, start) = planner_critical_path(&seed);
    let cost = seed.cost().delay;
    assert_eq!(
        units * TORCH_DELAY_GAME_TICKS,
        cost,
        "{label}: the re-derived planner walk must reproduce cost().delay exactly"
    );
    eprintln!("{label}: PLANNER cost().delay = {cost} game ticks ({units} units)");
    let planner_path: Vec<String> = std::iter::once(start.clone())
        .chain(hops.iter().map(|hop| hop.sink.clone()))
        .collect();
    eprintln!("{label}: PLANNER critical path = {}", planner_path.join(" -> "));
    let planner_gates: u64 = hops.iter().map(|hop| hop.gate_cost).sum();
    let planner_repeaters: u64 = hops.iter().map(|hop| hop.repeaters).sum();
    eprintln!(
        "{label}: PLANNER path = {planner_gates} non-merge gates + {planner_repeaters} repeaters"
    );

    // --- reconcile, hop by hop ---------------------------------------
    let stats = routing_stats::analyze(netlist, shipped).ok();
    let Some(stats) = stats else {
        eprintln!("{label}: routing_stats::analyze declined this layout -- NOT MEASURED per hop");
        return;
    };

    // Terminal repeater lookup: (source, sink gate, input index) -> repeaters.
    let mut terminal_repeaters: BTreeMap<(String, String, usize), u64> = BTreeMap::new();
    for route in seed.routes() {
        let Some(owner) = route.owner() else { continue };
        for terminal in route.terminals() {
            terminal_repeaters.insert(
                (owner.to_string(), terminal.sink.gate.clone(), terminal.sink.input_index),
                terminal.repeaters,
            );
        }
    }

    let producer_of: HashMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .map(|(i, gate)| (gate.output.as_str(), i))
        .collect();

    let show = |title: &str, path: &[String]| {
        eprintln!("--- {label}: hop-by-hop along the {title} path ---");
        let mut planner_sum = 0u64;
        let mut world_sum = 0usize;
        let mut missing: BTreeMap<RoutePart, usize> = BTreeMap::new();
        for pair in path.windows(2) {
            let (source, sink_output) = (&pair[0], &pair[1]);
            let Some(&gate_index) = producer_of.get(sink_output.as_str()) else {
                eprintln!("  {source} -> {sink_output}: not a gate output, skipped");
                continue;
            };
            let gate = &netlist.gates[gate_index];
            let Some(input_index) = gate.inputs.iter().position(|input| input == source) else {
                eprintln!("  {source} -> {sink_output}: source does not feed this gate, skipped");
                continue;
            };
            let sink_label = format!("{sink_output}.in[{input_index}]");
            let edge = stats.edges.iter().find(|e| e.source == *source && e.sink == sink_label);
            let planned = terminal_repeaters
                .get(&(source.clone(), sink_output.clone(), input_index))
                .copied();
            match (edge, planned) {
                (Some(edge), Some(planned)) => {
                    let world = edge.total().repeaters;
                    planner_sum += planned;
                    world_sum += world;
                    for part in ALL_PARTS {
                        let repeaters = edge.part(part).repeaters;
                        if repeaters > 0 {
                            *missing.entry(part).or_insert(0) += repeaters;
                        }
                    }
                    eprintln!(
                        "  {source} -> {sink_label}: planner={planned} world={world} \
                         delta={} merge={} | {}",
                        world as i64 - planned as i64,
                        gate.is_merge(),
                        parts_line(&edge.parts),
                    );
                }
                (edge, planned) => {
                    eprintln!(
                        "  {source} -> {sink_label}: edge={} terminal={:?} -- MISSING",
                        edge.is_some(),
                        planned
                    );
                }
            }
        }
        eprintln!(
            "  TOTALS on this path: planner={planner_sum} world={world_sum} \
             delta={} | world repeaters by part: {:?}",
            world_sum as i64 - planner_sum as i64,
            missing
        );
    };

    show("MEASURED (timing)", &summary.critical_path);
    show("PLANNER", &planner_path);

    // --- the same walk, with the world's own repeater counts ---------
    //
    // Everything about the planner's walk kept except the one number: each
    // edge's `terminal.repeaters` is replaced by what `routing_stats` finds
    // physically on that edge. If the gap is only the repeater count, this
    // lands on timing's number; if it also picks a different path, it will
    // not, and that is hypothesis (a) showing itself separately.
    let mut corrected = planner_edges(&seed);
    let mut unresolved = 0usize;
    for (owner, list) in corrected.iter_mut() {
        for edge in list.iter_mut() {
            let sink_label = format!("{}.in[{}]", edge.sink, edge.input_index);
            match stats.edges.iter().find(|e| &e.source == owner && e.sink == sink_label) {
                Some(found) => edge.repeaters = found.total().repeaters as u64,
                None => unresolved += 1,
            }
        }
    }
    let (corrected_units, corrected_hops, corrected_start) = walk(corrected);
    let corrected_path: Vec<String> = std::iter::once(corrected_start)
        .chain(corrected_hops.iter().map(|hop| hop.sink.clone()))
        .collect();
    eprintln!(
        "{label}: CORRECTED (planner walk, world repeaters) = {} game ticks ({} units), \
         {} gates + {} repeaters, {unresolved} edges unresolved",
        corrected_units * TORCH_DELAY_GAME_TICKS,
        corrected_units,
        corrected_hops.iter().map(|h| h.gate_cost).sum::<u64>(),
        corrected_hops.iter().map(|h| h.repeaters).sum::<u64>(),
    );
    eprintln!("{label}: CORRECTED path = {}", corrected_path.join(" -> "));

    // --- per-edge ground truth from the simulator --------------------
    //
    // In the worst transition, every net's own last-change tick. For each
    // netlist edge the difference between the two ends is the delay that
    // edge really contributed *on this transition*, provided both ends
    // actually moved and the sink's own latest input is this source (a gate
    // whose other input arrived later is gated by that one, not by this).
    let worst = &transitions[summary.worst_transition_index];
    let arrivals: BTreeMap<&str, u64> = worst
        .nets
        .iter()
        .filter_map(|(name, timing)| timing.arrival_tick().map(|tick| (name.as_str(), tick)))
        .collect();
    eprintln!("--- {label}: measured arrival ticks in the worst transition ---");
    let mut sorted: Vec<_> = arrivals.iter().collect();
    sorted.sort_by_key(|&(name, tick)| (*tick, name.to_string()));
    eprintln!(
        "  {}",
        sorted
            .iter()
            .map(|(name, tick)| format!("{name}@{tick}"))
            .collect::<Vec<_>>()
            .join(" ")
    );

    // Across EVERY transition in the sweep, not just the worst: whenever a
    // gate's output moved and its latest-arriving input moved too, the
    // difference between them is that edge's real contribution. Compare it
    // against both models and count agreement. A primary input's own lever
    // is recorded one game tick after it is set (MIN_SCHEDULE_DELAY), so an
    // edge out of a lever measures one tick short -- noted, not fudged.
    let primary: BTreeSet<&str> = netlist.inputs.iter().map(String::as_str).collect();
    let mut world_ok = 0usize;
    let mut world_bad: Vec<String> = Vec::new();
    let mut planner_ok = 0usize;
    let mut planner_bad: BTreeMap<String, (i64, usize)> = BTreeMap::new();
    for (index, transition) in transitions.iter().enumerate() {
        let ticks: BTreeMap<&str, u64> = transition
            .nets
            .iter()
            .filter_map(|(name, timing)| timing.arrival_tick().map(|t| (name.as_str(), t)))
            .collect();
        for gate in &netlist.gates {
            let Some(&sink_tick) = ticks.get(gate.output.as_str()) else { continue };
            let mut latest: Option<(usize, &str, u64)> = None;
            let mut ambiguous = false;
            for (input_index, input) in gate.inputs.iter().enumerate() {
                let Some(&tick) = ticks.get(input.as_str()) else { continue };
                match latest {
                    Some((_, _, best)) if tick == best => ambiguous = true,
                    Some((_, _, best)) if tick < best => {}
                    _ => {
                        latest = Some((input_index, input.as_str(), tick));
                        ambiguous = false;
                    }
                }
            }
            let Some((input_index, source, source_tick)) = latest else { continue };
            if ambiguous {
                continue;
            }
            let lever_offset = u64::from(primary.contains(source));
            let measured = sink_tick as i64 - source_tick as i64 + lever_offset as i64;
            let gate_cost = u64::from(!gate.is_merge());
            let sink_label = format!("{}.in[{input_index}]", gate.output);
            let world = stats
                .edges
                .iter()
                .find(|e| e.source == *source && e.sink == sink_label)
                .map(|e| TORCH_DELAY_GAME_TICKS as i64 * (e.total().repeaters as i64 + gate_cost as i64));
            let planned = terminal_repeaters
                .get(&(source.to_string(), gate.output.clone(), input_index))
                .map(|&r| TORCH_DELAY_GAME_TICKS as i64 * (r + gate_cost) as i64);
            if let Some(world) = world {
                if world == measured {
                    world_ok += 1;
                } else {
                    world_bad.push(format!(
                        "t#{index} {source} -> {sink_label}: measured {measured}, world model {world}"
                    ));
                }
            }
            if let Some(planned) = planned {
                if planned == measured {
                    planner_ok += 1;
                } else {
                    let entry = planner_bad
                        .entry(format!("{source} -> {sink_label}"))
                        .or_insert((measured - planned, 0));
                    entry.1 += 1;
                }
            }
        }
    }
    // Every transition's own settle and its own backward-walked path, so a
    // path the static walk picks can be checked against every path the
    // circuit ever actually takes -- a static longest path that no single
    // input flip ever sensitises is a false path, and saying so needs the
    // list.
    eprintln!("--- {label}: every transition's settle and critical path ---");
    let mut by_settle: Vec<(u64, String)> = Vec::new();
    for transition in transitions.iter() {
        let ticks: BTreeMap<String, u64> = transition
            .nets
            .iter()
            .filter_map(|(name, timing)| timing.arrival_tick().map(|t| (name.clone(), t)))
            .collect();
        let output = netlist
            .outputs
            .iter()
            .max_by_key(|name| ticks.get(name.as_str()).copied().unwrap_or(0))
            .cloned()
            .unwrap_or_default();
        let path = reda::timing::critical_path(netlist, &ticks, &output);
        by_settle.push((transition.settle_game_ticks, path.join("->")));
    }
    by_settle.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let mut seen: BTreeMap<String, (u64, usize)> = BTreeMap::new();
    for (settle, path) in &by_settle {
        let entry = seen.entry(path.clone()).or_insert((*settle, 0));
        entry.0 = entry.0.max(*settle);
        entry.1 += 1;
    }
    let mut distinct: Vec<_> = seen.into_iter().collect();
    distinct.sort_by_key(|entry| std::cmp::Reverse(entry.1 .0));
    for (path, (settle, count)) in distinct.iter().take(12) {
        eprintln!("  worst settle {settle} ({count} transitions): {path}");
    }

    eprintln!(
        "--- {label}: gating-edge agreement over all {} transitions ---",
        transitions.len()
    );
    eprintln!(
        "  routing_stats(world) model: {world_ok} agree, {} disagree",
        world_bad.len()
    );
    for line in world_bad.iter().take(20) {
        eprintln!("    {line}");
    }
    eprintln!("  planner terminal.repeaters model: {planner_ok} agree, {} edges disagree", planner_bad.len());
    for (edge, (delta, count)) in &planner_bad {
        eprintln!("    {edge}: measured is {delta} game ticks more than planner, on {count} transitions");
    }

    eprintln!("--- {label}: per-edge measured delta vs each model (worst transition) ---");
    for gate in &netlist.gates {
        let Some(&sink_tick) = arrivals.get(gate.output.as_str()) else { continue };
        // Which input gated it: the latest-arriving one.
        let latest = gate
            .inputs
            .iter()
            .map(|input| arrivals.get(input.as_str()).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        for (input_index, input) in gate.inputs.iter().enumerate() {
            let Some(&source_tick) = arrivals.get(input.as_str()) else { continue };
            let sink_label = format!("{}.in[{input_index}]", gate.output);
            let world = stats
                .edges
                .iter()
                .find(|e| &e.source == input && e.sink == sink_label)
                .map(|e| e.total().repeaters);
            let planned = terminal_repeaters
                .get(&(input.clone(), gate.output.clone(), input_index))
                .copied();
            let gate_cost = u64::from(!gate.is_merge());
            let gating = source_tick == latest;
            eprintln!(
                "  {input}@{source_tick} -> {sink_label}@{sink_tick}: measured delta={} \
                 world model={:?} planner model={:?} gating={gating}",
                sink_tick as i64 - source_tick as i64,
                world.map(|r| TORCH_DELAY_GAME_TICKS * (r as u64 + gate_cost)),
                planned.map(|r| TORCH_DELAY_GAME_TICKS * (r + gate_cost)),
            );
        }
    }
}

#[test]
fn reconcile_and4() {
    let (netlist, _) = build_and4_netlist();
    report("and4", &netlist, &AND4_INPUTS);
}

#[test]
fn reconcile_full_adder() {
    let (netlist, _) = build_full_adder_netlist();
    report("full_adder", &netlist, &ADDER_INPUTS);
}

#[test]
fn reconcile_segment_a() {
    let (netlist, _) = build_single_segment_netlist(0);
    report("segment_a", &netlist, &DECODER_INPUTS);
}

#[test]
fn reconcile_seven_segment() {
    let (netlist, _) = build_seven_segment_netlist();
    report("seven_segment", &netlist, &DECODER_INPUTS);
}

#[test]
fn reconcile_verilog_and4() {
    let circuit = verilog::find("verilog:and4").expect("catalog entry");
    let (source, _) = circuit.baked_netlist();
    let netlist = lower(&source).expect("lowers");
    report("verilog:and4", &netlist, &["a", "b", "c", "d"]);
}

#[test]
fn reconcile_verilog_seven_segment() {
    let circuit = verilog::find("verilog:seven_segment").expect("catalog entry");
    let (source, _) = circuit.baked_netlist();
    let netlist = lower_optimised(&source).expect("lowers");
    report("verilog:seven_segment", &netlist, &DECODER_INPUTS);
}

#[test]
fn false_path_full_adder() {
    let (netlist, _) = build_full_adder_netlist();
    false_path_check("full_adder", &netlist, &ADDER_INPUTS);
}

#[test]
fn false_path_and4() {
    let (netlist, _) = build_and4_netlist();
    false_path_check("and4", &netlist, &AND4_INPUTS);
}

/// THE INVARIANT, over every routed edge of every reference circuit.
///
/// Two independent derivations of the same physical quantity have to agree
/// edge for edge:
///
///  * `RouteTerminal::repeaters`, counted by the emitter **as it writes**,
///    attributed to the electrical segment carrying each repeater and summed
///    over the segments one sink's signal passes through
///    (`compile::resolve_terminal_repeaters`);
///  * `routing_stats::analyze(..).edges[..].total().repeaters`, which never
///    writes anything -- it recomputes the emitter's geometry and **reads the
///    finished world back** along it, classifying each cell by the block that
///    actually stands there.
///
/// Neither can be derived from the other, which is what makes the agreement
/// worth asserting: the first knows the decisions, the second knows the
/// blocks.
///
///   terminal.repeaters == Column + Ramp + Track + GateEntry + Bypass
///
/// Before the repair this held for none of the fanned-out edges. The counter
/// was per-pass and only the Columns pass ever read it, so a terminal carried
/// `Column + GateEntry + Bypass` and dropped every ramp and track repeater --
/// 4 of and4's 11, 17 of full_adder's 53, 160 of segment_a's 340, 397 of
/// seven_segment's 819, 2 of verilog:and4's 9, 182 of verilog:seven_segment's
/// 447.
///
/// This prints the count of edges that obey and every edge that does not.
#[test]
fn terminal_repeaters_are_every_repeater_on_the_path() {
    let seven = build_seven_segment_netlist().0;
    let vand4 = lower(&verilog::find("verilog:and4").unwrap().baked_netlist().0).unwrap();
    let vseven =
        lower_optimised(&verilog::find("verilog:seven_segment").unwrap().baked_netlist().0).unwrap();
    let circuits: Vec<(&str, Netlist)> = vec![
        ("and4", build_and4_netlist().0),
        ("full_adder", build_full_adder_netlist().0),
        ("segment_a", build_single_segment_netlist(0).0),
        ("seven_segment", seven),
        ("verilog:and4", vand4),
        ("verilog:seven_segment", vseven),
    ];

    for (label, netlist) in &circuits {
        let legacy = compile_legacy(netlist).expect("legacy compiles");
        let seed = seed_from_legacy(netlist, &legacy).expect("seeds");
        let stats = routing_stats::analyze(netlist, &legacy).expect("legacy is analysable");

        let mut terminal_repeaters: BTreeMap<(String, String, usize), u64> = BTreeMap::new();
        for route in seed.routes() {
            let Some(owner) = route.owner() else { continue };
            for terminal in route.terminals() {
                terminal_repeaters.insert(
                    (owner.to_string(), terminal.sink.gate.clone(), terminal.sink.input_index),
                    terminal.repeaters,
                );
            }
        }

        let (mut obey, mut ramp_seen, mut total_missing, mut total_world) = (0usize, 0usize, 0i64, 0usize);
        let mut violations = Vec::new();
        for edge in &stats.edges {
            let (gate, index) = edge
                .sink
                .rsplit_once(".in[")
                .map(|(gate, rest)| (gate.to_string(), rest.trim_end_matches(']').parse::<usize>().unwrap()))
                .expect("sink label shape");
            let Some(&planned) = terminal_repeaters.get(&(edge.source.clone(), gate.clone(), index))
            else {
                violations.push(format!("{} -> {}: no terminal recorded", edge.source, edge.sink));
                continue;
            };
            let world = edge.total().repeaters;
            ramp_seen += edge.part(RoutePart::Ramp).repeaters;
            total_world += world;
            total_missing += world as i64 - planned as i64;
            if planned as usize == world {
                obey += 1;
            } else {
                violations.push(format!(
                    "{} -> {}: terminal={planned} world={world} ({})",
                    edge.source,
                    edge.sink,
                    parts_line(&edge.parts)
                ));
            }
        }
        eprintln!(
            "{label}: {obey}/{} edges obey `terminal.repeaters == every repeater on the path`; \
             world repeaters over all edges = {total_world}, unattributed = {total_missing}, \
             ramp repeaters seen = {ramp_seen}",
            stats.edges.len()
        );
        for line in &violations {
            eprintln!("    VIOLATION {line}");
        }
        assert!(violations.is_empty(), "{label}: the invariant does not hold");
    }
}

#[test]
fn false_path_segment_a() {
    let (netlist, _) = build_single_segment_netlist(0);
    false_path_check("segment_a", &netlist, &DECODER_INPUTS);
}

#[test]
fn false_path_seven_segment() {
    let (netlist, _) = build_seven_segment_netlist();
    false_path_check("seven_segment", &netlist, &DECODER_INPUTS);
}

/// The three circuits `compile()` hands to relaxation get their delay term
/// from a candidate nobody can read back out of the shipped
/// `CompiledCircuit`. This re-plans them with the public entry point, checks
/// whether the world that comes out is the one that shipped, and prints the
/// candidate's own `cost().delay` next to what the shipped world measures.
#[test]
fn the_unified3d_path_delay_term() {
    use reda::compile::planner::{plan_from_netlist, realise_and_verify, PortPlacements};

    let vand4 = lower(&verilog::find("verilog:and4").unwrap().baked_netlist().0).unwrap();
    let circuits: Vec<(&str, Netlist, Vec<&str>)> = vec![
        ("and4", build_and4_netlist().0, AND4_INPUTS.to_vec()),
        ("full_adder", build_full_adder_netlist().0, ADDER_INPUTS.to_vec()),
        ("verilog:and4", vand4, vec!["a", "b", "c", "d"]),
    ];

    for (label, netlist, inputs) in &circuits {
        let shipped = compile(netlist).expect("compiles");
        if shipped.planner_kind() != PlannerKind::Unified3d {
            eprintln!("{label}: ships {:?}, skipped here", shipped.planner_kind());
            continue;
        }
        let candidate = match plan_from_netlist(netlist, &PortPlacements::default()) {
            Ok(candidate) => candidate,
            Err(error) => {
                eprintln!("{label}: plan_from_netlist failed ({error}) -- NOT MEASURED");
                continue;
            }
        };
        let realised = realise_and_verify(&candidate, netlist, shipped.world.size());
        let same = realised
            .as_ref()
            .map(|realised| {
                realised.world.size() == shipped.world.size()
                    && (0..realised.world.cells().len()).all(|flat| {
                        let (x, y, z) = realised.world.decode(flat);
                        realised.world.get(x, y, z) == shipped.world.get(x, y, z)
                    })
            })
            .unwrap_or(false);
        let transitions = sweep(&shipped, inputs);
        let outputs: Vec<String> = netlist.outputs.clone();
        let summary = summarize_worst_case(netlist, &shipped, &outputs, &transitions);
        eprintln!(
            "{label}: Unified3d shipped world measures {} game ticks settle; \
             re-planned candidate cost().delay = {} (re-plan reproduces the shipped world? {same}); \
             routing_stats on the shipped world: {}",
            summary.worst_settle_game_ticks,
            candidate.cost().delay,
            routing_stats::analyze(netlist, &shipped)
                .map(|report| format!("{} edges", report.edges.len()))
                .unwrap_or_else(|error| format!("refused: {error}")),
        );
        eprintln!(
            "  measured critical path = {} ({} non-merge gates, repeater term {:?})",
            summary.critical_path.join(" -> "),
            summary.critical_path_gate_count,
            summary.critical_path_repeater_count
        );
    }
}

/// The same gating-edge agreement test as the legacy one, but on the
/// relaxation/A* layout, where there is no `routing_stats` to read the world
/// with -- so the only two things to compare are the simulator and the
/// candidate's own `terminal.repeaters`. This is the control that says
/// whether the miss is in the *model* or in the *legacy emitter's* bookkeeping.
#[test]
fn unified3d_terminal_repeaters_against_the_simulator() {
    use reda::compile::planner::{plan_from_netlist, PortPlacements};

    let vand4 = lower(&verilog::find("verilog:and4").unwrap().baked_netlist().0).unwrap();
    let circuits: Vec<(&str, Netlist, Vec<&str>)> = vec![
        ("and4", build_and4_netlist().0, AND4_INPUTS.to_vec()),
        ("full_adder", build_full_adder_netlist().0, ADDER_INPUTS.to_vec()),
        ("verilog:and4", vand4, vec!["a", "b", "c", "d"]),
    ];

    for (label, netlist, inputs) in &circuits {
        let shipped = compile(netlist).expect("compiles");
        assert_eq!(shipped.planner_kind(), PlannerKind::Unified3d, "{label}");
        let candidate = plan_from_netlist(netlist, &PortPlacements::default()).expect("plans");

        let mut terminal_repeaters: BTreeMap<(String, String, usize), u64> = BTreeMap::new();
        for route in candidate.routes() {
            let Some(owner) = route.owner() else { continue };
            for terminal in route.terminals() {
                terminal_repeaters.insert(
                    (owner.to_string(), terminal.sink.gate.clone(), terminal.sink.input_index),
                    terminal.repeaters,
                );
            }
        }

        let transitions = sweep(&shipped, inputs);
        let primary: BTreeSet<&str> = netlist.inputs.iter().map(String::as_str).collect();
        let (mut agree, mut disagree) = (0usize, BTreeMap::<String, (i64, usize)>::new());
        for transition in &transitions {
            let ticks: BTreeMap<&str, u64> = transition
                .nets
                .iter()
                .filter_map(|(name, timing)| timing.arrival_tick().map(|t| (name.as_str(), t)))
                .collect();
            for gate in &netlist.gates {
                let Some(&sink_tick) = ticks.get(gate.output.as_str()) else { continue };
                let mut latest: Option<(usize, &str, u64)> = None;
                let mut ambiguous = false;
                for (input_index, input) in gate.inputs.iter().enumerate() {
                    let Some(&tick) = ticks.get(input.as_str()) else { continue };
                    match latest {
                        Some((_, _, best)) if tick == best => ambiguous = true,
                        Some((_, _, best)) if tick < best => {}
                        _ => {
                            latest = Some((input_index, input.as_str(), tick));
                            ambiguous = false;
                        }
                    }
                }
                let Some((input_index, source, source_tick)) = latest else { continue };
                if ambiguous {
                    continue;
                }
                let offset = i64::from(primary.contains(source));
                let measured = sink_tick as i64 - source_tick as i64 + offset;
                let gate_cost = u64::from(!gate.is_merge());
                let Some(&planned) =
                    terminal_repeaters.get(&(source.to_string(), gate.output.clone(), input_index))
                else {
                    continue;
                };
                let model = TORCH_DELAY_GAME_TICKS as i64 * (planned + gate_cost) as i64;
                if model == measured {
                    agree += 1;
                } else {
                    let entry = disagree
                        .entry(format!("{source} -> {}.in[{input_index}]", gate.output))
                        .or_insert((measured - model, 0));
                    entry.1 += 1;
                }
            }
        }
        eprintln!(
            "{label} (Unified3d): candidate terminal.repeaters vs simulator -- {agree} gating \
             edges agree, {} disagree",
            disagree.len()
        );
        for (edge, (delta, count)) in &disagree {
            eprintln!("    {edge}: measured is {delta} game ticks off, on {count} transitions");
        }
    }
}

#[test]
fn false_path_verilog_and4() {
    let netlist = lower(&verilog::find("verilog:and4").unwrap().baked_netlist().0).unwrap();
    false_path_check("verilog:and4", &netlist, &["a", "b", "c", "d"]);
}

#[test]
fn false_path_verilog_seven_segment() {
    let netlist =
        lower_optimised(&verilog::find("verilog:seven_segment").unwrap().baked_netlist().0).unwrap();
    false_path_check("verilog:seven_segment", &netlist, &DECODER_INPUTS);
}

/// Whether the three circuits `compile()` gives to the legacy emitter can be
/// planned at all at the full rip-up budget -- i.e. whether a second compile
/// path even exists for them to be measured on.
#[test]
#[ignore = "slow: runs the full rip-up budget on circuits compile() already declined"]
fn is_there_a_unified3d_path_for_the_legacy_circuits() {
    use reda::compile::planner::{plan_from_netlist, PortPlacements};
    use std::time::Instant;

    let vseven =
        lower_optimised(&verilog::find("verilog:seven_segment").unwrap().baked_netlist().0).unwrap();
    let circuits: Vec<(&str, Netlist)> = vec![
        ("segment_a", build_single_segment_netlist(0).0),
        ("seven_segment", build_seven_segment_netlist().0),
        ("verilog:seven_segment", vseven),
    ];
    for (label, netlist) in &circuits {
        let started = Instant::now();
        let result = plan_from_netlist(netlist, &PortPlacements::default());
        match result {
            Ok(candidate) => eprintln!(
                "{label}: plans in {:?}, cost().delay = {}",
                started.elapsed(),
                candidate.cost().delay
            ),
            Err(error) => eprintln!("{label}: refuses to plan in {:?} -- {error}", started.elapsed()),
        }
    }
}
