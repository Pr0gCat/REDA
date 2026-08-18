//! The differential itself, run over every circuit the Stage 3 condition
//! names -- through the path that actually builds it -- plus the two cases in
//! dispute.
//!
//! Nothing here tunes anything. [`the_two_answers_agree_or_the_disagreement_is_named`]
//! asserts, and the rest print what they measured.

use std::collections::BTreeMap;

use super::*;
use crate::compile::planner::{
    self, candidate_world_size, plan_from_netlist_within, plan_negotiated_on_schedule,
    seed_from_legacy_parts, PlanCandidate, PortPlacements, PresentSchedule, NEGOTIATION_ROUNDS,
    TRIAL_RIP_UP_ROUNDS,
};
use crate::compile::{self, Netlist, PlannerKind};
use crate::redstone::simulator::position::ALL_SIX;

/// The negotiation schedule `segment_a` routes under.
///
/// Measured, not chosen here: on [`PresentSchedule::SHIPPING`] (iteration 0
/// free) `segment_a` does not converge within [`NEGOTIATION_ROUNDS`] and the
/// case reads `NOT MEASURED`; charging 8 for a shared cell on iteration 0 it
/// converges -- and produces a plan that does **not** compute `segment_a`.
/// `planner::negotiated_segment_a_routes_and_still_does_not_compute` is what
/// holds every clause of that sentence to being measured.
const SEGMENT_A_SCHEDULE: PresentSchedule = PresentSchedule::starting_at(8);

/// One circuit, and the plan whichever path builds it produced.
struct Case {
    name: &'static str,
    netlist: Netlist,
    candidate: PlanCandidate,
    /// How the plan was obtained, for the report.
    path: String,
}

fn lowered(name: &str, optimised: bool) -> Netlist {
    let circuit = crate::circuits::verilog::find(name).expect("the catalog has it");
    let (gate_level, _) = circuit.baked_netlist();
    if optimised {
        compile::lowering::lower_optimised(&gate_level)
    } else {
        compile::lowering::lower(&gate_level)
    }
    .expect("it lowers")
}

/// The plan `compile()` actually ships for this netlist.
///
/// Not a re-route with different settings: `compile` is run, its own
/// `planner_kind()` is read, and the candidate is rebuilt down whichever of the
/// two paths it reports -- the legacy seed out of the `LegacyEmission` it
/// carries, or `plan_from_netlist_within(TRIAL_RIP_UP_ROUNDS)`, which is the
/// exact call `compile` makes. `the_measured_plan_is_the_shipped_world` checks
/// the rebuild block for block against the world `compile` returned.
fn shipping_case(name: &'static str, netlist: Netlist) -> Case {
    let compiled = compile::compile(&netlist).unwrap_or_else(|error| {
        panic!("{name} must compile through the shipping path: {error}")
    });
    let (candidate, path) = match compiled.planner_kind() {
        PlannerKind::Legacy => {
            let emission = compiled
                .legacy_emission()
                .expect("a Legacy compile carries its emission");
            (
                seed_from_legacy_parts(&netlist, emission).expect("the legacy seed rebuilds"),
                "legacy seed".to_string(),
            )
        }
        PlannerKind::Unified3d => (
            plan_from_netlist_within(&netlist, &PortPlacements::default(), TRIAL_RIP_UP_ROUNDS)
                .expect("the planner path reproduces"),
            "planner, rip-up".to_string(),
        ),
    };
    Case {
        name,
        netlist,
        candidate,
        path,
    }
}

/// A plan from the negotiated router -- the two cases in dispute.
///
/// The schedule is named rather than assumed: `segment_a` routes only under
/// [`SEGMENT_A_SCHEDULE`], while `plan_from_netlist_with_router` always uses
/// [`PresentSchedule::SHIPPING`], so a case that does not say which of the two
/// it ran under is not reproducible.
fn negotiated_case(
    name: &'static str,
    netlist: Netlist,
    schedule: PresentSchedule,
) -> Option<Case> {
    let candidate = plan_negotiated_on_schedule(
        &netlist,
        &PortPlacements::default(),
        NEGOTIATION_ROUNDS,
        schedule,
    )
    .ok()?;
    Some(Case {
        name,
        netlist,
        candidate,
        path: format!("planner, negotiated, {schedule:?}"),
    })
}

/// Run the differential against one plan.
fn measure_case(case: &Case) -> Measurement {
    let size = candidate_world_size(&case.candidate);
    let parts = planner::realise_without_verifying(&case.candidate, &case.netlist, size)
        .unwrap_or_else(|error| panic!("{} must realise: {error}", case.name));
    measure(
        &parts.realised.world,
        &parts.reservation,
        &case.netlist,
        &parts.nets,
        &parts.realised.ports.gate_output_positions,
        &parts.realised.ports.input_positions,
        &parts.realised.ports.output_positions,
    )
    .unwrap_or_else(|error| panic!("{} must measure: {error}", case.name))
}

fn one_line(reading: &Reading, group: &GroupReport, direction: Direction) -> String {
    format!(
        "    {:?}{} {:?} ({}, {}, {}) {:?} walk={} solo={} live={}{} control={} nets=[{}]",
        direction,
        if reading.is_sink { " **SINK**" } else { "" },
        reading.class,
        reading.cell.x,
        reading.cell.y,
        reading.cell.z,
        reading.kind,
        reading.walk,
        reading.solo,
        reading.live,
        if reading.live_strong { "(strong)" } else { "" },
        reading.control,
        group.nets.join(", "),
    )
}

