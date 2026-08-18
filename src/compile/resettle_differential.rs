//! `redstone::simulator::differential` run wide: every truth table this
//! project relies on, checked against a full re-settle.
//!
//! # What is being measured
//!
//! `Simulator::run_until_stable` could leave a dust run stale behind a
//! one-way dust edge -- see `redstone::simulator::differential`'s module doc
//! for the mechanism and the measured case that found it, and
//! `propagate::active_dust_networks` for the incoming-edge flood that closed
//! it. Everything this branch claims is read through that settle: the four
//! reference circuits' truth tables, the negotiated plans' vector sweeps,
//! `a_self_placed_and4_computes_and4`'s 240-transition worst-settle, and
//! `genuine_decay_is_still_refused`'s per-origin isolation readings. This
//! module runs the differential over each of those surfaces and reports, per
//! settled state, every cell the incremental settle got wrong -- and,
//! decisively, whether any output reading moves.
//!
//! # How to run the harnesses
//!
//! ```text
//! cargo test --release \
//!   compile::resettle_differential -- --ignored --nocapture
//! ```
//!
//! The non-ignored tests in here are the gates that run in `check.sh` on
//! every change -- the reported case settling clean, and one full compiled
//! circuit swept vector by vector against the oracle -- because an oracle
//! that can silently start lying again is the thing the 2026-08-19 fix was
//! about. The wide sweeps are `#[ignore]`d measurement harnesses in the
//! tree's usual style.

use std::collections::BTreeSet;

use crate::compile::{self, Netlist};
use crate::compile::planner::{
    self, candidate_world_size, plan_negotiated_on_schedule, PortPlacements, PresentSchedule,
    NEGOTIATION_ROUNDS,
};
use crate::compile::strength_differential::{every_source, isolation_world};
use crate::redstone::simulator::differential::{resettle_differential, CellDiff, Resettle};
use crate::redstone::simulator::connectivity::dust_connections;
use crate::redstone::simulator::position::{Position, HORIZONTAL};
use crate::redstone::simulator::Simulator;
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// The same settle ceiling every other harness in the tree uses.
const MAX_TICKS: u64 = 2000;

/// The schedule negotiated `segment_a` routes under -- named, as
/// `strength_differential::tests::SEGMENT_A_SCHEDULE` names it, because a case
/// that does not say which schedule it ran under is not reproducible.
const SEGMENT_A_SCHEDULE: PresentSchedule = PresentSchedule::starting_at(8);

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) {
    let mut state = simulator
        .world()
        .get(position.0, position.1, position.2)
        .clone();
    assert_eq!(state.kind, BlockKind::Lever, "input position must hold a lever");
    state.lit = on;
    simulator
        .world_mut()
        .set(position.0, position.1, position.2, state);
}

/// One diff cell, with the local geometry that decides whether the
/// incremental flood could have reached it: for a dust cell, every directed
/// dust edge in and out of it, marked `one-way` when the reverse walk does
/// not exist. A one-way edge *into* a stale cell is the shape the seed flood
/// could not cross from below until `active_dust_networks` learnt to walk
/// incoming edges (2026-08-19) -- if one shows up beside a diff again, the
/// first suspect is that walk.
fn describe(world: &World, diff: &CellDiff) -> String {
    let mut extra = String::new();
    if diff.kind == BlockKind::RedstoneWire {
        let cell = diff.position;
        let mut edges: Vec<String> = Vec::new();
        for facing in HORIZONTAL {
            let out: BTreeSet<Position> =
                dust_connections(world, cell, facing).iter().collect();
            for candidate in [
                cell.offset(facing),
                cell.offset(facing).up(),
                cell.offset(facing).down(),
            ] {
                if world.get(candidate.x, candidate.y, candidate.z).kind
                    != BlockKind::RedstoneWire
                {
                    continue;
                }
                let outgoing = out.contains(&candidate);
                let incoming = dust_connections(world, candidate, facing.opposite())
                    .iter()
                    .any(|position| position == cell);
                let arrow = match (incoming, outgoing) {
                    (true, true) => "<->",
                    (true, false) => "-> (ONE-WAY IN)",
                    (false, true) => "<- (ONE-WAY OUT)",
                    (false, false) => continue,
                };
                edges.push(format!(
                    "({}, {}, {}) {arrow}",
                    candidate.x, candidate.y, candidate.z
                ));
            }
        }
        let lid = cell.up();
        extra = format!(
            "  edges: [{}]  lid above: {:?}",
            edges.join(", "),
            world.get(lid.x, lid.y, lid.z).kind
        );
    }
    format!(
        "    ({}, {}, {}) {:?} power {} -> {} lit {} -> {}{extra}",
        diff.position.x,
        diff.position.y,
        diff.position.z,
        diff.kind,
        diff.settled_power,
        diff.resettled_power,
        diff.settled_lit,
        diff.resettled_lit,
    )
}

