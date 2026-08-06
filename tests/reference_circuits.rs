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

use reda::circuits::and4::{build_and4_netlist, INPUT_NAMES as AND4_INPUT_NAMES};
use reda::circuits::full_adder::{build_full_adder_netlist, INPUT_NAMES as ADDER_INPUT_NAMES};
use reda::circuits::seven_segment::{
    build_single_segment_netlist, INPUT_NAMES as DECODER_INPUT_NAMES, TRUTH_TABLE,
};
use reda::compile::compile;
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

#[test]
fn the_compiled_and4_matches_its_truth_table() {
    let (netlist, output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    let lever_positions: HashMap<&str, (i32, i32, i32)> = AND4_INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();

    let mut mismatches = Vec::new();
    for combination in 0u8..16 {
        let bits = [
            (combination >> 3) & 1,
            (combination >> 2) & 1,
            (combination >> 1) & 1,
            combination & 1,
        ];
        for (&name, &bit) in AND4_INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever(&mut simulator, lever_positions[name], bit == 1);
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
}

#[test]
fn the_compiled_full_adder_matches_its_truth_table() {
    let (netlist, output_signal) = build_full_adder_netlist();
    let compiled = compile(&netlist).expect("full_adder is acyclic and fully driven");

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    let lever_positions: HashMap<&str, (i32, i32, i32)> = ADDER_INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let sum_position = *compiled.output_positions.get(&output_signal["sum"]).unwrap();
    let cout_position = *compiled.output_positions.get(&output_signal["cout"]).unwrap();

    let mut mismatches = Vec::new();
    for combination in 0u8..8 {
        let bits = [(combination >> 2) & 1, (combination >> 1) & 1, combination & 1];
        for (&name, &bit) in ADDER_INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever(&mut simulator, lever_positions[name], bit == 1);
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
}

#[test]
fn the_compiled_segment_a_matches_its_truth_table() {
    // Segment index 0 is "a" in `SEGMENT_NAMES`.
    let (netlist, output_signal) = build_single_segment_netlist(0);
    let compiled = compile(&netlist).expect("segment_a is acyclic and fully driven");

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

    let lever_positions: HashMap<&str, (i32, i32, i32)> = DECODER_INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();

    let mut mismatches = Vec::new();
    for value in 0u8..16 {
        let bits = [(value >> 3) & 1, (value >> 2) & 1, (value >> 1) & 1, value & 1];
        for (&name, &bit) in DECODER_INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever(&mut simulator, lever_positions[name], bit == 1);
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
}