fn describe(measurement: &Measurement) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "  walk verdict (replica) : {}",
        measurement
            .replica_verdict
            .clone()
            .unwrap_or_else(|| "passes".to_string())
    ));
    lines.push(format!(
        "  walk verdict (shipping): {}",
        measurement
            .shipping_verdict
            .clone()
            .unwrap_or_else(|| "passes".to_string())
    ));
    lines.push(format!(
        "  groups {}, cells compared {}, floors reported {}, settles {}, walk records into air {}",
        measurement.groups.len(),
        measurement.cells_compared(),
        measurement.floors_reported(),
        measurement.settles,
        measurement.recorded_into_air(),
    ));

    let count = |rows: &[(&GroupReport, Direction, &Reading)], want: Direction| {
        rows.iter().filter(|(_, d, _)| *d == want).count()
    };
    let decisive = measurement.decisive();
    let all = measurement.disagreements();
    lines.push(format!(
        "  DECISIVE (a cell the judge reads): false refusals {}, FALSE PASSES {}",
        count(&decisive, Direction::FalseRefusal),
        count(&decisive, Direction::FalsePass),
    ));
    lines.push(format!(
        "  diagnostic (every other compared cell): walk-0/sim-delivers {}, walk-N/sim-silent {}",
        count(&all, Direction::FalseRefusal) - count(&decisive, Direction::FalseRefusal),
        count(&all, Direction::FalsePass) - count(&decisive, Direction::FalsePass),
    ));

    for (group, direction, reading) in &decisive {
        lines.push(one_line(reading, group, *direction));
    }

    // Diagnostics, folded: the walk-records-into-air class is one shape and is
    // printed as a count, everything else is printed cell by cell.
    let mut folded = 0usize;
    for (group, direction, reading) in &all {
        if reading.is_sink {
            continue;
        }
        if reading.kind == BlockKind::Air {
            folded += 1;
            continue;
        }
        lines.push(one_line(reading, group, *direction));
    }
    if folded > 0 {
        lines.push(format!(
            "    (+{folded} diagnostic cells where the walk recorded into AIR --              a dead end its own comment documents recording and nothing ever reads)"
        ));
    }

    for note in &measurement.unmeasured {
        lines.push(format!("    NOT MEASURED: {note}"));
    }
    lines.join("
")
}

/// **The assertion.** The replica walk and the shipping judge must reach the
/// same verdict on every plan measured, or the whole differential is measuring
/// something other than the judge.
///
/// Rule 2: injecting any change into the replica's grouping (dropping the
/// merge-body cells, seeding a merge-sourced net, using per-net cells instead
/// of per-group) turns this red on the circuits that have merges.
#[test]
fn the_replica_reaches_the_shipping_verdict() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;

    for case in [
        shipping_case("and4", build_and4_netlist().0),
        shipping_case("full_adder", build_full_adder_netlist().0),
    ] {
        let measurement = measure_case(&case);
        assert_eq!(
            measurement.replica_verdict, measurement.shipping_verdict,
            "{}: the replica walk and `verify_signal_strength` must agree",
            case.name
        );
    }
}

/// **The finding, and its repair, pinned in one place.**
///
/// Negotiated `full_adder` -- the plan
/// `planner::the_strength_verifier_follows_a_repeater_that_feeds_a_climb`
/// pins as computing `full_adder` on all eight vectors in the real `Simulator`
/// -- used to be refused by `verify_signal_strength` at `(57, 1, 91)`, a cell
/// the simulator reads **9** with `cin`'s own lever the only component emitting
/// in the whole world. Two further gate inputs the judge never got as far as
/// naming were refused the same way, `(40, 1, 125)` at 15 and `(60, 1, 83)`
/// at 6.
///
/// Now: the plan passes, and this asserts the thing that makes that safe --
/// **on this plan the walk and the simulator agree at every cell, in both
/// directions**, not merely that the refusal went away. Measured 2026-08-18:
/// 558 cells compared, 0 disagreements, 0 of them at a sink.
///
/// **Rule 2, confirmed by injection.** Restoring the old enqueue rule --
/// `deliver` walking onward only from a cell in `own_cells`, whatever stands
/// there and whatever arrived -- brings the refusal back and turns this red at
/// three sinks and 68 conductors.
#[test]
fn the_judge_agrees_with_the_simulator_on_the_plan_it_used_to_refuse() {
    use crate::circuits::full_adder::build_full_adder_netlist;

    let Some(case) = negotiated_case(
        "full_adder [NEGOTIATED]",
        build_full_adder_netlist().0,
        PresentSchedule::SHIPPING,
    ) else {
        panic!("negotiated full_adder must route -- it is the case in dispute");
    };
    let measurement = measure_case(&case);

    assert!(
        measurement.shipping_verdict.is_none(),
        "`verify_signal_strength` must no longer refuse this plan: {:?}",
        measurement.shipping_verdict
    );

    let cell = Position::new(57, 1, 91);
    let named = measurement
        .groups
        .iter()
        .flat_map(|group| group.readings.iter())
        .find(|reading| reading.cell == cell && reading.is_sink)
        .expect("(57, 1, 91) is still a gate support this plan's `cin` feeds");
    assert!(
        named.solo > 0 && named.walk > 0,
        "the cell the old refusal named must now be reached by both answers, \
         got walk={} solo={}",
        named.walk,
        named.solo
    );

    let disagreements: Vec<String> = measurement
        .disagreements()
        .into_iter()
        .map(|(group, direction, reading)| one_line(reading, group, direction))
        .collect();
    assert!(
        disagreements.is_empty(),
        "{} cells compared and the two answers must agree on every one of them:\n{}",
        measurement.cells_compared(),
        disagreements.join("\n"),
    );
}