/// Settle-vs-oracle over one already-settled world, folded to a summary line.
fn diff_line(label: &str, settled: &World) -> (Resettle, String) {
    let result = resettle_differential(settled, MAX_TICKS)
        .unwrap_or_else(|error| panic!("{label}: the oracle must settle: {error:?}"));
    let line = format!(
        "{label}: {} stale cell(s), fixpoint in {} pass(es)",
        result.diffs.len(),
        result.passes
    );
    (result, line)
}

// -------------------------------------------------------------------------
// The gates: the reported case and one compiled circuit, in check.sh.
// -------------------------------------------------------------------------

/// The world the defect was reported in: negotiated `full_adder`
/// ([`PresentSchedule::SHIPPING`]), realised, `g11`'s per-origin isolation
/// world, with the route repeater at `(56, 1, 91)` reversed.
fn the_reported_world() -> World {
    use crate::circuits::full_adder::build_full_adder_netlist;

    let (netlist, _) = build_full_adder_netlist();
    let candidate = plan_negotiated_on_schedule(
        &netlist,
        &PortPlacements::default(),
        NEGOTIATION_ROUNDS,
        PresentSchedule::SHIPPING,
    )
    .expect("negotiated full_adder routes");
    let parts = planner::realise_without_verifying(
        &candidate,
        &netlist,
        candidate_world_size(&candidate),
    )
    .expect("it realises");
    let world = &parts.realised.world;
    let ports = &parts.realised.ports;

    let g11 = netlist
        .gates
        .iter()
        .find(|gate| gate.name == "g11")
        .expect("full_adder has a gate named g11");
    let origin = ports.gate_output_positions[&g11.output];
    let origin = Position::new(origin.0, origin.1, origin.2);

    let sources = every_source(&netlist, &ports.gate_output_positions, &ports.input_positions);
    let mut isolated = isolation_world(world, &sources, origin);

    let repeater = Position::new(56, 1, 91);
    let mut state = isolated.get(repeater.x, repeater.y, repeater.z).clone();
    assert_eq!(
        state.kind,
        BlockKind::Repeater,
        "the reported recipe names a repeater at (56, 1, 91)"
    );
    state.facing = Some(state.facing.expect("a repeater has a facing").opposite());
    isolated.set(repeater.x, repeater.y, repeater.z, state);
    isolated
}

