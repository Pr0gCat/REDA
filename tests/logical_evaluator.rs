//! Acceptance tests for the pure logical [`Evaluator`]: gate-level
//! evaluation and positive-edge flip-flop stepping with no placement, no
//! routing, no world, and no Yosys.
//!
//! This is Milestone 0's oracle. Every later frontend milestone compares
//! truth tables and state traces through *this* machine, so it has to be
//! checked against something independent of itself first -- which is what
//! the baked netlists and the hand-written truth table below are: circuits
//! this project already trusts, evaluated by a machine that has never seen
//! them.
//!
//! None of these tests needs Python or `yowasp-yosys`: the baked netlists
//! are checked-in artifacts (`reda::circuits::verilog::VerilogCircuit::baked_netlist`),
//! and everything else is built by hand.

use std::collections::BTreeMap;

use reda::circuits::seven_segment::{SEGMENT_NAMES, TRUTH_TABLE};
use reda::circuits::verilog;
use reda::compile::topology::GateKind;
use reda::compile::{Gate, Netlist};
use reda::frontend::evaluate::{inputs_from_bits, validate, EvalError, Evaluator};

fn inputs(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), *value))
        .collect()
}

/// Milestone 0's exit criterion: the evaluator reproduces the independent
/// seven-segment truth table from the existing baked Netlist.
#[test]
fn the_evaluator_reproduces_the_seven_segment_truth_table_from_the_baked_netlist() {
    let circuit = verilog::find("verilog:seven_segment").expect("catalog entry");
    let (netlist, labels) = circuit.baked_netlist();
    let evaluator = Evaluator::new(&netlist).expect("the baked netlist is evaluable");

    for (digit, expected) in TRUTH_TABLE.iter().enumerate() {
        let row = inputs(&[
            ("d3", digit & 0b1000 != 0),
            ("d2", digit & 0b0100 != 0),
            ("d1", digit & 0b0010 != 0),
            ("d0", digit & 0b0001 != 0),
        ]);
        let values = evaluator.evaluate(&row).expect("evaluates");
        for (index, segment) in SEGMENT_NAMES.iter().enumerate() {
            let signal = labels
                .iter()
                .find(|(port, _)| port == segment)
                .map(|(_, signal)| signal)
                .unwrap_or_else(|| panic!("no label for segment `{segment}`"));
            assert_eq!(
                values[signal],
                expected[index] == 1,
                "digit {digit}, segment {segment}"
            );
        }
    }
}

/// The same check for the smaller baked circuit, against a predicate rather
/// than a table.
#[test]
fn the_evaluator_reproduces_and4_from_the_baked_netlist() {
    let circuit = verilog::find("verilog:and4").expect("catalog entry");
    let (netlist, labels) = circuit.baked_netlist();
    let evaluator = Evaluator::new(&netlist).expect("the baked netlist is evaluable");
    let y = labels
        .iter()
        .find(|(port, _)| port == "y")
        .map(|(_, signal)| signal.clone())
        .expect("a `y` label");

    for row in 0..16u32 {
        let values = evaluator
            .evaluate(&inputs_from_bits(&netlist.inputs, row))
            .expect("evaluates");
        assert_eq!(values[&y], row == 0b1111, "row {row:04b}");
    }
}

fn dff(output: &str, data: &str, clock: &str) -> Gate {
    Gate {
        name: output.to_string(),
        inputs: vec![data.to_string(), clock.to_string()],
        output: output.to_string(),
        kind: GateKind::DffPosedge,
    }
}

/// A flip-flop captures on the 0-to-1 transition of its clock and at no
/// other time -- not while the clock is already high, and not on its fall.
#[test]
fn a_flip_flop_captures_only_on_the_rising_edge() {
    let netlist = Netlist {
        inputs: vec!["d".to_string(), "clk".to_string()],
        outputs: vec!["q".to_string()],
        gates: vec![dff("q", "d", "clk")],
    };
    let mut evaluator = Evaluator::new(&netlist).expect("evaluable");

    // Every DFF starts at zero, so the first settle sees Q = 0.
    let trace = [
        // (d, clk, expected q after the step)
        (true, false, false),
        (true, true, true),   // rising edge: captures 1
        (false, true, true),  // clock still high: holds
        (false, false, true), // falling edge: holds
        (false, true, false), // rising edge: captures 0
    ];
    for (index, &(d, clk, expected)) in trace.iter().enumerate() {
        let outputs = evaluator
            .step(&inputs(&[("d", d), ("clk", clk)]))
            .expect("steps");
        assert_eq!(outputs["q"], expected, "step {index}");
    }
}