/// **The negative control, and it is what gives the false-pass direction any
/// power at all.**
///
/// Measured first, and it is the reason this test exists: on a *working* plan
/// the false-pass direction is unfalsifiable. Every gate input of a circuit that
/// computes its function is genuinely delivered to, so no over-report at a sink
/// can be observed there however loose the walk is made. Injecting the loosest
/// walk expressible -- `deliver` enqueueing **every** target, dropping the
/// `own_cells` gate entirely -- leaves all seven measured plans at *zero*
/// decisive false passes and only inflates the diagnostic counts (and4 0 -> 38,
/// seven_segment 0 -> 875). A test reading "no false pass on the shipping
/// circuits" would therefore have passed against an arbitrarily broken judge.
///
/// (That injection *does* bite once the world is damaged rather than merely
/// re-judged, which is what [`genuine_decay_is_still_refused`] measures and
/// this control's own second cut anticipates -- the two tests are the same
/// idea applied to two different kinds of damage.)
///
/// So the control manufactures the one thing a working circuit does not have:
/// **a gate input its own net genuinely cannot reach.** Two cuts per sink, both
/// made in the realised world so the walk and the simulator see the same
/// damage:
///
/// 1. **the terminal** -- the group's own conductor cells touching the support;
/// 2. **the terminal's feeder** -- the cells touching *those*, leaving the
///    terminal itself standing but unfed. This is the sharper of the two: the
///    repeater is still there, still pointed at the support, and the only thing
///    that says it is silent is whether its own input ever arrived -- which is
///    the exact distinction `net_signal_strength`'s doc comment says separates
///    it from `net_reach`.
///
/// The claim asserted is: **wherever a cut leaves the group unable to deliver in
/// the simulator, the walk must record zero there and the judge must refuse.**
/// Cuts that do not sever delivery (a fanout with another path) prove nothing
/// and are skipped -- and the test fails if too few cuts bite, so the control
/// cannot go vacuous.
///
/// **Rule 2, confirmed by injection.** Seeding `net_signal_strength` from every
/// repeater in `own_cells` as well as from the real origins -- "a repeater is
/// assumed to fire", the defect the function's own comment exists to rule out --
/// turns the feeder cut red with `**FALSE PASS** -- the walk still records 15`.
#[test]
fn a_gate_input_its_own_net_cannot_reach_is_refused() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;

    let mut bit = 0usize;
    for case in [
        shipping_case("and4", build_and4_netlist().0),
        shipping_case("full_adder", build_full_adder_netlist().0),
    ] {
        let size = candidate_world_size(&case.candidate);
        let parts = planner::realise_without_verifying(&case.candidate, &case.netlist, size)
            .expect("it realises");
        let world = &parts.realised.world;
        let ports = &parts.realised.ports;
        let run = |w: &crate::redstone::world::storage::World| {
            measure(
                w,
                &parts.reservation,
                &case.netlist,
                &parts.nets,
                &ports.gate_output_positions,
                &ports.input_positions,
                &ports.output_positions,
            )
            .expect("it measures")
        };

        let baseline = run(world);
        assert!(
            baseline.shipping_verdict.is_none(),
            "{}: the plan must pass as built, or the control proves nothing: {:?}",
            case.name,
            baseline.shipping_verdict
        );

        let walks = walk_by_group(
            world,
            &parts.reservation,
            &case.netlist,
            &parts.nets,
            &ports.gate_output_positions,
            &ports.input_positions,
        );

        for group in &baseline.groups {
            let walk = walks
                .iter()
                .find(|walk| walk.root == group.root)
                .expect("every reported group has a walk");
            let own_touching = |cell: Position| -> Vec<Position> {
                ALL_SIX
                    .into_iter()
                    .map(|direction| cell.offset(direction))
                    .filter(|neighbour| walk.cells.contains(neighbour))
                    .collect()
            };

            for reading in group.readings.iter().filter(|reading| reading.is_sink) {
                let terminal = own_touching(reading.cell);
                if terminal.is_empty() {
                    continue;
                }
                let feeder: Vec<Position> = terminal
                    .iter()
                    .flat_map(|cell| own_touching(*cell))
                    .filter(|cell| !terminal.contains(cell))
                    .collect();

                for (what, cut_cells) in [("terminal", &terminal), ("feeder", &feeder)] {
                    if cut_cells.is_empty() {
                        continue;
                    }
                    let mut cut = world.clone();
                    for cell in cut_cells.iter() {
                        cut.set(cell.x, cell.y, cell.z, BlockState::air());
                    }
                    let severed = run(&cut);
                    let after = severed
                        .groups
                        .iter()
                        .find(|other| other.root == group.root)
                        .and_then(|other| {
                            other.readings.iter().find(|other| other.cell == reading.cell)
                        })
                        .expect("the same support is still read after the cut");

                    // A cut that did not sever delivery says nothing either way.
                    if after.solo > 0 {
                        continue;
                    }
                    bit += 1;

                    assert_eq!(
                        after.walk,
                        0,
                        "{}: **FALSE PASS** -- with the {what} cut ({cut_cells:?}) the simulator \
                         delivers nothing from nets [{}] to ({}, {}, {}), and the walk still \
                         records {}",
                        case.name,
                        group.nets.join(", "),
                        reading.cell.x,
                        reading.cell.y,
                        reading.cell.z,
                        after.walk,
                    );
                    assert!(
                        severed
                            .shipping_verdict
                            .as_deref()
                            .is_some_and(|verdict| verdict.contains("signal-strength violation")),
                        "{}: the judge must refuse a plan whose {what} into ({}, {}, {}) is gone, \
                         got {:?}",
                        case.name,
                        reading.cell.x,
                        reading.cell.y,
                        reading.cell.z,
                        severed.shipping_verdict,
                    );
                }
            }
        }
    }
    assert!(
        bit >= 8,
        "only {bit} cuts actually severed delivery -- the control has gone vacuous"
    );
}