/// **The reported case, settling clean.** This test used to be the pin that
/// asserted the defect: as settled incrementally, the dust at `(56, 1, 99)`
/// read 0 while `(57, 2, 99)` read 10 and connected to it, and only the full
/// re-settle read the true chain 9, 8, 7, 6 into `(60, 1, 99)`. With
/// `active_dust_networks` flooding incoming dust edges too, the incremental
/// settle must now read that chain itself, and the differential over the
/// whole world must be empty.
///
/// Rule 2, measured: with the incoming-edge walk removed from
/// `active_dust_networks`' flood, this test goes red at `(56, 1, 99) == 0`
/// with the differential naming all four stale cells -- the old bookkeeping
/// cannot pass it.
#[test]
fn the_reported_stale_dust_case_settles_clean() {
    let world = the_reported_world();
    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("the isolation world settles");
    let settled = simulator.world().clone();

    // The chain the report named, read straight out of the incremental
    // settle -- the values only the oracle used to see.
    for (expected, position) in [
        (10u8, Position::new(57, 2, 99)),
        (9, Position::new(56, 1, 99)),
        (8, Position::new(57, 1, 99)),
        (7, Position::new(58, 1, 99)),
        (6, Position::new(59, 1, 99)),
    ] {
        assert_eq!(
            settled.get(position.x, position.y, position.z).power,
            expected,
            "({}, {}, {}) must carry the re-settled chain's value",
            position.x,
            position.y,
            position.z
        );
    }

    let (result, line) = diff_line("reported case", &settled);
    eprintln!("{line}");
    for diff in &result.diffs {
        eprintln!("{}", describe(&result.world, diff));
    }
    assert!(
        result.diffs.is_empty(),
        "the reported world's incremental settle must agree with the full \
         re-settle everywhere"
    );
}

/// **The standing gate on a whole compiled circuit.** and4 through the real
/// `compile()`, swept exactly the way the truth-table tests sweep it -- one
/// long-lived simulator, levers thrown per mask -- with the full-re-settle
/// differential at every one of the 16 settled states. Not `#[ignore]`d, so
/// `check.sh` runs it on every change: the incremental settle is the oracle
/// everything in this project reads through, and an oracle that can silently
/// start lying again is the defect this file exists to catch.
#[test]
fn and4s_full_sweep_is_differential_clean() {
    use crate::circuits::and4::build_and4_netlist;

    let (netlist, _) = build_and4_netlist();
    let (vectors, with_diffs, stale, moves) = sweep_compiled("and4 [gate]", &netlist, true);
    assert_eq!(vectors, 16, "and4 has 16 input vectors");
    assert_eq!(
        (with_diffs, stale),
        (0, 0),
        "every settled state must agree with its full re-settle"
    );
    assert!(
        moves.is_empty(),
        "no output reading may move under the oracle:\n{}",
        moves.join("\n")
    );
}

// -------------------------------------------------------------------------
// The wide sweeps.
// -------------------------------------------------------------------------

/// Sweep every input vector of one compiled circuit exactly the way the
/// truth-table tests do -- one long-lived simulator, levers thrown per mask --
/// and run the differential at every settled state.
///
/// Returns `(vectors, vectors_with_diffs, total_stale_cells, output_moves)`
/// where `output_moves` lists every (mask, output) whose reading differs
/// between the settled world and its full re-settle. A non-empty
/// `output_moves` is a truth table this project relies on reading
/// differently under the oracle.
fn sweep_compiled(
    name: &str,
    netlist: &Netlist,
    verbose: bool,
) -> (usize, usize, usize, Vec<String>) {
    let compiled = compile::compile(netlist)
        .unwrap_or_else(|error| panic!("{name} must compile: {error}"));
    let inputs: Vec<(String, (i32, i32, i32))> = netlist
        .inputs
        .iter()
        .map(|input| (input.clone(), compiled.input_positions[input]))
        .collect();
    let outputs: Vec<(String, (i32, i32, i32))> = netlist
        .outputs
        .iter()
        .map(|output| (output.clone(), compiled.output_positions[output]))
        .collect();

    let mut simulator = Simulator::new(compiled.world.clone());
    simulator
        .run_until_stable(MAX_TICKS)
        .unwrap_or_else(|error| panic!("{name} must settle before the first vector: {error:?}"));

    let mut vectors = 0usize;
    let mut vectors_with_diffs = 0usize;
    let mut total_stale = 0usize;
    let mut output_moves: Vec<String> = Vec::new();

    for mask in 0u32..(1u32 << inputs.len()) {
        for (bit, (_, position)) in inputs.iter().enumerate() {
            let on = (mask >> (inputs.len() - 1 - bit)) & 1 == 1;
            set_lever(&mut simulator, *position, on);
        }
        simulator
            .run_until_stable(MAX_TICKS)
            .unwrap_or_else(|error| panic!("{name} must settle at {mask:0width$b}: {error:?}", width = inputs.len()));
        let settled = simulator.world().clone();
        let (result, line) = diff_line(&format!("{name} mask {mask:0width$b}", width = inputs.len()), &settled);

        vectors += 1;
        if !result.diffs.is_empty() {
            vectors_with_diffs += 1;
            total_stale += result.diffs.len();
            eprintln!("{line}");
            if verbose {
                for diff in &result.diffs {
                    eprintln!("{}", describe(&result.world, diff));
                }
            }
        }

        for (output, position) in &outputs {
            let before = settled.get(position.0, position.1, position.2).lit;
            let after = result.world.get(position.0, position.1, position.2).lit;
            if before != after {
                output_moves.push(format!(
                    "{name} mask {mask:0width$b} output `{output}`: settled {before}, re-settled {after}",
                    width = inputs.len()
                ));
            }
        }
    }

    (vectors, vectors_with_diffs, total_stale, output_moves)
}

