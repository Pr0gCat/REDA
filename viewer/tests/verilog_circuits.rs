//! Headless acceptance test for the Verilog-derived circuits reaching the
//! wasm-facing API -- the same "no browser, no wasm runtime, just `cargo
//! test`" arrangement as `and4_truth_table.rs`, and the same reasons for it.
//!
//! # What is actually being proved here
//!
//! These circuits cannot be synthesized in a `wasm32` build: no Python, no
//! Yosys, no subprocess. They reach this crate as `reda::circuits::verilog`'s
//! *baked* netlists instead (see `VerilogCircuit::baked_netlist`). That makes
//! two things worth checking that no existing test covers:
//!
//! 1. **The baked netlist is a working circuit, not just a well-formed
//!    file.** `reda`'s own `tests/verilog_frontend.rs` proves the *freshly
//!    synthesized* netlist against the truth table, and separately proves the
//!    baked file matches a fresh synthesis -- but both of those need Yosys.
//!    This drives the baked one through `Session`, all 16 input combinations,
//!    against the same truth table, on any machine.
//! 2. **The circuit the viewer loads is the one the size ladder names.**
//!    `verilog:seven_segment` is 31 gates and 7888 blocks; if either moved,
//!    every number this project quotes about the synthesised decoder would be
//!    quoting something else.
//!
//! Like `and4_truth_table.rs`, this never calls `Session::pinout` or
//! `Session::legend` (both return a `JsValue`, which aborts outside a real
//! wasm host), and learns output coordinates by compiling the same netlist
//! directly instead.

use reda::circuits::seven_segment::TRUTH_TABLE;
use reda::circuits::verilog;
use reda::compile::compile;
use reda::compile::lowering::lower;
use reda_viewer::{list_circuits, Axis, Session};

/// Bytes per cell in `Session::geometry`'s packed output -- the layout that
/// method documents (`[x, y, z, kind, facing, face, delay]`, coordinates as
/// little-endian `u16`). Restated here rather than exported, same as
/// `and4_truth_table.rs` restates `slice`'s layout: the point is to check the
/// documented contract, not the implementation against itself.
const GEOMETRY_BYTES_PER_CELL: usize = 10;

/// One cell's signal strength out of a `slice` result, using the row-major
/// layout `Session::slice` documents for `Axis::Z`.
fn strength_at(slice_bytes: &[u8], size_y: i32, coord: (i32, i32, i32)) -> u8 {
    let (x, y, _z) = coord;
    slice_bytes[2 * ((x * size_y + y) as usize) + 1]
}

/// Set every lever to the bits of `value` (MSB first over `inputs`), settle,
/// and read each output lamp back through `slice`.
fn evaluate(session: &mut Session, inputs: &[&str], value: u32, outputs: &[(i32, i32, i32)]) -> Vec<bool> {
    for (index, name) in inputs.iter().enumerate() {
        let bit = (value >> (inputs.len() - 1 - index)) & 1 == 1;
        session.set_lever(name, bit).expect("every input name comes from the netlist itself");
    }
    session.run_until_stable().expect("a synthesised circuit must settle");

    let size = session.size();
    outputs
        .iter()
        .map(|&position| {
            let bytes = session.slice(Axis::Z, position.2).expect("an output lamp is inside the world");
            strength_at(&bytes, size[1], position) > 0
        })
        .collect()
}

/// Every output lamp's coordinate, learned by compiling the same baked
/// netlist directly -- the same "two front doors onto one compiled circuit"
/// cross-check `and4_truth_table.rs` makes.
fn output_positions(circuit_name: &str) -> Vec<(i32, i32, i32)> {
    let circuit = verilog::find(circuit_name).expect("catalog entry must exist");
    let (netlist, labels) = circuit.baked_netlist();
    let netlist = lower(&netlist).expect("a baked netlist lowers");
    let compiled = compile(&netlist).expect("a baked netlist compiles");
    labels
        .iter()
        .map(|(_port, signal)| *compiled.output_positions.get(signal).expect("compile places every output"))
        .collect()
}