/// What the false-pass direction reads on the plans as built, reported rather
/// than asserted.
///
/// Zero on all seven, and by the control above that is **not** evidence the
/// walk is sound -- only that nothing in these plans gives it an opportunity to
/// be caught. The number is here because its absence would be a finding.
#[test]
fn the_false_pass_count_on_the_plans_as_built() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;

    for case in [
        shipping_case("and4", build_and4_netlist().0),
        shipping_case("full_adder", build_full_adder_netlist().0),
        shipping_case("verilog:and4", lowered("verilog:and4", false)),
    ] {
        let measurement = measure_case(&case);
        let passes: Vec<String> = measurement
            .decisive()
            .into_iter()
            .filter(|(_, direction, _)| *direction == Direction::FalsePass)
            .map(|(group, direction, reading)| one_line(reading, group, direction))
            .collect();
        assert!(
            passes.is_empty(),
            "{}: the walk claims a signal at a gate input the net cannot deliver to:\n{}",
            case.name,
            passes.join("\n")
        );
    }
}

/// The whole differential, printed. Every circuit the shipping router lays
/// today, plus the two the negotiated router lays.
///
/// Asserts nothing except that the replica tracks the judge -- the numbers are
/// the result. `--ignored --nocapture`. As of 2026-08-18, with the repair in:
///
/// ```text
/// circuit                path                    cells  dec FR  dec FP  diag FR  air
/// and4                   planner, rip-up           114     0       0       0      0
/// full_adder             planner, rip-up           540     0       0       0      0
/// verilog:and4           planner, rip-up           143     0       0       0      0
/// segment_a              legacy seed              3440     0       0       1      0
/// seven_segment          legacy seed              8601     0       0       0      0
/// verilog:seven_segment  legacy seed              5134     0       0       0      0
/// full_adder  NEGOTIATED planner, negotiated, 0    558     0       0       0      0
/// segment_a   NEGOTIATED planner, negotiated, 8   1566     6       0     169      0
/// ```
///
/// The one diagnostic on shipping `segment_a` is `(68, 1, 48)`, a cross-net
/// energisation -- see
/// [`the_one_shipping_circuit_cell_the_two_answers_differ_on`].
///
/// The six on negotiated `segment_a` are **not** a reason to widen further:
/// that plan contains a closed repeater ring in `g0` that comes up latched and
/// is driven by no source at all, and it computes 8 of 16 vectors wrong. See
/// `planner::negotiated_segment_a_routes_and_still_does_not_compute`, which is
/// where that is measured rather than asserted here.
#[test]
#[ignore = "measurement harness: compiles, routes and settles six circuits plus two disputed plans"]
fn the_walk_against_the_simulator_cell_by_cell() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
    use std::time::Instant;

    let mut cases: Vec<Case> = vec![
        shipping_case("and4", build_and4_netlist().0),
        shipping_case("full_adder", build_full_adder_netlist().0),
        shipping_case("verilog:and4", lowered("verilog:and4", false)),
        shipping_case("segment_a", build_single_segment_netlist(0).0),
        shipping_case("seven_segment", build_seven_segment_netlist().0),
        shipping_case(
            "verilog:seven_segment",
            lowered("verilog:seven_segment", true),
        ),
    ];
    if let Some(case) = negotiated_case(
        "full_adder [NEGOTIATED]",
        build_full_adder_netlist().0,
        PresentSchedule::SHIPPING,
    ) {
        cases.push(case);
    } else {
        eprintln!("NOT MEASURED: negotiated full_adder did not route");
    }
    if let Some(case) = negotiated_case(
        "segment_a [NEGOTIATED]",
        build_single_segment_netlist(0).0,
        SEGMENT_A_SCHEDULE,
    )
    {
        cases.push(case);
    } else {
        eprintln!("NOT MEASURED: negotiated segment_a did not route");
    }

    for case in &cases {
        let started = Instant::now();
        let measurement = measure_case(case);
        eprintln!(
            "{} [{}] {:.1}s\n{}",
            case.name,
            case.path,
            started.elapsed().as_secs_f64(),
            describe(&measurement)
        );
        assert_eq!(
            measurement.replica_verdict, measurement.shipping_verdict,
            "{}: the replica walk and `verify_signal_strength` must agree",
            case.name
        );
    }
}