/// **The question that decides how urgent the defect is**: do any of the six
/// condition circuits' truth tables, read through the real `compile()` and
/// the same chained sweep the truth-table tests use, read differently under a
/// full re-settle?
#[test]
#[ignore = "measurement harness: compiles and sweeps all six condition circuits, ~minutes; run --release --nocapture"]
fn resettle_differential_on_the_six_condition_circuits() {
    let mut all_moves: Vec<String> = Vec::new();
    eprintln!("circuit                 vectors  with-diffs  stale-cells  output-moves");
    for (name, netlist) in compile::tests::the_six_condition_netlists() {
        let (vectors, with_diffs, stale, moves) = sweep_compiled(name, &netlist, true);
        eprintln!(
            "{name:<22} {vectors:>8} {with_diffs:>11} {stale:>12} {:>13}",
            moves.len()
        );
        all_moves.extend(moves);
    }
    for line in &all_moves {
        eprintln!("OUTPUT MOVED: {line}");
    }
    assert!(
        all_moves.is_empty(),
        "a truth table this project relies on reads differently under a full \
         re-settle:\n{}",
        all_moves.join("\n")
    );
}

/// The two negotiated plans, swept vector by vector on a **fresh** simulator
/// each time (the way `strength_differential::measure` sweeps them), with a
/// differential at every settled state.
#[test]
#[ignore = "measurement harness: negotiates, realises and sweeps full_adder and segment_a; run --release --nocapture"]
fn resettle_differential_on_the_negotiated_plans() {
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::build_single_segment_netlist;

    let cases: Vec<(&str, Netlist, PresentSchedule)> = vec![
        (
            "full_adder [NEGOTIATED]",
            build_full_adder_netlist().0,
            PresentSchedule::SHIPPING,
        ),
        (
            "segment_a [NEGOTIATED]",
            build_single_segment_netlist(0).0,
            SEGMENT_A_SCHEDULE,
        ),
    ];

    for (name, netlist, schedule) in cases {
        let Ok(candidate) = plan_negotiated_on_schedule(
            &netlist,
            &PortPlacements::default(),
            NEGOTIATION_ROUNDS,
            schedule,
        ) else {
            eprintln!("{name}: does not route under {schedule:?} -- NOT MEASURED");
            continue;
        };
        let parts = planner::realise_without_verifying(
            &candidate,
            &netlist,
            candidate_world_size(&candidate),
        )
        .unwrap_or_else(|error| panic!("{name} must realise: {error}"));
        let world = &parts.realised.world;
        let ports = &parts.realised.ports;

        let inputs: Vec<(String, (i32, i32, i32))> = netlist
            .inputs
            .iter()
            .map(|input| (input.clone(), ports.input_positions[input]))
            .collect();

        let mut vectors_with_diffs = 0usize;
        let mut total_stale = 0usize;
        for mask in 0u32..(1u32 << inputs.len()) {
            let mut fresh = world.clone();
            for (bit, (_, position)) in inputs.iter().enumerate() {
                let on = (mask >> (inputs.len() - 1 - bit)) & 1 == 1;
                let mut state = fresh.get(position.0, position.1, position.2).clone();
                state.lit = on;
                fresh.set(position.0, position.1, position.2, state);
            }
            let mut simulator = Simulator::new(fresh);
            simulator
                .run_until_stable(MAX_TICKS)
                .unwrap_or_else(|error| panic!("{name} must settle at {mask:04b}: {error:?}"));
            let settled = simulator.world().clone();
            let (result, line) =
                diff_line(&format!("{name} mask {mask:04b}"), &settled);
            if !result.diffs.is_empty() {
                vectors_with_diffs += 1;
                total_stale += result.diffs.len();
                eprintln!("{line}");
                for diff in &result.diffs {
                    eprintln!("{}", describe(&result.world, diff));
                }
            }
        }
        eprintln!(
            "{name}: {} of {} vectors carry stale cells, {} stale cells total",
            vectors_with_diffs,
            1u32 << inputs.len(),
            total_stale
        );
    }
}

