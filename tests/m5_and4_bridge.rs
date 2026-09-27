//! Milestone 5, exit criterion 7 (`docs/native-wasm-verilog-compiler-plan.md`,
//! "Production cutover is a no-go until all of these are true"):
//!
//! > A bridge test compiles `and4`, joins every gate-owned primitive back
//! > through `(gate index, output)` to a source span, joins ports by name,
//! > and rejects a deliberately mismatched Netlist fingerprint.
//!
//! This is preparation, not cutover: nothing here calls `synthesize_verilog`,
//! switches a production caller, or regenerates a baked netlist. It only
//! proves the generator-bridge contract the plan's debug section describes --
//! `GateRef { index, output }` plus the canonical fingerprint -- holds for a
//! real compiled fixture, the same proof criterion 7 asks for before
//! Milestone 5 may begin.

use reda::frontend::debug::GateRef;
use reda::frontend::{
    compile_systemverilog, CompileArtifact, CompileOptions, PortDirection, SourceInput,
};

const AND4: &str = include_str!("fixtures/and4.sv");

fn compile() -> CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "and4.sv",
            text: AND4,
        }],
        &CompileOptions::new("and4"),
    )
    .unwrap_or_else(|diagnostics| panic!("and4.sv should compile: {diagnostics:?}"))
}

/// Every gate REDA emitted joins back to a source span through nothing but
/// its `(index, output)` pair -- the same pair `Realisation::Gate` and the
/// future primitive/placement/route maps already key on, per the plan's
/// "compiler-generator bridge" section.
#[test]
fn every_gate_joins_by_index_and_output_to_a_source_span() {
    let artifact = compile();
    assert!(
        !artifact.netlist.gates.is_empty(),
        "and4 must emit at least one gate"
    );

    for (index, gate) in artifact.netlist.gates.iter().enumerate() {
        let gate_ref = GateRef {
            index: index as u32,
            output: gate.output.clone(),
        };
        let origins = artifact.debug.origins_for_gate(&gate_ref);
        assert!(
            !origins.is_empty(),
            "gate {index} ({}) has no source origin via its (index, output) pair",
            gate.output
        );
        for origin in &origins {
            assert!(
                AND4.get(origin.span.start as usize..origin.span.end as usize)
                    .is_some(),
                "gate {index}'s origin span {:?} does not fall inside and4.sv",
                origin.span
            );
        }
    }
}

/// Ports join by declared name, not by position -- `CompileArtifact::ports`
/// is sorted for transport, so a caller (and this test) must look every port
/// up by name rather than assume `ports[i]` lines up with `netlist.inputs[i]`
/// or `netlist.outputs[i]`.
#[test]
fn ports_join_by_name_to_netlist_signals() {
    let artifact = compile();

    for name in ["a", "b", "c", "d"] {
        let port = artifact
            .port(name)
            .unwrap_or_else(|| panic!("and4 declares input `{name}`"));
        assert_eq!(port.direction, PortDirection::Input);
        assert!(
            artifact.netlist.inputs.contains(&port.signal),
            "input port `{name}` joins to signal `{}`, which is not in netlist.inputs",
            port.signal
        );
    }

    let y = artifact.port("y").expect("and4 declares output `y`");
    assert_eq!(y.direction, PortDirection::Output);
    assert!(
        artifact.netlist.outputs.contains(&y.signal),
        "output port `y` joins to signal `{}`, which is not in netlist.outputs",
        y.signal
    );
    assert_eq!(artifact.output_map()["y"], y.signal);
}

/// The other half of the same bridge: a `DebugDatabase` must refuse to be
/// joined against a `Netlist` it does not describe. `matches` is the gate
/// every future join (today just this test; later `PhysicalDebugArtifact`)
/// has to pass first, per the plan's debug-and-provenance contract: "A
/// mismatched fingerprint means the two halves describe different netlists
/// and must be rejected rather than joined."
#[test]
fn a_mismatched_netlist_fingerprint_is_rejected() {
    let artifact = compile();
    assert!(artifact.debug.matches(&artifact.netlist));

    // Reordering gates moves every (index, output) pair's index without
    // changing any gate's own fields -- exactly the kind of mismatch a
    // caller could otherwise not detect except by re-deriving the
    // fingerprint by hand.
    let mut reordered = artifact.netlist.clone();
    reordered.gates.reverse();
    assert!(
        !artifact.debug.matches(&reordered),
        "a reordered netlist must not match the original sidecar's fingerprint"
    );

    // A deliberately different circuit's sidecar must not match and4's
    // netlist either.
    let and2 = compile_systemverilog(
        &[SourceInput {
            name: "and2.sv",
            text: include_str!("fixtures/and2.sv"),
        }],
        &CompileOptions::new("and2"),
    )
    .expect("and2.sv should compile");
    assert!(!and2.debug.matches(&artifact.netlist));
    assert!(!artifact.debug.matches(&and2.netlist));
}