/// The step relation the walk implements, laid against
/// `docs/derived/coupling-mechanisms.md`'s numbered mechanisms.
///
/// Not an argument about the code: every row's verdict is attached to a cell
/// the differential above actually measured, or is marked NOT MEASURED.
#[test]
#[ignore = "measurement harness: prints the step map"]
fn which_measured_couplings_the_walk_models() {
    let rows: [(&str, &str, &str); 8] = [
        (
            "1 dust <-> dust",
            "MODELLED",
            "`dust_connections` per horizontal direction, one hop of decay, taken \
             forward only -- which is the right direction for propagation even though \
             `verify_connectivity` needs both. Measured: on every plan but the \
             negotiated one, the walk's value equals the simulator's on every dust \
             cell of every net, decay step for decay step (e.g. segment_a's `g16`, \
             15,3,4,..,15 through two repeaters, walk == solo on all 47 cells).",
        ),
        (
            "2 component -> adjacent dust",
            "MODELLED",
            "`structural_output` from each seeded origin, and again from any repeater \
             the walk has established is fed. Measured wherever a route's first cell \
             reads 15 in both answers.",
        ),
        (
            "3 component -> strongly powered block -> dust on every face",
            "MODELLED (was unreachable; fixed 2026-08-18)",
            "the radiate-from-a-conductive-block arm covers ALL_SIX and now runs. It \
             used not to: it only fires for a cell that was ENQUEUED, and `deliver` \
             enqueued only `own_cells`, which holds route ANCHORS -- no route anchor \
             ever holds a conductive block, and a route's floor is not an anchor, so \
             the arm never fired. **That was the whole defect.** `deliver` now decides \
             from what stands at the target and which of `PowerOutput`'s two channels \
             arrived. Measured on negotiated `full_adder`: repeater (55,1,108) drives \
             the floor (55,1,107), the dust standing on it at (55,2,107) reads 15 with \
             `cin`'s lever the only thing emitting in the world, and the walk now reads \
             15 too -- as do the 24 cells past it and the support (57,1,91) the refusal \
             used to name. 558 cells compared on that plan, zero disagreements.",
        ),
        (
            "4 weak power -> torch support / diode rear",
            "PARTIAL",
            "the dust-into-block arm records the arriving strength without \
             distinguishing weak from strong, which is right at a SINK because \
             `torch_should_be_lit` fires on either, and it now loops ALL_SIX through \
             the shared `structural_output_in_world`, so `dust_powers_block_toward`'s \
             always-true `Down` case -- the block a run stands on -- is recorded where \
             it used to be invisible. Still missing: `propagate::diode_rear_signal`, a \
             diode reading its rear *block*, has no arm at all. NOT MEASURED: whether \
             any gate support in any of these circuits is read by a diode through a \
             block; none of the seven plans measured showed a disagreement needing one.",
        ),
        (
            "5 weak power -> dust",
            "CORRECTLY ABSENT",
            "does not exist in the simulator; the dust-into-block arm deliberately \
             refuses to enqueue, with that reason in its own comment.",
        ),
        (
            "6 block -> block",
            "CORRECTLY ABSENT",
            "does not exist in the simulator; the walk never conducts block to block.",
        ),
        (
            "7 torch -> its own support",
            "CORRECTLY ABSENT",
            "`structural_output` carries the withheld direction, `Down` for a standing \
             torch and `facing.opposite()` for a wall torch.",
        ),
        (
            "8 quasi-connectivity",
            "NOT MEASURED",
            "the simulator has no such edge, so nothing here can say whether a realised \
             world contains one.",
        ),
    ];
    for (mechanism, verdict, note) in rows {
        eprintln!("{mechanism}\n  {verdict}\n  {note}");
    }

    eprintln!(
        "\nSTEPS THE TABLE DOES NOT SUPPORT (the false-pass side of the same question)\n  \
         None, on any of the seven plans. The walk used to record a value at cells \
         holding AIR -- 40 on and4, 92 on full_adder, 188 on segment_a, 324 on \
         seven_segment -- every one a seed or a repeater output landing on nothing. \
         `deliver` now refuses a delivery the target cannot receive at all, so that \
         class is 0 everywhere and `Measurement::recorded_into_air` reads 0 on every \
         case. No other over-record was found."
    );
}

/// The world the differential measured is the world `compile` shipped.
///
/// Without this the whole harness could be measuring a re-route that happens to
/// look like the shipping one.
#[test]
fn the_measured_plan_is_the_shipped_world() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;

    for (name, netlist) in [
        ("and4", build_and4_netlist().0),
        ("full_adder", build_full_adder_netlist().0),
    ] {
        let compiled = compile::compile(&netlist).expect("it compiles");
        let case = shipping_case(name, netlist.clone());
        let size = candidate_world_size(&case.candidate);
        let parts = planner::realise_without_verifying(&case.candidate, &case.netlist, size)
            .expect("it realises");
        let (sx, sy, sz) = compiled.world.size();
        assert_eq!(parts.realised.world.size(), (sx, sy, sz), "{name}: same size");
        for x in 0..sx {
            for y in 0..sy {
                for z in 0..sz {
                    assert_eq!(
                        parts.realised.world.get(x, y, z).kind,
                        compiled.world.get(x, y, z).kind,
                        "{name}: ({x}, {y}, {z}) differs from the shipped world"
                    );
                }
            }
        }
    }
}

