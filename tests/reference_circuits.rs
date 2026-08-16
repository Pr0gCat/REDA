//! Acceptance tests for the smaller reference circuits: `and4`, `full_adder`,
//! and a single segment of the seven-segment decoder (`segment_a`).
//!
//! Same style as `the_compiled_decoder_matches_its_truth_table` in
//! `tests/seven_segment.rs`: compile, simulate every input combination
//! through the real redstone simulator, and check it against a truth table
//! that is written independently of the netlist (so a bug shared between the
//! netlist builder and the test can't cancel itself out). These circuits are
//! small, so all three tests together run in a small fraction of the time
//! the full decoder's test takes.

use std::collections::HashMap;

use reda::circuits::and4::build_and4_netlist;
use reda::circuits::and4::INPUT_NAMES as AND4_INPUT_NAMES;
use reda::circuits::full_adder::build_full_adder_netlist;
use reda::circuits::full_adder::INPUT_NAMES as ADDER_INPUT_NAMES;
use reda::circuits::seven_segment::{
    build_seven_segment_netlist, build_single_segment_netlist,
    INPUT_NAMES as DECODER_INPUT_NAMES, TRUTH_TABLE,
};
use reda::compile::{compile, compile_legacy, CompiledCircuit, Netlist, PlannerKind};
use reda::redstone::rules::taxonomy::flags_of;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, Face, Facing};
use reda::redstone::world::storage::World;
use reda::timing::{
    game_ticks_to_redstone_ticks, game_ticks_to_seconds, observations_to_result, summarize_worst_case,
    watch_all_nets, TransitionResult,
};

const MAX_TICKS: u64 = 2000;

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) {
    let mut state = simulator.world().get(position.0, position.1, position.2).clone();
    state.lit = on;
    simulator.world_mut().set(position.0, position.1, position.2, state);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle after changing an input");
}

/// Same as `set_lever`, but also records the transition's timing -- the
/// simulator must already have an observer attached (see `watch_all_nets`).
fn set_lever_and_record(
    simulator: &mut Simulator,
    position: (i32, i32, i32),
    on: bool,
    transitions: &mut Vec<TransitionResult>,
) {
    simulator.reset_observer();
    let start_tick = simulator.current_tick();
    set_lever(simulator, position, on);
    let settle_game_ticks = simulator.current_tick() - start_tick;
    transitions.push(observations_to_result(simulator.observations(), start_tick, settle_game_ticks));
}

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator.world().get(position.0, position.1, position.2).lit
}

