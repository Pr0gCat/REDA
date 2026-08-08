//! The deliverable check for
//! `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`'s step 1:
//! "expanding today's netlists and re-emitting today's geometry must
//! produce byte-identical worlds", for all five of this project's reference
//! circuits.
//!
//! `reda::compile::equivalence::verify_expansion_matches_compiled` is the
//! actual check -- see its module doc comment for what it verifies and why
//! (expand + compare against `compile`'s existing output, not a second
//! emitter). `and4`, `full_adder`, `segment_a` and `seven_segment` are also
//! covered by unit tests inside `src/compile/equivalence.rs`; this file adds
//! the fifth circuit (the Yosys/ABC-synthesised decoder, which -- like every
//! other test that needs the Verilog frontend -- requires `python` and
//! `yowasp-yosys`, so it lives here rather than as a `src/` unit test), and
//! collects all five in one place so "checked for all five circuits" has one
//! obvious home.

use reda::circuits::and4::build_and4_netlist;
use reda::circuits::full_adder::build_full_adder_netlist;
use reda::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
use reda::compile::equivalence::verify_expansion_matches_compiled;
use reda::compile::primitive_graph::expand;
use reda::compile::topology::Library;
use reda::compile::{compile, Netlist};
use reda::frontend::synthesize_verilog;

fn check(label: &str, netlist: &Netlist) {
    let compiled = compile(netlist).unwrap_or_else(|e| panic!("{label}: must compile: {e}"));
    let library = Library::default_library();
    let graph = expand(netlist, &library).unwrap_or_else(|e| panic!("{label}: must expand: {e}"));
    verify_expansion_matches_compiled(netlist, &graph, &compiled)
        .unwrap_or_else(|e| panic!("{label}: expansion does not match compiled world: {e}"));
}

#[test]
fn and4_expansion_matches_its_compiled_world() {
    let (netlist, _output) = build_and4_netlist();
    check("and4", &netlist);
}

#[test]
fn full_adder_expansion_matches_its_compiled_world() {
    let (netlist, _outputs) = build_full_adder_netlist();
    check("full_adder", &netlist);
}

#[test]
fn segment_a_expansion_matches_its_compiled_world() {
    let (netlist, _output) = build_single_segment_netlist(0);
    check("segment_a", &netlist);
}

#[test]
fn seven_segment_expansion_matches_its_compiled_world() {
    let (netlist, _outputs) = build_seven_segment_netlist();
    check("seven_segment", &netlist);
}

#[test]
fn verilog_seven_segment_expansion_matches_its_compiled_world() {
    let source = std::fs::read_to_string("tests/fixtures/seven_segment.v").expect("fixture must exist");
    let (netlist, _port_map) =
        synthesize_verilog(&source, "bcd_seven_segment").expect("seven_segment.v must synthesize");
    check("verilog seven_segment", &netlist);
}