/// A sanity floor under [`observed`]: the reading of a cell is the simulator's
/// own, not a restatement.
#[test]
fn observed_reads_what_the_simulator_left_behind() {
    use crate::circuits::and4::build_and4_netlist;

    let (netlist, _) = build_and4_netlist();
    let compiled = compile::compile(&netlist).expect("and4 compiles");
    let mut world = compiled.world.clone();
    for &(x, y, z) in compiled.input_positions.values() {
        let mut state = world.get(x, y, z).clone();
        state.lit = true;
        world.set(x, y, z, state);
    }
    let mut simulator = Simulator::new(world);
    simulator.run_until_stable(MAX_TICKS).expect("and4 settles");
    let settled = simulator.world().clone();

    let mut dust_cells = 0usize;
    let mut lit_dust = 0usize;
    let (sx, sy, sz) = settled.size();
    for x in 0..sx {
        for y in 0..sy {
            for z in 0..sz {
                let position = Position::new(x, y, z);
                let state = settled.get(x, y, z);
                if state.kind == BlockKind::RedstoneWire {
                    dust_cells += 1;
                    assert_eq!(
                        observed(&settled, position),
                        state.power.max(if state.power > 0 { 1 } else { 0 }),
                        "dust at ({x}, {y}, {z}) must read its own power"
                    );
                    if state.power > 0 {
                        lit_dust += 1;
                    }
                }
                if state.kind == BlockKind::Air {
                    assert_eq!(
                        observed(&settled, position),
                        0,
                        "air at ({x}, {y}, {z}) carries nothing"
                    );
                }
            }
        }
    }
    assert!(dust_cells > 0 && lit_dust > 0, "and4 has live dust");
    let _: BTreeMap<String, (i32, i32, i32)> = compiled.input_positions.clone();
}

/// The neighbourhood of one cell, in the world the plan realised -- so a
/// disagreement can be attributed to a *geometry* rather than to a coordinate.
///
/// Prints the 3x3x3 box around the cell, plus what the walk and the simulator
/// each say about it.
#[test]
#[ignore = "measurement harness: dumps the geometry around the cells the differential named"]
fn the_geometry_under_every_named_disagreement() {
    use crate::circuits::full_adder::build_full_adder_netlist;
    use crate::circuits::seven_segment::build_single_segment_netlist;

    let mut cases: Vec<Case> = vec![shipping_case("segment_a", build_single_segment_netlist(0).0)];
    if let Some(case) = negotiated_case(
        "full_adder [NEGOTIATED]",
        build_full_adder_netlist().0,
        PresentSchedule::SHIPPING,
    ) {
        cases.push(case);
    }
    if let Some(case) = negotiated_case(
        "segment_a [NEGOTIATED]",
        build_single_segment_netlist(0).0,
        SEGMENT_A_SCHEDULE,
    )
    {
        cases.push(case);
    }

    for case in &cases {
        let size = candidate_world_size(&case.candidate);
        let parts = planner::realise_without_verifying(&case.candidate, &case.netlist, size)
            .expect("it realises");
        let world = &parts.realised.world;
        let measurement = measure_case(case);
        eprintln!("==== {} [{}]", case.name, case.path);
        for (group, direction, reading) in measurement.disagreements() {
            eprintln!(
                "{:?}{} ({}, {}, {}) {:?} walk={} solo={} from origin {:?} nets=[{}]",
                direction,
                if reading.is_sink { " **SINK**" } else { "" },
                reading.cell.x,
                reading.cell.y,
                reading.cell.z,
                reading.kind,
                reading.walk,
                reading.solo,
                reading.solo_origin,
                group.nets.join(", "),
            );
            for dy in [1i32, 0, -1] {
                for dz in [-1i32, 0, 1] {
                    let row: Vec<String> = (-1i32..=1)
                        .map(|dx| {
                            let c = Position::new(
                                reading.cell.x + dx,
                                reading.cell.y + dy,
                                reading.cell.z + dz,
                            );
                            let state = world.get(c.x, c.y, c.z);
                            let mark = match state.kind {
                                BlockKind::Air => ".".to_string(),
                                BlockKind::RedstoneWire => "w".to_string(),
                                BlockKind::Repeater => {
                                    format!("R{:?}", state.facing.map(|f| format!("{f:?}")))
                                }
                                BlockKind::Solid => "#".to_string(),
                                BlockKind::Torch => "T".to_string(),
                                BlockKind::WallTorch => "t".to_string(),
                                BlockKind::Lever => "L".to_string(),
                                BlockKind::Lamp => "O".to_string(),
                                other => format!("{other:?}"),
                            };
                            format!("{mark:>10}")
                        })
                        .collect();
                    eprintln!("   dy{dy:+} dz{dz:+} | {}", row.join(" "));
                }
            }
            // Sinks are what decide a verdict; the rest would drown it.
            if !reading.is_sink {
                break;
            }
        }
    }
}

