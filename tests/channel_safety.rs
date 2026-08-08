//! Regression coverage for the channel router's dust-adjacency safety
//! condition (see `docs/superpowers/specs/2026-08-09-channel-safety-condition.md`).
//!
//! That document derives, from `redstone::simulator::connectivity::dust_reach`,
//! the exact rule the router has always depended on: two different-net
//! conductor cells need at least 2 cells of horizontal clearance at any Y,
//! except across a repeater's own non-facing sides. It also found a real bug
//! in `resolve_bypass_and_geometry`'s widened bypass pass: two candidates
//! decided in the same pass check their own horizontal jog against a
//! `Reservation` snapshotted *before* the loop, so neither candidate's check
//! can see the other's prospective jog, and two jogs that overlap in X at the
//! same Z can both get approved.
//!
//! Two tests live here:
//!
//! - `two_feed_forward_nor_gates_at_the_widened_bypass_boundary_compile_and_
//!   match_their_truth_table`: the minimal two-gate reproduction from the
//!   spec, byte-for-byte (same input names, same fan-in order, same gate
//!   shapes), which used to fail `compile()` with a spurious
//!   `ConnectivityViolation` between two primary inputs' own nets.
//! - `a_seeded_search_over_small_feed_forward_nor_netlists_never_hits_a_
//!   connectivity_violation`: a deterministic, seeded version of the random
//!   search that originally found the bug. It generates the same *shape* of
//!   netlist every reference circuit already is -- plain feed-forward NOR
//!   gates, no merges, no adversarial construction -- and requires every one
//!   of them to compile. A fixed seed makes a failure exactly reproducible;
//!   on failure it prints the netlist it found so the case can be lifted
//!   straight into a new minimal regression test the way the spec's own
//!   two-gate case was.

use reda::compile::{compile, CompileError, Gate, Netlist};
use reda::redstone::simulator::Simulator;

const MAX_TICKS: u64 = 2000;

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) {
    let mut state = simulator.world().get(position.0, position.1, position.2).clone();
    state.lit = on;
    simulator.world_mut().set(position.0, position.1, position.2, state);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle after changing an input");
}

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator.world().get(position.0, position.1, position.2).lit
}

fn nor(name: &str, inputs: &[&str], output: &str) -> Gate {
    Gate {
        name: name.to_string(),
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        output: output.to_string(),
        is_merge: false,
    }
}

/// The exact minimal reproduction from
/// `docs/superpowers/specs/2026-08-09-channel-safety-condition.md`: two
/// feed-forward NOR gates, five primary inputs, no merges, nothing exotic.
/// `in1` and `in2` are both levers in row 0, both at distance 12 from their
/// sink's approach column -- exactly `BYPASS_QUERY_MAX_DISTANCE`, so both are
/// evaluated in `resolve_bypass_and_geometry`'s widened pass, and (before the
/// fix) both got approved because their horizontal jogs, laid at the same
/// row's Z, were each checked only against a reservation snapshot that
/// predated either jog.
fn two_gate_widened_bypass_netlist() -> Netlist {
    Netlist {
        inputs: vec!["in0".into(), "in1".into(), "in2".into(), "in3".into(), "in4".into()],
        outputs: vec!["g0".into(), "g1".into()],
        gates: vec![
            nor("g0", &["in0", "in3", "in2"], "g0"),
            nor("g1", &["in1", "in3", "in4"], "g1"),
        ],
    }
}

#[test]
fn two_feed_forward_nor_gates_at_the_widened_bypass_boundary_compile_and_match_their_truth_table() {
    let netlist = two_gate_widened_bypass_netlist();
    let compiled = compile(&netlist).unwrap_or_else(|err| {
        panic!(
            "this netlist is acyclic, fully driven, and has no merges -- it must compile; got {err:?}"
        )
    });

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    let lever = |name: &str| *compiled.input_positions.get(name).unwrap();
    let (in0, in1, in2, in3, in4) = (lever("in0"), lever("in1"), lever("in2"), lever("in3"), lever("in4"));
    let g0 = *compiled.output_positions.get("g0").unwrap();
    let g1 = *compiled.output_positions.get("g1").unwrap();

    // Full 32-row truth table, computed independently of the netlist builder:
    // g0 = NOR(in0, in3, in2), g1 = NOR(in1, in3, in4).
    for mask in 0u8..32 {
        let bit = |i: u8| (mask >> i) & 1 == 1;
        let (a0, a1, a2, a3, a4) = (bit(0), bit(1), bit(2), bit(3), bit(4));

        set_lever(&mut simulator, in0, a0);
        set_lever(&mut simulator, in1, a1);
        set_lever(&mut simulator, in2, a2);
        set_lever(&mut simulator, in3, a3);
        set_lever(&mut simulator, in4, a4);

        let expected_g0 = !(a0 || a3 || a2);
        let expected_g1 = !(a1 || a3 || a4);
        let got_g0 = read_output(&simulator, g0);
        let got_g1 = read_output(&simulator, g1);

        assert_eq!(
            got_g0, expected_g0,
            "g0 = NOR(in0={a0}, in3={a3}, in2={a2}) should be {expected_g0}, got {got_g0}"
        );
        assert_eq!(
            got_g1, expected_g1,
            "g1 = NOR(in1={a1}, in3={a3}, in4={a4}) should be {expected_g1}, got {got_g1}"
        );
    }
}