/// `a_self_placed_and4_computes_and4`'s exact 240-transition loop, with a
/// differential at both settled states of every transition. If the oracle
/// disagrees anywhere in here, that test's headline number (worst settle,
/// and the 240/240 output readings) is not trustworthy.
#[test]
#[ignore = "measurement harness: 240 transitions x 2 settles x oracle; run --release --nocapture"]
fn resettle_differential_on_and4s_240_transitions() {
    use crate::circuits::and4::build_and4_netlist;

    let (netlist, _) = build_and4_netlist();
    let candidate = planner::plan_from_netlist(&netlist, &PortPlacements::default())
        .expect("and4 must be placeable");
    let realised = planner::realise_and_verify(
        &candidate,
        &netlist,
        candidate_world_size(&candidate),
    )
    .expect("and4 must be legal");

    let set_inputs = |simulator: &mut Simulator, mask: u8| {
        for (bit, name) in ["a", "b", "c", "d"].iter().enumerate() {
            let at = realised.ports.input_positions[*name];
            let mut state = simulator.world().get(at.0, at.1, at.2).clone();
            state.lit = (mask >> bit) & 1 == 1;
            simulator.world_mut().set(at.0, at.1, at.2, state);
        }
    };

    let out = realised.ports.output_positions[&netlist.outputs[0]];
    let mut settles_with_diffs = 0usize;
    let mut total_stale = 0usize;
    let mut output_moves = 0usize;
    for from in 0u8..16 {
        for to in 0u8..16 {
            if from == to {
                continue;
            }
            let mut simulator = Simulator::new(realised.world.clone());
            set_inputs(&mut simulator, from);
            simulator.run_until_stable(MAX_TICKS).expect("settles at from");
            for (label, mask) in [("from", from), ("to", to)] {
                if label == "to" {
                    set_inputs(&mut simulator, to);
                    simulator.run_until_stable(MAX_TICKS).expect("settles at to");
                }
                let settled = simulator.world().clone();
                let result = resettle_differential(&settled, MAX_TICKS)
                    .expect("the oracle settles");
                if !result.diffs.is_empty() {
                    settles_with_diffs += 1;
                    total_stale += result.diffs.len();
                    eprintln!(
                        "{from:04b} -> {to:04b} at `{label}` ({mask:04b}): {} stale cell(s)",
                        result.diffs.len()
                    );
                    for diff in &result.diffs {
                        eprintln!("{}", describe(&result.world, diff));
                    }
                }
                let before = settled.get(out.0, out.1, out.2).lit;
                let after = result.world.get(out.0, out.1, out.2).lit;
                if before != after {
                    output_moves += 1;
                    eprintln!(
                        "{from:04b} -> {to:04b} at `{label}`: OUTPUT MOVED {before} -> {after}"
                    );
                }
            }
        }
    }
    eprintln!(
        "and4 240 transitions: {settles_with_diffs} of 480 settled states carry stale \
         cells ({total_stale} cells), {output_moves} output readings move"
    );
    assert_eq!(
        output_moves, 0,
        "the 240-transition sweep's output readings move under the oracle"
    );
}