/// The hand-written size ladder first, then the Verilog catalog verbatim --
/// `verilog:` prefix included, because `verilog:seven_segment` and
/// `seven_segment` compute the same function out of entirely different gates
/// and a viewer showing one of them has to say which.
#[test]
fn list_circuits_reports_both_catalogs_with_the_verilog_prefix_intact() {
    let names = list_circuits();
    let expected_tail: Vec<String> = verilog::CIRCUITS.iter().map(|c| c.name.to_string()).collect();
    assert_eq!(
        names[names.len() - expected_tail.len()..],
        expected_tail[..],
        "the Verilog catalog must appear, in its own order, after the hand-written circuits; got {names:?}"
    );
    assert!(names.iter().any(|n| n == "seven_segment"), "the hand-written decoder must still be listed");
    assert!(names.iter().any(|n| n == "verilog:seven_segment"), "the synthesised decoder must be listed");
}

#[test]
fn the_verilog_and4_session_matches_its_truth_table_through_the_wasm_api() {
    let outputs = output_positions("verilog:and4");
    let mut session = Session::new("verilog:and4").expect("the baked and4 netlist builds a session");

    for value in 0..16u32 {
        let expected = value == 0b1111;
        let observed = evaluate(&mut session, &["a", "b", "c", "d"], value, &outputs);
        assert_eq!(
            observed,
            vec![expected],
            "verilog:and4 with inputs {value:04b} must output {expected}"
        );
    }
}

#[test]
fn the_verilog_seven_segment_session_matches_its_truth_table_through_the_wasm_api() {
    let outputs = output_positions("verilog:seven_segment");
    assert_eq!(outputs.len(), 7, "a seven-segment decoder has seven outputs");
    let mut session =
        Session::new("verilog:seven_segment").expect("the baked seven_segment netlist builds a session");

    for value in 0..16u32 {
        let expected: Vec<bool> = if (value as usize) < TRUTH_TABLE.len() {
            TRUTH_TABLE[value as usize].iter().map(|&bit| bit == 1).collect()
        } else {
            vec![false; 7]
        };
        let observed = evaluate(&mut session, &["d3", "d2", "d1", "d0"], value, &outputs);
        assert_eq!(observed, expected, "verilog:seven_segment on digit {value}");
    }
}

/// The synthesised decoder the viewer loads is the same circuit the rest of
/// this project quotes, at both of its levels: 31 gate-level cells as Yosys
/// left them, 56 torches and merges once `compile::lowering` has had them,
/// 12348 blocks once the compiler has. `geometry()`'s length is that block
/// count as the viewer itself sees it: one entry per non-air cell.
#[test]
fn the_verilog_seven_segment_is_the_size_the_ladder_says_it_is() {
    let (netlist, _) = verilog::find("verilog:seven_segment").expect("catalog entry").baked_netlist();
    assert_eq!(netlist.gates.len(), 31, "gate-level cell count has moved");
    assert_eq!(
        netlist.gates.iter().filter(|gate| gate.kind.is_realisable()).count(),
        9,
        "only 9 of the decoder's 31 cells are things redstone builds directly"
    );

    let lowered = lower(&netlist).expect("the decoder lowers");
    assert_eq!(lowered.gates.len(), 56, "lowered gate count has moved");
    assert_eq!(lowered.gates.iter().filter(|gate| gate.is_merge()).count(), 17);

    let session = Session::new("verilog:seven_segment").expect("session builds");
    let cells = session.geometry().len() / GEOMETRY_BYTES_PER_CELL;
    assert_eq!(cells, 12348, "the synthesised decoder's block count has moved");
    assert_eq!(session.geometry().len() % GEOMETRY_BYTES_PER_CELL, 0);
    assert_eq!(session.strengths().len(), cells, "one strength byte per geometry entry");
}
