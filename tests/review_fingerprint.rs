//! The review method behind "the rest length changed nothing" (task 14,
//! 2026-08-27), kept in the tree because a cited number needs a reproducible
//! method in it.
//!
//! # What it answers that a pinned block count cannot
//!
//! `SIGNAL_REST_LENGTH` shipped at `0.0`, and the claim that goes with it is
//! that the solver is *bit for bit* the one that shipped at `2ef0b4f`. Four
//! pinned counts (232 / 1,065 / 6,416 / 16,244) do not establish that, for two
//! separate reasons:
//!
//! - **Two of the four are not placed by relaxation at all.** `segment_a` and
//!   `seven_segment` place by relaxation, fail to route, and `compile` returns
//!   the legacy emitter's world. Their block counts are therefore constant
//!   across any change to the placer whatsoever -- including one that broke it.
//!   The same is true of `verilog:seven_segment`, which does not even place.
//! - **A count is not a configuration.** Two different layouts can hold the
//!   same number of blocks, and a divergence in the last bits of an `f64` is
//!   invisible to a rounded anchor until some body happens to sit near a
//!   half-integer.
//!
//! `continuous_placement_fingerprint` is the quantity that does establish it:
//! the step count the relaxation loop exited on, plus every body's position as
//! raw `f64` bits and its chosen facing, one step before `snap` rounds
//! anything. This harness prints it for all six reference circuits, including
//! the three whose shipped world comes from somewhere else.
//!
//! # Method
//!
//! ```text
//! git worktree add /tmp/base <before-sha>
//! cp tests/review_fingerprint.rs /tmp/base/tests/
//! (cd /tmp/base && cargo test --release --test review_fingerprint -- --ignored --nocapture) > base.txt
//! cargo test --release --test review_fingerprint -- --ignored --nocapture   > head.txt
//! diff <(grep -v '^test result' base.txt) <(grep -v '^test result' head.txt)
//! ```
//!
//! Measured 2026-08-27 across `2ef0b4f..5470282`: identical, sha256
//! `b75e225f...` on both sides -- 187 body lines, five step counts (and4 8,
//! full_adder 9, segment_a 11, seven_segment 11, verilog:and4 8) and one
//! identical projection deadlock.
//!
//! # Its companion, which is a different claim
//!
//! This stops at `relax`. The other half of the same review compares the
//! *compiled artefact* over the same two trees, and needs no test file at all:
//!
//! ```text
//! for c in and4 full_adder segment_a seven_segment verilog:and4 verilog:seven_segment; do
//!   cargo run --release --bin mc_dump -- "$c"
//! done
//! ```
//!
//! `mc_dump` prints `SIZE`, every `BLOCK` with its kind, facing, face, lit
//! state and delay, and the `INPUT` / `OUTPUT` / `GATEOUT` / `GATE` tables, so
//! a `sha256sum` over its output is a byte comparison of the whole shipped
//! circuit. All six matched across the same pair of trees.
//!
//! Neither of these asserts anything: they are a method for a review that has
//! two trees to hand, not a guard for a tree that has one.
//! `relax::tests::a_zero_rest_length_is_the_solver_that_shipped_before_it` is
//! the guard, and it is a weaker claim -- `relax` against
//! `relax_with_rest(0.0)` inside one tree, which cannot see a change that
//! moved both.

use reda::circuits::{and4, full_adder, seven_segment, verilog};
use reda::compile::lowering::{lower, lower_optimised};
use reda::compile::planner::{continuous_placement_fingerprint, plan_from_netlist, PortPlacements};
use reda::compile::Netlist;

/// The six circuits every condition sweep in this project runs, built the way
/// `planner::sweep_the_signal_rest_length` builds them -- `baked_netlist`
/// rather than `synthesize`, so the run needs no `python` and no `yowasp-yosys`
/// and cannot pick up a synthesis difference that is not the placer's.
fn cases() -> Vec<(String, Netlist)> {
    let mut out: Vec<(String, Netlist)> = vec![
        ("and4".to_string(), and4::build_and4_netlist().0),
        ("full_adder".to_string(), full_adder::build_full_adder_netlist().0),
        ("segment_a".to_string(), seven_segment::build_single_segment_netlist(0).0),
        ("seven_segment".to_string(), seven_segment::build_seven_segment_netlist().0),
    ];
    // `verilog:and4` lowers ordinarily and `verilog:seven_segment` optimised,
    // which is the pairing `mc_dump` and the condition sweeps both use.
    for (name, optimised) in [("verilog:and4", false), ("verilog:seven_segment", true)] {
        let circuit = verilog::find(name).expect("the catalog has it");
        let (gate_level, _) = circuit.baked_netlist();
        let netlist = if optimised { lower_optimised(&gate_level) } else { lower(&gate_level) }
            .expect("it lowers");
        out.push((name.to_string(), netlist));
    }
    out
}

/// Asserts nothing; `--ignored --nocapture`. A circuit that cannot place
/// prints its error rather than panicking, because
/// `verilog:seven_segment`'s projection deadlock is a fact about the netlist
/// that this harness has to be able to compare rather than die on.
#[test]
#[ignore = "review harness: asserts nothing, prints the placement fingerprint of six circuits"]
fn review_continuous_placement_fingerprint() {
    for (name, netlist) in cases() {
        println!("=== {name} ===");
        match continuous_placement_fingerprint(&netlist, &PortPlacements::default()) {
            Ok(text) => print!("{text}"),
            Err(error) => println!("PLACE ERR {error}"),
        }
    }
}

/// The exact delay model's reading on every circuit the planner can actually
/// route, printed so that "delay is unchanged" is a comparison and not an
/// inference.
///
/// `2ef0b4f` made `cost().delay` an upper bound rather than an optimistic
/// guess, and the pre-registered NO_GO for the rest length was "delay gets
/// worse on any circuit". `the_ship_review_panel` answers that side through the
/// **simulator** (`worst_settle_game_ticks`); this answers it through the
/// **model**, which is the other half of the same question and the half that
/// covers a plan the simulator never sees. A circuit the planner refuses has no
/// `cost()` at all, so it prints its refusal -- which is itself a comparison,
/// because the refusal has an address and the address is what moves when a
/// placement moves.
///
/// Measured 2026-08-27 across `2ef0b4f..5470282`: identical on all six lines.
///
/// Asserts nothing; `--ignored --nocapture`.
#[test]
#[ignore = "review harness: asserts nothing, prints the exact delay model per circuit"]
fn review_planner_cost_per_circuit() {
    for (name, netlist) in cases() {
        match plan_from_netlist(&netlist, &PortPlacements::default()) {
            Ok(plan) => {
                let cost = plan.cost();
                let cells: usize = plan.routes().iter().map(|route| route.anchors().len()).sum();
                println!(
                    "{name:24} delay {:>3}  wire {:>5}  turns {:>4}  {cells:>5} cells",
                    cost.delay, cost.wire, cost.turns
                );
            }
            Err(error) => println!("{name:24} NO PLAN: {error}"),
        }
    }
}