/// The one cell in a **shipping** circuit where the two answers differ, walked
/// out along its own run so the discrepancy is a mechanism and not a
/// coordinate.
///
/// `segment_a` through the legacy seed: `(68, 1, 48)` reads 10 in the simulator
/// with `g16`'s own torch the only thing emitting, and the walk records
/// nothing. It changes no verdict -- the cell is not a sink and the plan passes
/// -- and it is **not** the climb defect: it survived the repair unchanged, and
/// it is not a gap in this walk at all. Its north and south neighbours
/// `(68, 1, 47)`/`(68, 1, 49)` hold `g19`'s dust and read 11 and 9 with only
/// `g16` emitting, with genuine bidirectional `dust_connections` edges both
/// ways. That is a **cross-net energisation** -- `compile::coupling`'s
/// question, not this one -- and a walk whose whole job is "does *this* net
/// deliver" is right to refuse to follow it.
#[test]
#[ignore = "measurement harness: prints one net's whole run, both answers per cell"]
fn the_one_shipping_circuit_cell_the_two_answers_differ_on() {
    use crate::circuits::seven_segment::build_single_segment_netlist;

    let case = shipping_case("segment_a", build_single_segment_netlist(0).0);
    let size = candidate_world_size(&case.candidate);
    let parts = planner::realise_without_verifying(&case.candidate, &case.netlist, size)
        .expect("it realises");
    let ports = &parts.realised.ports;
    let measurement = measure(
        &parts.realised.world,
        &parts.reservation,
        &case.netlist,
        &parts.nets,
        &ports.gate_output_positions,
        &ports.input_positions,
        &ports.output_positions,
    )
    .expect("it measures");

    for group in &measurement.groups {
        if !group.nets.iter().any(|net| net == "g16") {
            continue;
        }
        eprintln!("group {} nets [{}]", group.root, group.nets.join(", "));
        eprintln!("origins {:?}", group.origins);
        let mut rows: Vec<&Reading> = group
            .readings
            .iter()
            .filter(|reading| reading.class != CellClass::Floor)
            .collect();
        rows.sort_by_key(|reading| (reading.cell.x, reading.cell.z, reading.cell.y));
        for reading in rows {
            eprintln!(
                "  ({:>3},{:>2},{:>4}) {:?}{} walk={:>2} solo={:>2} live={:>2}{}",
                reading.cell.x,
                reading.cell.y,
                reading.cell.z,
                reading.kind,
                if reading.is_sink { " SINK" } else { "" },
                reading.walk,
                reading.solo,
                reading.live,
                if reading.walk == 0 && reading.solo > 0 { "   <-- DIFFERS" } else { "" },
            );
            if reading.walk == 0 && reading.solo > 0 {
                // Who owns everything touching it, and what does g16 alone put
                // there? A neighbour owned by a *different* net that carries
                // g16's signal is an extra edge (`compile::coupling`'s
                // question), not a gap in this walk.
                let solo_world = isolation_world(
                    &parts.realised.world,
                    &every_source(
                        &case.netlist,
                        &ports.gate_output_positions,
                        &ports.input_positions,
                    ),
                    group.origins[0].0,
                );
                let solo_world = settle(solo_world, "the isolation world").expect("settles");
                for direction in ALL_SIX {
                    let neighbour = reading.cell.offset(direction);
                    let owner = parts
                        .reservation
                        .get(&neighbour)
                        .map(|&net| net_name_for(&case.netlist, &parts.nets, net))
                        .unwrap_or_else(|| "unclaimed".to_string());
                    eprintln!(
                        "      {direction:?} ({}, {}, {}) {:?} owner={owner} solo={}",
                        neighbour.x,
                        neighbour.y,
                        neighbour.z,
                        solo_world.get(neighbour.x, neighbour.y, neighbour.z).kind,
                        observed(&solo_world, neighbour),
                    );
                }
                // Is the join the simulator is using a *dust* edge -- the one
                // relation `verify_connectivity` walks -- or a block-mediated
                // one it does not?
                for direction in crate::redstone::simulator::position::HORIZONTAL {
                    let out = crate::redstone::simulator::connectivity::dust_connections(
                        &parts.realised.world,
                        reading.cell,
                        direction,
                    );
                    let back: Vec<Position> = crate::redstone::simulator::connectivity::
                        dust_connections(
                            &parts.realised.world,
                            reading.cell.offset(direction),
                            direction.opposite(),
                        )
                        .iter()
                        .collect();
                    eprintln!(
                        "      dust_connections {direction:?}: out {:?} back {:?}",
                        out.iter().collect::<Vec<_>>(),
                        back,
                    );
                }
            }
        }
    }
}

