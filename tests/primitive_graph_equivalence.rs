//! The deliverable check for
//! `docs/superpowers/specs/2026-08-08-primitive-level-flow.md`'s step 1:
//! "expanding today's netlists and re-emitting today's geometry must
//! produce byte-identical worlds", for all five of this project's reference
//! circuits, in both directions.
//!
//! `reda::compile::equivalence::verify_expansion_matches_compiled` checks
//! that everything the graph claims is present in the compiled world.
//! `reda::compile::world_partition::partition_world` checks the converse --
//! that every non-air block of the compiled world is accounted for, by a
//! graph node or by a narrowly-defined routing-fill rule, with nothing left
//! unexplained. See each module's own doc comment for exactly what it
//! verifies and why. `and4`, `full_adder`, `segment_a` and `seven_segment`
//! are also covered by unit tests inside `src/compile/equivalence.rs` and
//! `src/compile/world_partition.rs`; this file adds the fifth circuit (the
//! Yosys/ABC-synthesised decoder, which -- like every other test that needs
//! the Verilog frontend -- requires `python` and `yowasp-yosys`, so it lives
//! here rather than as a `src/` unit test), and collects all five in one
//! place so "checked for all five circuits" has one obvious home.

use reda::circuits::and4::build_and4_netlist;
use reda::circuits::full_adder::build_full_adder_netlist;
use reda::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
use reda::compile::equivalence::verify_expansion_matches_compiled;
use reda::compile::primitive_graph::expand;
use reda::compile::topology::Library;
use reda::compile::world_partition::partition_world;
use reda::compile::{compile, Netlist};
use reda::frontend::synthesize_verilog;

fn check(label: &str, netlist: &Netlist) {
    let compiled = compile(netlist).unwrap_or_else(|e| panic!("{label}: must compile: {e}"));
    let library = Library::default_library();
    let graph = expand(netlist, &library).unwrap_or_else(|e| panic!("{label}: must expand: {e}"));

    // Forward: everything the graph claims is present in the world.
    verify_expansion_matches_compiled(netlist, &graph, &compiled)
        .unwrap_or_else(|e| panic!("{label}: expansion does not match compiled world: {e}"));

    // Converse: everything in the world is accounted for by the graph, or
    // by a narrowly-defined routing-fill rule -- no block left unexplained.
    let partition = partition_world(netlist, &graph, &compiled)
        .unwrap_or_else(|e| panic!("{label}: could not partition the compiled world: {e}"));
    assert!(
        partition.unexplained.is_empty(),
        "{label}: {} block(s) neither explained by the graph nor routing fill: {:?}",
        partition.unexplained.len(),
        partition.unexplained
    );
    eprintln!(
        "{label}: {} blocks total -- {} graph-explained, {} routing fill (dust {}, repeater {}, support {}), \
         {} unexplained ({:.1}% explained by the graph)",
        partition.total_non_air(),
        partition.graph_explained,
        partition.routing_fill(),
        partition.routing_fill_dust,
        partition.routing_fill_repeater,
        partition.routing_fill_support,
        partition.unexplained.len(),
        partition.graph_explained_ratio().unwrap_or(0.0) * 100.0,
    );
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