// ---------------------------------------------------------------------
// A seeded, deterministic search over small feed-forward NOR netlists
// ---------------------------------------------------------------------
//
// This is a fixed-seed replacement for the random search described in
// `docs/superpowers/specs/2026-08-09-channel-safety-condition.md`, which
// found the bug above without any merges or hand-crafted geometry -- plain
// `Gate`s, feed-forward only, built the exact same way every reference
// circuit already is. Keeping a deterministic version of that search in the
// suite is worth more than the single case it happened to catch: a bug two
// gates wide survived every hand-written circuit in this project, so the
// space of "plain, small, feed-forward netlists" is not as thoroughly
// covered by hand as it looks.
//
// A small, dependency-free splitmix64 generator is used instead of pulling in
// `rand`, so the sequence this test walks is pinned to this file forever --
// no dependency version bump can ever change which netlists get generated.

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound` (`bound` must be > 0). Not perfectly unbiased,
    /// but the bounds this test uses are all tiny, so the bias is
    /// negligible and reproducibility matters far more than uniformity here.
    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// The fixed seed this test always runs with. Changing it changes which
/// netlists get generated -- if this ever needs to change, say why in the
/// commit that changes it, the same way a snapshot-test golden file would.
const SEED: u64 = 0x00C0_FFEE_2026_0809;

const TRIALS: usize = 2000;
const MIN_INPUTS: usize = 3;
const MAX_INPUTS: usize = 6;
const MIN_GATES: usize = 1;
const MAX_GATES: usize = 4;
const MAX_FAN_IN: usize = 3;

/// Build one small, feed-forward, merge-free NOR netlist: a random number of
/// primary inputs, then a random number of gates, each a NOR of 1..=3
/// signals drawn only from the primary inputs and gates already built --
/// which makes the netlist acyclic by construction, exactly the shape
/// `NetlistBuilder` and every hand-written reference circuit already produce.
/// Every gate's output is also a declared circuit output, so every net in
/// the netlist actually has to route somewhere and none of them are dead
/// signals `build_nets` would silently drop.
fn random_feed_forward_netlist(rng: &mut SplitMix64) -> Netlist {
    let input_count = MIN_INPUTS + rng.below(MAX_INPUTS - MIN_INPUTS + 1);
    let inputs: Vec<String> = (0..input_count).map(|i| format!("in{i}")).collect();

    let gate_count = MIN_GATES + rng.below(MAX_GATES - MIN_GATES + 1);
    let mut gates = Vec::with_capacity(gate_count);
    let mut available: Vec<String> = inputs.clone();

    for g in 0..gate_count {
        let fan_in = 1 + rng.below(MAX_FAN_IN.min(available.len()));
        let mut chosen: Vec<String> = Vec::with_capacity(fan_in);
        // Sample without replacement out of `available` so a gate never
        // takes the same signal as two of its own inputs.
        let mut pool = available.clone();
        for _ in 0..fan_in {
            let index = rng.below(pool.len());
            chosen.push(pool.remove(index));
        }
        let output = format!("g{g}");
        gates.push(nor(&output, &chosen.iter().map(String::as_str).collect::<Vec<_>>(), &output));
        available.push(output);
    }

    let outputs: Vec<String> = (0..gate_count).map(|g| format!("g{g}")).collect();
    Netlist { inputs, outputs, gates }
}

fn describe(netlist: &Netlist) -> String {
    let mut out = String::new();
    out.push_str(&format!("inputs: {:?}\n", netlist.inputs));
    out.push_str(&format!("outputs: {:?}\n", netlist.outputs));
    for gate in &netlist.gates {
        out.push_str(&format!("  {} = NOR({})\n", gate.output, gate.inputs.join(", ")));
    }
    out
}

#[test]
fn a_seeded_search_over_small_feed_forward_nor_netlists_never_hits_a_connectivity_violation() {
    let mut rng = SplitMix64::new(SEED);

    for trial in 0..TRIALS {
        let netlist = random_feed_forward_netlist(&mut rng);

        if let Err(err) = compile(&netlist) {
            panic!(
                "trial {trial} (seed {SEED:#x}) found a netlist that failed to compile: {err:?}\n\n\
                 this netlist is acyclic, fully driven, and has no merges -- it must compile.\n\
                 netlist that failed:\n{}",
                describe(&netlist)
            );
        }
    }
}

/// Confirms the search above is not vacuous -- it actually knows how to
/// reject a bad compile, not merely how to always report success. Feeds it
/// a netlist with an undriven input, which every `compile()` call must
/// reject with `CompileError::UndrivenSignal`.
#[test]
fn the_seeded_search_harness_can_tell_a_bad_compile_from_a_good_one() {
    let netlist = Netlist {
        inputs: vec!["a".into()],
        outputs: vec!["y".into()],
        gates: vec![nor("g0", &["a", "never_driven"], "y")],
    };
    assert!(matches!(compile(&netlist), Err(CompileError::UndrivenSignal(_))));
}