/// Two chained flip-flops commit *simultaneously*: one clock edge moves the
/// data one stage, not two. This is the property a cascade would get wrong.
#[test]
fn chained_flip_flops_commit_simultaneously() {
    let netlist = Netlist {
        inputs: vec!["d".to_string(), "clk".to_string()],
        outputs: vec!["q0".to_string(), "q1".to_string()],
        gates: vec![dff("q0", "d", "clk"), dff("q1", "q0", "clk")],
    };
    let mut evaluator = Evaluator::new(&netlist).expect("evaluable");

    let high = inputs(&[("d", true), ("clk", true)]);
    let low = inputs(&[("d", true), ("clk", false)]);

    let after_first = evaluator.step(&high).expect("steps");
    assert_eq!((after_first["q0"], after_first["q1"]), (true, false));

    evaluator.step(&low).expect("steps");
    let after_second = evaluator.step(&high).expect("steps");
    assert_eq!((after_second["q0"], after_second["q1"]), (true, true));

    // And a reset puts both stages back to the all-zero start state.
    evaluator.reset();
    assert_eq!(evaluator.state(), [false, false]);
}

/// A feedback path *through* a flip-flop is legal -- it is how a toggle is
/// built -- while a loop of combinational gates alone is not.
#[test]
fn feedback_through_a_flip_flop_is_legal() {
    let netlist = Netlist {
        inputs: vec!["clk".to_string()],
        outputs: vec!["q".to_string()],
        gates: vec![
            Gate {
                name: "n".to_string(),
                inputs: vec!["q".to_string()],
                output: "n".to_string(),
                kind: GateKind::Nor(1),
            },
            dff("q", "n", "clk"),
        ],
    };
    let mut evaluator = Evaluator::new(&netlist).expect("a DFF cuts the cycle");
    for expected in [true, false, true, false] {
        evaluator
            .step(&inputs(&[("clk", false)]))
            .expect("falling half");
        let outputs = evaluator
            .step(&inputs(&[("clk", true)]))
            .expect("rising half");
        assert_eq!(outputs["q"], expected);
    }
}

#[test]
fn validation_names_what_is_wrong_with_a_netlist() {
    let undriven = Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![Gate::nor("y", &["a", "missing"])],
    };
    assert_eq!(
        validate(&undriven),
        Err(EvalError::UndrivenNet {
            gate: "y".to_string(),
            input: "missing".to_string(),
        })
    );

    let duplicate = Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![Gate::nor("y", &["a"]), Gate::nor("y", &["a"])],
    };
    assert_eq!(
        validate(&duplicate),
        Err(EvalError::DuplicateOutput("y".to_string()))
    );

    let unproduced = Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["a".to_string()],
        gates: Vec::new(),
    };
    assert_eq!(
        validate(&unproduced),
        Err(EvalError::UndrivenOutput("a".to_string()))
    );

    let malformed = Netlist {
        inputs: vec!["d".to_string()],
        outputs: vec!["q".to_string()],
        gates: vec![Gate {
            name: "q".to_string(),
            inputs: vec!["d".to_string()],
            output: "q".to_string(),
            kind: GateKind::DffPosedge,
        }],
    };
    assert!(matches!(
        validate(&malformed),
        Err(EvalError::MalformedGate { .. })
    ));

    let cycle = Netlist {
        inputs: Vec::new(),
        outputs: vec!["y".to_string()],
        gates: vec![Gate::nor("y", &["x"]), Gate::nor("x", &["y"])],
    };
    assert_eq!(validate(&cycle), Err(EvalError::CombinationalCycle));

    let missing_input = Netlist {
        inputs: vec!["a".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![Gate::nor("y", &["a"])],
    };
    let evaluator = Evaluator::new(&missing_input).expect("evaluable");
    assert_eq!(
        evaluator.evaluate(&BTreeMap::new()),
        Err(EvalError::MissingInput("a".to_string()))
    );
}