/// **NO_GO criterion 2, and the whole reason a looser judge needs its own
/// brief.** `net_signal_strength` was widened -- mechanism 3 was written into
/// it and unreachable, and now it fires -- so the question that decides whether
/// that was safe is not "is the suite green" but **does it still refuse a run
/// that really does decay to nothing.**
///
/// Four shapes, injected into the *realised world* so the walk and the
/// simulator see exactly the same damage, each re-measured through the same
/// per-origin isolation the differential uses:
///
/// 1. **a repeater deleted** from a run -- the run is severed outright;
/// 2. **a repeater replaced by plain dust** -- the wire stays continuous, so
///    `verify_connectivity` still passes, and the run is now longer than the
///    strength budget and decays out. This is the invariant's own reason for
///    existing, and it is the shape "lengthen a run past 15" takes on a plan
///    whose router already refuses to lay one that long;
/// 3. **the refresh before a climb deleted** -- the exact geometry the widening
///    opened. The repeater that fed the ramp is gone; the ramp itself is
///    untouched. Deleted rather than replaced by dust because replacing it
///    measurably does **not** sever: on all three plans here, every run feeding
///    a ramp is short enough to survive unrefreshed, so that variant never bit
///    once and asserting on it would have been asserting on nothing;
/// 4. **the riser under a climb turned to glass** -- the ramp's own conductor,
///    made non-conductive. Mechanism 3 needs the block to conduct
///    (`propagate::block_signal_at`'s own first gate) and `deliver` carries that
///    requirement; this is what says the widened arm respects it rather than
///    radiating from whatever a repeater happens to point at.
///
/// The claim asserted, per injection: **wherever the damage leaves a group
/// unable to deliver to one of its own gate supports in the simulator, the walk
/// must record zero there and the judge must refuse.** An injection that does
/// not sever delivery -- a fanout with another path, a run short enough to
/// survive unrefreshed -- proves nothing and is skipped, and the test fails if
/// any of the four shapes never bit, so no row here is a claim with no
/// measurement under it.
#[test]
fn genuine_decay_is_still_refused() {
    use crate::circuits::and4::build_and4_netlist;
    use crate::circuits::full_adder::build_full_adder_netlist;

    let cases: Vec<Case> = vec![
        shipping_case("and4", build_and4_netlist().0),
        shipping_case("full_adder", build_full_adder_netlist().0),
        negotiated_case(
            "full_adder [NEGOTIATED]",
            build_full_adder_netlist().0,
            PresentSchedule::SHIPPING,
        )
        .expect("negotiated full_adder routes -- it is the plan the fix is about"),
    ];

    let mut bit: BTreeMap<&'static str, usize> = BTreeMap::new();
    for case in &cases {
        let size = candidate_world_size(&case.candidate);
        let parts = planner::realise_without_verifying(&case.candidate, &case.netlist, size)
            .expect("it realises");
        let world = &parts.realised.world;
        let ports = &parts.realised.ports;
        let run = |w: &World| {
            measure(
                w,
                &parts.reservation,
                &case.netlist,
                &parts.nets,
                &ports.gate_output_positions,
                &ports.input_positions,
                &ports.output_positions,
            )
            .expect("it measures")
        };

        let baseline = run(world);
        assert!(
            baseline.shipping_verdict.is_none(),
            "{}: the plan must pass as built, or an injection proves nothing: {:?}",
            case.name,
            baseline.shipping_verdict
        );

        // Every repeater this plan laid, and -- separately -- every one whose
        // output lands on a conductive block carrying this plan's own dust on
        // its top face, which is the ramp the widening opened.
        let mut repeaters: Vec<Position> = Vec::new();
        let mut ramps: Vec<(Position, Position)> = Vec::new();
        for anchor in parts.reservation.keys() {
            let anchor = *anchor;
            let state = world.get(anchor.x, anchor.y, anchor.z);
            if state.kind != BlockKind::Repeater {
                continue;
            }
            repeaters.push(anchor);
            let Some(facing) = state.facing else {
                continue;
            };
            let landing = anchor.offset(facing.opposite());
            let over = landing.up();
            if world.get(over.x, over.y, over.z).kind == BlockKind::RedstoneWire
                && parts.reservation.contains_key(&over)
            {
                ramps.push((anchor, landing));
            }
        }
        repeaters.sort_by_key(|cell| (cell.x, cell.y, cell.z));
        ramps.sort_by_key(|(cell, _)| (cell.x, cell.y, cell.z));
        assert!(
            !repeaters.is_empty(),
            "{}: no repeater to injure -- the harness would be vacuous",
            case.name
        );

        let mut injections: Vec<(&'static str, Position, BlockState)> = Vec::new();
        for cell in &repeaters {
            injections.push((DELETED, *cell, BlockState::air()));
            injections.push((UNREFRESHED, *cell, compile::dust()));
        }
        for (repeater, riser) in &ramps {
            injections.push((RAMP_CUT, *repeater, BlockState::air()));
            injections.push((RISER_INSULATED, *riser, glass()));
        }

        for (shape, cell, replacement) in injections {
            let mut broken = world.clone();
            broken.set(cell.x, cell.y, cell.z, replacement);
            let after = run(&broken);

            for group in &after.groups {
                for reading in group.readings.iter().filter(|reading| reading.is_sink) {
                    // Only a support the group *used* to deliver to and now
                    // cannot is a case the judge owes an answer for.
                    let delivered_before = baseline
                        .groups
                        .iter()
                        .find(|other| other.root == group.root)
                        .and_then(|other| {
                            other.readings.iter().find(|other| other.cell == reading.cell)
                        })
                        .is_some_and(|before| before.solo > 0);
                    if reading.solo > 0 || !delivered_before {
                        continue;
                    }
                    *bit.entry(shape).or_default() += 1;

                    assert_eq!(
                        reading.walk,
                        0,
                        "{}: **FALSE PASS** -- with `{shape}` at ({}, {}, {}) the simulator \
                         delivers nothing from nets [{}] to gate support ({}, {}, {}), and the \
                         walk still records {}",
                        case.name,
                        cell.x,
                        cell.y,
                        cell.z,
                        group.nets.join(", "),
                        reading.cell.x,
                        reading.cell.y,
                        reading.cell.z,
                        reading.walk,
                    );
                    assert!(
                        after
                            .shipping_verdict
                            .as_deref()
                            .is_some_and(|verdict| verdict.contains("signal-strength violation")),
                        "{}: the judge must refuse a plan with `{shape}` at ({}, {}, {}), which \
                         leaves ({}, {}, {}) dark; got {:?}",
                        case.name,
                        cell.x,
                        cell.y,
                        cell.z,
                        reading.cell.x,
                        reading.cell.y,
                        reading.cell.z,
                        after.shipping_verdict,
                    );
                }
            }
        }
    }

    for shape in [DELETED, UNREFRESHED, RAMP_CUT, RISER_INSULATED] {
        assert!(
            bit.get(shape).copied().unwrap_or(0) > 0,
            "no injection of the shape `{shape}` ever severed delivery -- that row is vacuous; \
             the shapes that did bite were {bit:?}"
        );
    }
}

const DELETED: &str = "repeater deleted";
const UNREFRESHED: &str = "repeater replaced by dust";
const RAMP_CUT: &str = "the refresh before a climb deleted";
const RISER_INSULATED: &str = "the riser under a climb turned to glass";

/// A full cube that does **not** conduct -- `propagate::block_signal_at`'s own
/// first gate refuses it, so nothing block-mediated can pass through one.
fn glass() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Glass;
    state.name = "minecraft:glass".to_string();
    state
}