/// Every per-origin isolation world of the three plans
/// `genuine_decay_is_still_refused` measures through, settled as that test
/// settles them, with a differential on each. Any staleness in here is the
/// condition that manufactures `solo == 0` -- the reading that test counts a
/// "biting" injection by.
#[test]
#[ignore = "measurement harness: ~60 isolation worlds x settle x oracle; run --release --nocapture"]
fn resettle_differential_on_the_isolation_worlds() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;

    struct IsolationCase {
        name: &'static str,
        netlist: Netlist,
        negotiated: bool,
    }
    let cases = vec![
        IsolationCase { name: "and4", netlist: build_and4_netlist().0, negotiated: false },
        IsolationCase {
            name: "full_adder",
            netlist: build_full_adder_netlist().0,
            negotiated: false,
        },
        IsolationCase {
            name: "full_adder [NEGOTIATED]",
            netlist: build_full_adder_netlist().0,
            negotiated: true,
        },
    ];

    for case in cases {
        let candidate = if case.negotiated {
            plan_negotiated_on_schedule(
                &case.netlist,
                &PortPlacements::default(),
                NEGOTIATION_ROUNDS,
                PresentSchedule::SHIPPING,
            )
            .expect("negotiated full_adder routes")
        } else {
            planner::plan_from_netlist_within(
                &case.netlist,
                &PortPlacements::default(),
                planner::TRIAL_RIP_UP_ROUNDS,
            )
            .expect("the shipping plan reproduces")
        };
        let parts = planner::realise_without_verifying(
            &candidate,
            &case.netlist,
            candidate_world_size(&candidate),
        )
        .expect("it realises");
        let world = &parts.realised.world;
        let ports = &parts.realised.ports;
        let sources =
            every_source(&case.netlist, &ports.gate_output_positions, &ports.input_positions);

        let mut worlds_with_diffs = 0usize;
        let mut total_stale = 0usize;
        for &origin in &sources {
            let isolated = isolation_world(world, &sources, origin);
            let mut simulator = Simulator::new(isolated);
            simulator
                .run_until_stable(MAX_TICKS)
                .expect("an isolation world settles");
            let settled = simulator.world().clone();
            let (result, line) = diff_line(
                &format!(
                    "{} isolation of ({}, {}, {})",
                    case.name, origin.x, origin.y, origin.z
                ),
                &settled,
            );
            if !result.diffs.is_empty() {
                worlds_with_diffs += 1;
                total_stale += result.diffs.len();
                eprintln!("{line}");
                for diff in &result.diffs {
                    eprintln!("{}", describe(&result.world, diff));
                }
            }
        }
        eprintln!(
            "{}: {} of {} baseline isolation worlds carry stale cells ({} cells)",
            case.name,
            worlds_with_diffs,
            sources.len(),
            total_stale
        );
    }
}