/// Print the worst case across an instrumented sweep: settle time (game
/// ticks / redstone ticks / seconds), the logic-depth lower bound, the ratio
/// between them, the corrected critical-path settle model (and its exactness
/// against the measured settle time), the critical path itself, and how many
/// input vectors glitched which outputs. This is dynamic timing analysis
/// (`reda::timing`) applied to the same sweep the correctness check above
/// already ran.
fn report_timing(
    label: &str,
    netlist: &Netlist,
    compiled: &CompiledCircuit,
    outputs: &[String],
    transitions: &[TransitionResult],
) {
    let summary = summarize_worst_case(netlist, compiled, outputs, transitions);
    eprintln!(
        "{label} timing: worst-case settle = {} game ticks ({:.1} redstone ticks, {:.3}s)",
        summary.worst_settle_game_ticks,
        game_ticks_to_redstone_ticks(summary.worst_settle_game_ticks),
        game_ticks_to_seconds(summary.worst_settle_game_ticks),
    );
    eprintln!(
        "{label} timing: logic-depth bound (netlist, layout-independent) = {} gates -> {} game ticks; \
         ratio (measured/bound) = {:.2}x",
        summary.logic_depth, summary.logic_depth_bound_game_ticks, summary.ratio,
    );
    // The model's repeater term is read out of `compile::routing_stats`, which
    // recomputes the row/channel/track emitter's own geometry and reads the
    // world along it. Since the hybrid `compile` landed, the world may instead
    // have been placed by relaxation, and then there is no such geometry and no
    // per-edge route in the `CompiledCircuit` to count -- realisation consumed
    // the `PlanCandidate` that held the routes.
    //
    // **Both arms assert.** Which one runs is decided by an observable property
    // of the circuit rather than by a flag, so this is not a check that can be
    // switched off: a circuit the emitter laid out must reconstruct its settle
    // time exactly, and a circuit relaxation laid out must have no model at
    // all. `the_settle_model_is_exact_on_the_emitters_layout` below keeps the
    // exactness assertion running for the two circuits that moved to the
    // planner, on the layout it describes.
    match summary.critical_path_model_game_ticks {
        Some(model) => {
            eprintln!(
                "{label} timing: critical-path settle model (this layout) = {} gates + {:?} \
                 repeaters -> {model} game ticks predicted, {} measured",
                summary.critical_path_gate_count,
                summary.critical_path_repeater_count,
                summary.worst_settle_game_ticks,
            );
            assert_eq!(
                model, summary.worst_settle_game_ticks,
                "{label}: the critical-path settle model must exactly reconstruct the measured settle time"
            );
        }
        None => {
            assert_eq!(
                compiled.planner_kind(),
                PlannerKind::Unified3d,
                "{label}: only a relaxation-placed layout may be without a settle model"
            );
            eprintln!(
                "{label} timing: critical-path settle model unavailable -- relaxation placed this \
                 layout and `routing_stats` describes the emitter's; {} gates on the path, \
                 {} game ticks measured",
                summary.critical_path_gate_count, summary.worst_settle_game_ticks,
            );
        }
    }
    eprintln!(
        "{label} timing: critical path to worst output `{}`: {}",
        summary.critical_output,
        summary.critical_path.join(" -> ")
    );
    eprintln!(
        "{label} timing: glitches by output (number of input vectors that glitched it): {:?}",
        summary.glitch_counts
    );
}