/// The injection space `genuine_decay_is_still_refused` counts over -- its
/// three plans, its repeater shapes (deleted / replaced-by-dust, which
/// subsume its ramp-cut, plus the riser-turned-glass), and every origin's
/// isolation world -- with `reversed` added because it is the shape the
/// reported case was found under. Every one of those settled isolation
/// worlds is differenced against the oracle. This is the direct measurement
/// of how much of that test's claimed counting surface (64 / 30 / 3 biting
/// injections) the stale settle contaminates: its assertion direction is
/// unaffected either way, but its counts read through exactly these settles,
/// and a stale ZERO at a sink is precisely the `solo == 0` condition it
/// counts a bite by.
#[test]
#[ignore = "measurement harness: 3 plans x repeaters x 4 shapes x origins, ~8k settles; run --release --nocapture"]
fn resettle_differential_on_the_injected_isolation_worlds() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::redstone::world::block::BlockState;

    fn glass() -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::Glass;
        state.name = "minecraft:glass".to_string();
        state
    }

    let cases: Vec<(&'static str, Netlist, bool)> = vec![
        ("and4", build_and4_netlist().0, false),
        ("full_adder", build_full_adder_netlist().0, false),
        ("full_adder [NEGOTIATED]", build_full_adder_netlist().0, true),
    ];

    let mut settles = 0usize;
    let mut stale_worlds = 0usize;
    let mut stale_zero_worlds = 0usize;
    for (name, netlist, negotiated) in cases {
        let candidate = if negotiated {
            plan_negotiated_on_schedule(
                &netlist,
                &PortPlacements::default(),
                NEGOTIATION_ROUNDS,
                PresentSchedule::SHIPPING,
            )
            .expect("negotiated full_adder routes")
        } else {
            planner::plan_from_netlist_within(
                &netlist,
                &PortPlacements::default(),
                planner::TRIAL_RIP_UP_ROUNDS,
            )
            .expect("the shipping plan reproduces")
        };
        let parts = planner::realise_without_verifying(
            &candidate,
            &netlist,
            candidate_world_size(&candidate),
        )
        .expect("it realises");
        let world = &parts.realised.world;
        let ports = &parts.realised.ports;
        let sources =
            every_source(&netlist, &ports.gate_output_positions, &ports.input_positions);

        // The repeaters and ramps, found the way genuine_decay finds them.
        let mut injections: Vec<(&'static str, Position, BlockState)> = Vec::new();
        for anchor in parts.reservation.keys() {
            let anchor = *anchor;
            let state = world.get(anchor.x, anchor.y, anchor.z);
            if state.kind != BlockKind::Repeater {
                continue;
            }
            injections.push(("deleted", anchor, BlockState::air()));
            injections.push(("dust", anchor, compile::dust()));
            let mut reversed = state.clone();
            reversed.facing = Some(reversed.facing.expect("a repeater has a facing").opposite());
            injections.push(("reversed", anchor, reversed));
            let Some(facing) = state.facing else { continue };
            let landing = anchor.offset(facing.opposite());
            let over = landing.up();
            if world.get(over.x, over.y, over.z).kind == BlockKind::RedstoneWire
                && parts.reservation.contains_key(&over)
            {
                injections.push(("riser to glass", landing, glass()));
            }
        }
        injections.sort_by_key(|&(shape, cell, _)| (cell.x, cell.y, cell.z, shape));

        for (shape, cell, replacement) in &injections {
            let mut injured = world.clone();
            injured.set(cell.x, cell.y, cell.z, replacement.clone());
            for &origin in &sources {
                let isolated = isolation_world(&injured, &sources, origin);
                let mut simulator = Simulator::new(isolated);
                simulator
                    .run_until_stable(MAX_TICKS)
                    .expect("an injured isolation world settles");
                settles += 1;
                let settled = simulator.world().clone();
                let result =
                    resettle_differential(&settled, MAX_TICKS).expect("the oracle settles");
                if result.diffs.is_empty() {
                    continue;
                }
                stale_worlds += 1;
                let stale_zeroes: Vec<&CellDiff> = result
                    .diffs
                    .iter()
                    .filter(|diff| diff.settled_power == 0 && diff.resettled_power > 0)
                    .collect();
                if !stale_zeroes.is_empty() {
                    stale_zero_worlds += 1;
                }
                eprintln!(
                    "{name}: `{shape}` at ({}, {}, {}), isolation of ({}, {}, {}): {} stale, {} stale-at-zero",
                    cell.x,
                    cell.y,
                    cell.z,
                    origin.x,
                    origin.y,
                    origin.z,
                    result.diffs.len(),
                    stale_zeroes.len()
                );
                for diff in &result.diffs {
                    eprintln!("{}", describe(&result.world, diff));
                }
            }
        }
    }
    eprintln!(
        "injected isolation worlds: {stale_worlds} of {settles} settles carry stale cells; \
         {stale_zero_worlds} contain a stale ZERO -- the reading genuine_decay counts a bite by"
    );
}