#[test]
fn the_compiled_and4_matches_its_truth_table() {
    let (netlist, output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");

    let lever_positions: HashMap<&str, (i32, i32, i32)> = AND4_INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();
    let watched = watch_all_nets(&compiled);

    // Simulate on a clone of the world -- `compiled` itself is kept intact
    // (repeater kinds/positions never change during simulation, only `lit`
    // states do) so `report_timing` can hand it to
    // `compile::routing_stats::analyze` afterwards to count the actual
    // measured critical path's repeaters.
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");
    simulator.attach_observer(watched);

    let mut mismatches = Vec::new();
    let mut transitions: Vec<TransitionResult> = Vec::new();
    for combination in 0u8..16 {
        let bits = [
            (combination >> 3) & 1,
            (combination >> 2) & 1,
            (combination >> 1) & 1,
            combination & 1,
        ];
        for (&name, &bit) in AND4_INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever_and_record(&mut simulator, lever_positions[name], bit == 1, &mut transitions);
        }

        // Independently-written expected table: AND of all four bits.
        let expected = bits.iter().all(|&bit| bit == 1);
        let actual = read_output(&simulator, output_position);
        if actual != expected {
            mismatches.push(format!("inputs={bits:?}: expected {expected}, got {actual}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "compiled and4 does not match its truth table ({}/16 wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    report_timing("and4", &netlist, &compiled, &[output_signal], &transitions);
}

#[test]
fn the_compiled_full_adder_matches_its_truth_table() {
    let (netlist, output_signal) = build_full_adder_netlist();
    let compiled = compile(&netlist).expect("full_adder is acyclic and fully driven");

    let lever_positions: HashMap<&str, (i32, i32, i32)> = ADDER_INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let sum_position = *compiled.output_positions.get(&output_signal["sum"]).unwrap();
    let cout_position = *compiled.output_positions.get(&output_signal["cout"]).unwrap();
    let watched = watch_all_nets(&compiled);

    // See the and4 test above for why this simulates on a clone of the
    // world rather than the world moved out of `compiled`.
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");
    simulator.attach_observer(watched);

    let mut mismatches = Vec::new();
    let mut transitions: Vec<TransitionResult> = Vec::new();
    for combination in 0u8..8 {
        let bits = [(combination >> 2) & 1, (combination >> 1) & 1, combination & 1];
        for (&name, &bit) in ADDER_INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever_and_record(&mut simulator, lever_positions[name], bit == 1, &mut transitions);
        }

        // Independently-written expected table: a 1-bit binary adder.
        let ones = bits.iter().filter(|&&bit| bit == 1).count();
        let expected_sum = ones % 2 == 1;
        let expected_cout = ones >= 2;

        let actual_sum = read_output(&simulator, sum_position);
        let actual_cout = read_output(&simulator, cout_position);
        if actual_sum != expected_sum {
            mismatches.push(format!("inputs={bits:?} sum: expected {expected_sum}, got {actual_sum}"));
        }
        if actual_cout != expected_cout {
            mismatches.push(format!("inputs={bits:?} cout: expected {expected_cout}, got {actual_cout}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "compiled full_adder does not match its truth table ({}/16 wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    report_timing(
        "full_adder",
        &netlist,
        &compiled,
        &[output_signal["sum"].clone(), output_signal["cout"].clone()],
        &transitions,
    );
}

#[test]
fn the_compiled_segment_a_matches_its_truth_table() {
    // Segment index 0 is "a" in `SEGMENT_NAMES`.
    let (netlist, output_signal) = build_single_segment_netlist(0);
    let compiled = compile(&netlist).expect("segment_a is acyclic and fully driven");

    let lever_positions: HashMap<&str, (i32, i32, i32)> = DECODER_INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();
    let watched = watch_all_nets(&compiled);

    // See the and4 test above for why this simulates on a clone of the
    // world rather than the world moved out of `compiled`.
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");
    simulator.attach_observer(watched);

    let mut mismatches = Vec::new();
    let mut transitions: Vec<TransitionResult> = Vec::new();
    for value in 0u8..16 {
        let bits = [(value >> 3) & 1, (value >> 2) & 1, (value >> 1) & 1, value & 1];
        for (&name, &bit) in DECODER_INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever_and_record(&mut simulator, lever_positions[name], bit == 1, &mut transitions);
        }

        // Independently-sourced expected value: the project's own truth
        // table for segment "a" (column 0), undefined (off) past digit 9.
        let expected = (value as usize) < TRUTH_TABLE.len() && TRUTH_TABLE[value as usize][0] == 1;
        let actual = read_output(&simulator, output_position);
        if actual != expected {
            mismatches.push(format!("d3d2d1d0={value:04b}: expected {expected}, got {actual}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "compiled segment_a does not match its truth table ({}/16 wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    report_timing("segment_a", &netlist, &compiled, &[output_signal], &transitions);
}

// ---------------------------------------------------------------------
// Lever attachment: every lever compile() places must carry an explicit
// `face`, and the neighbour it attaches to given that face must actually be
// a valid support -- not air. This is the regression test for the bug where
// `lever()` never set `face` at all: Minecraft's own default is `wall`, our
// layout never builds a wall next to a lever, and every lever popped off as
// a dropped item on the first block update after paste. See `lever()` in
// `src/compile/mod.rs` and the blockstate table on `minecraft.wiki/w/Lever`.
// ---------------------------------------------------------------------

/// Where the neighbour a lever with this `face` × `facing` must attach to
/// lives, in world coordinates.
/// The critical-path settle model still reconstructs the measured settle time
/// exactly -- on the layout it describes.
///
/// **This is coverage moved, not coverage dropped.** Before the hybrid,
/// `report_timing` asserted this for and4, full_adder and segment_a, on worlds
/// the row/channel/track emitter had laid out. Two of those three now compile
/// through the planner, and `routing_stats` -- which is where the model's
/// repeater term comes from -- can only read the emitter's geometry, so
/// `report_timing`'s assertion no longer runs for them. This runs it, over the
/// same three circuits, against `compile_legacy`.
///
/// segment_a is included even though it still falls back and is therefore
/// still covered by `report_timing`: a circuit that starts routing through the
/// planner would silently drop out of that check, and the whole point of this
/// test is that the model's exactness stops depending on which path `compile`
/// happens to take.
///
/// The sweep is the truth-table sweep without the truth table -- only the
/// timing matters here, and the correctness of these three worlds is asserted
/// three times over already.
#[test]
fn the_settle_model_is_exact_on_the_emitters_layout() {
    let circuits: [(&str, Netlist, &[&str], Vec<String>); 3] = [
        {
            let (netlist, output) = build_and4_netlist();
            ("and4", netlist, &AND4_INPUT_NAMES[..], vec![output])
        },
        {
            let (netlist, outputs) = build_full_adder_netlist();
            (
                "full_adder",
                netlist,
                &ADDER_INPUT_NAMES[..],
                vec![outputs["sum"].clone(), outputs["cout"].clone()],
            )
        },
        {
            let (netlist, output) = build_single_segment_netlist(0);
            ("segment_a", netlist, &DECODER_INPUT_NAMES[..], vec![output])
        },
    ];

    for (name, netlist, input_names, outputs) in circuits {
        let compiled = compile_legacy(&netlist).expect("every reference circuit compiles");
        assert_eq!(
            compiled.planner_kind(),
            PlannerKind::Legacy,
            "{name}: `compile_legacy` must produce the emitter's layout"
        );

        let lever_positions: Vec<(i32, i32, i32)> = input_names
            .iter()
            .map(|&input| *compiled.input_positions.get(input).unwrap())
            .collect();
        let watched = watch_all_nets(&compiled);
        let mut simulator = Simulator::new(compiled.world.clone());
        simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");
        simulator.attach_observer(watched);

        let mut transitions: Vec<TransitionResult> = Vec::new();
        for combination in 0u32..(1 << input_names.len()) {
            for (index, &position) in lever_positions.iter().enumerate() {
                let bit = (combination >> (input_names.len() - 1 - index)) & 1;
                set_lever_and_record(&mut simulator, position, bit == 1, &mut transitions);
            }
        }

        let summary = summarize_worst_case(&netlist, &compiled, &outputs, &transitions);
        let model = summary.critical_path_model_game_ticks.unwrap_or_else(|| {
            panic!("{name}: the emitter's own layout must have a settle model")
        });
        eprintln!(
            "{name} (emitter layout): {} gates + {:?} repeaters -> {model} predicted, \
             {} measured",
            summary.critical_path_gate_count,
            summary.critical_path_repeater_count,
            summary.worst_settle_game_ticks,
        );
        assert_eq!(
            model, summary.worst_settle_game_ticks,
            "{name}: the critical-path settle model must exactly reconstruct the measured settle time"
        );
    }
}

/// Which of `compile`'s two paths each reference circuit takes, pinned.
///
/// `compile` is a hybrid: it tries relaxation placement with A* routing first
/// and falls back to the row/channel/track emitter on any failure -- placement,
/// routing or verification. The fallback is deliberately silent, because a
/// trial that failed is not a compile that failed; `planner_kind` is what keeps
/// it from being *invisible*, and this is what makes it audited.
///
/// **This replaces an assertion that could not fail.** Before the hybrid the
/// same test asserted `Unified3d` for all four -- but `compile` stamped
/// `Unified3d` on every circuit it ever returned, so the assertion held no
/// matter what the compiler did. `PlannerKind` now names the **placer**, and
/// these four values are four different measured facts:
///
/// | circuit | gates | path | why |
/// |---|---|---|---|
/// | and4 | 7 | `Unified3d` | routes on rip-up round 1 |
/// | full_adder | 22 | `Unified3d` | routes on rip-up round 5 |
/// | segment_a | 46 | `Legacy` | never routes -- `no safe local route`, at 8 rounds or at 64 |
/// | seven_segment | 84 | `Legacy` | never routes, same failure |
///
/// The two frontiers behind the last two rows are recorded in
/// `docs/superpowers/specs/2026-08-15-routing-at-scale.md`. A row that changes
/// is either routing having been fixed -- in which case the block count in
/// `the_hand_written_circuits_keep_their_measured_size` moves with it -- or a
/// circuit having quietly stopped taking the better placer, which is the thing
/// this exists to catch.
#[test]
fn every_reference_circuit_records_which_path_produced_it() {
    let circuits: [(&str, Netlist, PlannerKind); 4] = [
        ("and4", build_and4_netlist().0, PlannerKind::Unified3d),
        ("full_adder", build_full_adder_netlist().0, PlannerKind::Unified3d),
        ("segment_a", build_single_segment_netlist(0).0, PlannerKind::Legacy),
        ("seven_segment", build_seven_segment_netlist().0, PlannerKind::Legacy),
    ];

    for (name, netlist, expected) in circuits {
        let compiled = compile(&netlist).expect("every reference circuit compiles");
        assert_eq!(
            compiled.planner_kind(),
            expected,
            "{name} took the other path -- if that is intended, its block count moved too"
        );
    }
}

/// The size of every hand-written circuit, pinned.
///
/// `2026-08-09-global-polarity-assignment.md` made these a hard constraint --
/// "the control circuits remain byte-for-byte at their measured values" -- and
/// nothing asserted them, so the one thing the polarity work was not allowed
/// to disturb was also the one thing nothing would have noticed it disturbing.
/// These four are pure NOR and no lowering touches them, which is exactly why
/// a change here means something moved that should not have.
///
/// # Re-pinned at the hybrid switchover, 2026-08-16
///
/// Two of the four moved, and that movement **is** the change: `compile` now
/// tries relaxation placement first and falls back to the row/channel/track
/// emitter only where the planner cannot deliver a verified circuit.
///
/// | circuit | emitter | today | path | change |
/// |---|---|---|---|---|
/// | and4 | 472 | **232** | `Unified3d` | -50.8% |
/// | full_adder | 1,784 | **1,065** | `Unified3d` | -40.3% |
/// | segment_a | 6,416 | 6,416 | `Legacy` | unmoved -- fell back |
/// | seven_segment | 16,244 | 16,244 | `Legacy` | unmoved -- fell back |
///
/// **A number that did not move is a circuit that fell back**, and that is
/// what the fourth column is here to make legible: segment_a and seven_segment
/// place by relaxation and then fail to route (`no safe local route`, at the
/// trial's 8 rip-up rounds and at the router's full 64 alike), so `compile`
/// returns the emitter's world for them, byte for byte what it always
/// returned. Nothing regressed; two things improved.
/// `every_reference_circuit_records_which_path_produced_it` above is where the
/// path itself is asserted rather than merely described.
///
/// This test's meaning moves from "these must not change" to "these were
/// measured here, and changing them again needs an explanation" -- which is
/// what it was always for. The numbers printed alongside are what
/// `README.md`'s table reports.
#[test]
fn the_hand_written_circuits_keep_their_measured_size() {
    // Blocks as measured at the hybrid switchover. The emitter's own numbers
    // are in the doc table above, beside the path each circuit takes.
    let circuits: [(&str, Netlist, usize, usize); 4] = [
        ("and4", build_and4_netlist().0, 7, 232),
        ("full_adder", build_full_adder_netlist().0, 22, 1065),
        ("segment_a", build_single_segment_netlist(0).0, 46, 6416),
        ("seven_segment", build_seven_segment_netlist().0, 84, 16244),
    ];

    for (name, netlist, gates, blocks) in circuits {
        let compiled = compile(&netlist).expect("every reference circuit compiles");
        let (size_x, size_y, size_z) = compiled.world.size();

        let mut placed = 0usize;
        let (mut min, mut max) = ((i32::MAX, i32::MAX, i32::MAX), (0, 0, 0));
        for x in 0..size_x {
            for y in 0..size_y {
                for z in 0..size_z {
                    if compiled.world.get(x, y, z).kind == BlockKind::Air {
                        continue;
                    }
                    placed += 1;
                    min = (min.0.min(x), min.1.min(y), min.2.min(z));
                    max = (max.0.max(x), max.1.max(y), max.2.max(z));
                }
            }
        }

        eprintln!(
            "{name}: {} gates, {placed} blocks, bounding box {}x{}x{}",
            netlist.gates.len(),
            max.0 - min.0 + 1,
            max.1 - min.1 + 1,
            max.2 - min.2 + 1,
        );
        assert_eq!(netlist.gates.len(), gates, "{name} gate count");
        assert_eq!(placed, blocks, "{name} block count");
    }
}

fn lever_support_position(pos: (i32, i32, i32), face: Face, facing: Facing) -> (i32, i32, i32) {
    let (x, y, z) = pos;
    match face {
        // Floor: the lever stands on top of the block below it.
        Face::Floor => (x, y - 1, z),
        // Ceiling: the lever hangs off the bottom of the block above it.
        Face::Ceiling => (x, y + 1, z),
        // Wall: `facing` points away from the wall, so the support sits in
        // the opposite direction (minecraft.wiki/w/Lever: "Opposite to the
        // direction the player is facing if placed on the side of a block").
        Face::Wall => match facing.opposite() {
            Facing::North => (x, y, z - 1),
            Facing::South => (x, y, z + 1),
            Facing::East => (x + 1, y, z),
            Facing::West => (x - 1, y, z),
            Facing::Up => (x, y + 1, z),
            Facing::Down => (x, y - 1, z),
        },
    }
}

/// Whether a lever with this `face` can actually attach to `support`.
///
/// `SUPPORT_FULL` is documented on `BlockFlags` as "頂面是完整實心方形面：
/// 紅石粉、拉桿、按鈕放得上去" -- top face is a full solid square, which is
/// exactly what a floor (and, for the full cubes we ever place, a ceiling)
/// lever needs. `SIDE_FULL` (`can_attach_wall_torch`) is the same "full side
/// face" requirement a wall torch has, which a wall-mounted lever shares.
fn face_is_supported(support: &reda::redstone::world::block::BlockState, face: Face) -> bool {
    let flags = flags_of(support);
    match face {
        Face::Floor | Face::Ceiling => flags.can_carry_dust(),
        Face::Wall => flags.can_attach_wall_torch(),
    }
}

/// Scans `world` for every placed lever and asserts it has an explicit
/// `face`/`facing` and a real, valid support block.
fn assert_every_lever_is_properly_attached(circuit_name: &str, world: &World) {
    let (size_x, size_y, size_z) = world.size();
    let mut lever_count = 0usize;

    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                let state = world.get(x, y, z);
                if state.kind != BlockKind::Lever {
                    continue;
                }
                lever_count += 1;

                let face = state.face.unwrap_or_else(|| {
                    panic!("{circuit_name}: lever at ({x},{y},{z}) has no explicit face")
                });
                let facing = state.facing.unwrap_or_else(|| {
                    panic!("{circuit_name}: lever at ({x},{y},{z}) has no explicit facing")
                });

                let (sx, sy, sz) = lever_support_position((x, y, z), face, facing);
                let support = world.get(sx, sy, sz);

                assert_ne!(
                    support.kind,
                    BlockKind::Air,
                    "{circuit_name}: lever at ({x},{y},{z}) with face={face:?} facing={facing:?} \
                     attaches to air at ({sx},{sy},{sz}) -- it would pop off as a dropped item \
                     the moment Minecraft re-checks its support"
                );
                assert!(
                    face_is_supported(support, face),
                    "{circuit_name}: lever at ({x},{y},{z}) with face={face:?} attaches to \
                     {support:?} at ({sx},{sy},{sz}), which is not a valid support for that face"
                );
            }
        }
    }

    assert!(
        lever_count > 0,
        "{circuit_name}: found no levers at all -- this test would be vacuously true"
    );
}

#[test]
fn every_lever_in_and4_is_properly_attached() {
    let (netlist, _output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
    assert_every_lever_is_properly_attached("and4", &compiled.world);
}

#[test]
fn every_lever_in_full_adder_is_properly_attached() {
    let (netlist, _output_signal) = build_full_adder_netlist();
    let compiled = compile(&netlist).expect("full_adder is acyclic and fully driven");
    assert_every_lever_is_properly_attached("full_adder", &compiled.world);
}

#[test]
fn every_lever_in_segment_a_is_properly_attached() {
    let (netlist, _output_signal) = build_single_segment_netlist(0);
    let compiled = compile(&netlist).expect("segment_a is acyclic and fully driven");
    assert_every_lever_is_properly_attached("segment_a", &compiled.world);
}

#[test]
fn every_lever_in_seven_segment_is_properly_attached() {
    let (netlist, _segment_signal) = build_seven_segment_netlist();
    let compiled = compile(&netlist).expect("seven_segment is acyclic and fully driven");
    assert_every_lever_is_properly_attached("seven_segment", &compiled.world);
}
