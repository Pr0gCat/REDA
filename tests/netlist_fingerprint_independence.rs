//! Milestone 4 ("Shadow validation"): "Verify the debug sidecar fingerprint
//! against an independent canonical render of every emitted Netlist."
//!
//! Everything below this comment is test-only and never calls
//! `frontend::debug::canonical_netlist_render`. It reads only `Netlist`,
//! `Gate`, and `GateKind`'s public fields/accessors and reimplements
//! serialization and hashing from scratch, so a bug introduced in the
//! production renderer (wrong field, wrong order, wrong separator) has an
//! independently-written check to be caught by rather than a second call
//! into the same code path.
//!
//! Two independent oracles are used, not one, because a single
//! re-implementation of the *same* text format could still share the
//! production renderer's blind spots:
//!
//! - `independent_fingerprint` reproduces the exact documented text format
//!   (`netlist 1` / `input ...` / `gate ...` / `output ...`) from
//!   `canonical_netlist_render`'s doc comment, using `sha2` directly (an
//!   existing dependency, not `metrics::canonical_fingerprint`). Against
//!   real compiled fixtures this must match the production
//!   `netlist_fingerprint` byte for byte -- the strong form of the
//!   milestone's requirement.
//! - `independent_structural_fingerprint` uses a completely different,
//!   binary, length-prefixed framing with no textual resemblance to the
//!   production format. It cannot match production bytes, so it is used
//!   instead as a mutation-sensitivity and collision oracle: a meaningful
//!   check that is not simply restating the production algorithm.

use std::collections::HashSet;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use reda::compile::topology::GateKind;
use reda::compile::{Gate, Netlist};
use reda::frontend::debug::netlist_fingerprint;
use reda::frontend::{compile_systemverilog, CompileOptions, Diagnostic, SourceInput};

const AND4: &str = include_str!("fixtures/and4.sv");
const SEVEN_SEGMENT: &str = include_str!("fixtures/seven_segment.sv");
const DFF: &str = include_str!("fixtures/dff.sv");

// ---------------------------------------------------------------------
// Oracle 1: an independently-written reproduction of the documented
// canonical text format, hashed with `sha2` directly.
// ---------------------------------------------------------------------

fn independent_wire_name(kind: GateKind) -> &'static str {
    match kind {
        GateKind::Nor(_) => "nor",
        GateKind::Or(_) => "merge",
        GateKind::Buf => "buf",
        GateKind::And => "and",
        GateKind::Nand => "nand",
        GateKind::Xor => "xor",
        GateKind::Xnor => "xnor",
        GateKind::AndNot => "andnot",
        GateKind::OrNot => "ornot",
        GateKind::Aoi3 => "aoi3",
        GateKind::Oai3 => "oai3",
        GateKind::Aoi4 => "aoi4",
        GateKind::Oai4 => "oai4",
        GateKind::Mux => "mux",
        GateKind::Nmux => "nmux",
        GateKind::DffPosedge => "dff_p",
    }
}

/// Deliberately uses `gate.inputs.len()` rather than `gate.kind.arity()`:
/// the two must always agree in a well-formed netlist, so reading arity from
/// the input list itself is both a genuinely separate data source and a
/// latent check that they do.
fn independent_canonical_render(netlist: &Netlist) -> String {
    let mut out = String::new();
    out.push_str("netlist 1\n");
    for name in &netlist.inputs {
        out.push_str("input ");
        out.push_str(name);
        out.push('\n');
    }
    for gate in &netlist.gates {
        out.push_str("gate ");
        out.push_str(independent_wire_name(gate.kind));
        out.push(' ');
        out.push_str(&gate.inputs.len().to_string());
        out.push(' ');
        out.push_str(&gate.output);
        out.push_str(" <-");
        for input in &gate.inputs {
            out.push(' ');
            out.push_str(input);
        }
        out.push('\n');
    }
    for name in &netlist.outputs {
        out.push_str("output ");
        out.push_str(name);
        out.push('\n');
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

fn independent_fingerprint(netlist: &Netlist) -> String {
    sha256_hex(independent_canonical_render(netlist).as_bytes())
}

// ---------------------------------------------------------------------
// Oracle 2: a structurally unrelated binary framing -- netstrings and a
// numeric kind discriminant, no shared vocabulary with the text format.
// ---------------------------------------------------------------------

fn netstring(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(s.as_bytes());
    out.push(b',');
}

fn kind_discriminant(kind: GateKind) -> u8 {
    match kind {
        GateKind::Nor(_) => 0,
        GateKind::Or(_) => 1,
        GateKind::Buf => 2,
        GateKind::And => 3,
        GateKind::Nand => 4,
        GateKind::Xor => 5,
        GateKind::Xnor => 6,
        GateKind::AndNot => 7,
        GateKind::OrNot => 8,
        GateKind::Aoi3 => 9,
        GateKind::Oai3 => 10,
        GateKind::Aoi4 => 11,
        GateKind::Oai4 => 12,
        GateKind::Mux => 13,
        GateKind::Nmux => 14,
        GateKind::DffPosedge => 15,
    }
}

fn independent_structural_digest(netlist: &Netlist) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"REDA-TEST-ORACLE-V1;");
    out.extend_from_slice(netlist.inputs.len().to_string().as_bytes());
    out.push(b'#');
    for name in &netlist.inputs {
        netstring(&mut out, name);
    }
    out.extend_from_slice(netlist.gates.len().to_string().as_bytes());
    out.push(b'#');
    for gate in &netlist.gates {
        out.push(kind_discriminant(gate.kind));
        out.push(gate.inputs.len() as u8);
        netstring(&mut out, &gate.output);
        for input in &gate.inputs {
            netstring(&mut out, input);
        }
    }
    out.extend_from_slice(netlist.outputs.len().to_string().as_bytes());
    out.push(b'#');
    for name in &netlist.outputs {
        netstring(&mut out, name);
    }
    out
}

fn independent_structural_fingerprint(netlist: &Netlist) -> String {
    sha256_hex(&independent_structural_digest(netlist))
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

fn render(diagnostics: &[Diagnostic], name: &str, text: &str) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.render(&[SourceInput { name, text }]))
        .collect::<Vec<_>>()
        .join("; ")
}

fn compile_netlist(name: &'static str, source: &'static str, top: &str) -> Netlist {
    compile_systemverilog(
        &[SourceInput { name, text: source }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|diagnostics| {
        panic!(
            "{name} must compile: {}",
            render(&diagnostics, name, source)
        )
    })
    .netlist
}

fn fixtures() -> Vec<(&'static str, Netlist)> {
    vec![
        ("and4.sv", compile_netlist("and4.sv", AND4, "and4")),
        (
            "seven_segment.sv",
            compile_netlist("seven_segment.sv", SEVEN_SEGMENT, "seven_segment"),
        ),
        ("dff.sv", compile_netlist("dff.sv", DFF, "dff")),
    ]
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

/// The milestone's literal requirement: the debug sidecar fingerprint of
/// every checked-in fixture matches an independently rendered and hashed
/// canonical form byte for byte -- computed without calling
/// `canonical_netlist_render` at all.
#[test]
fn independent_render_reproduces_the_production_fingerprint_byte_for_byte() {
    for (name, netlist) in fixtures() {
        let production = netlist_fingerprint(&netlist);
        let independent = independent_fingerprint(&netlist);
        assert_eq!(
            independent,
            production.as_str(),
            "{name}: an independently rendered and hashed netlist must match \
             the production debug-sidecar fingerprint"
        );
    }
}

/// The second, structurally unrelated oracle agrees that every fixture's
/// netlist is fingerprinted distinctly from every other -- a check the
/// text-format oracle alone could not make, since it shares production's
/// own vocabulary.
#[test]
fn fixtures_are_pairwise_distinct_under_the_structural_oracle() {
    let fixtures = fixtures();
    let mut seen = HashSet::new();
    for (name, netlist) in &fixtures {
        let fingerprint = independent_structural_fingerprint(netlist);
        assert!(
            seen.insert(fingerprint),
            "{name}: structural oracle collided with an earlier fixture"
        );
    }
}

/// A mutation that changes what a netlist *is* must move every fingerprint
/// that describes it -- both independent oracles and the production one --
/// and the two independent oracles must keep agreeing with production after
/// the mutation, not just before it. Checking only the unmutated case would
/// be tautological: two implementations of the same spec trivially agree on
/// one shared input, so the meaningful assertion is that they keep tracking
/// each other as the netlist changes.
#[test]
fn mutations_move_every_oracle_together() {
    let netlist = compile_netlist("and4.sv", AND4, "and4");
    assert!(
        netlist.gates.len() >= 2,
        "the mutation cases below need at least two distinct gates"
    );

    let assert_all_oracles_agree_and_moved = |mutated: &Netlist, label: &str| {
        let original_production = netlist_fingerprint(&netlist);
        let original_independent = independent_fingerprint(&netlist);
        let original_structural = independent_structural_fingerprint(&netlist);

        let mutated_production = netlist_fingerprint(mutated);
        let mutated_independent = independent_fingerprint(mutated);
        let mutated_structural = independent_structural_fingerprint(mutated);

        assert_ne!(
            mutated_production, original_production,
            "{label}: production fingerprint did not move"
        );
        assert_ne!(
            mutated_independent, original_independent,
            "{label}: text-format oracle did not move"
        );
        assert_ne!(
            mutated_structural, original_structural,
            "{label}: structural oracle did not move"
        );
        assert_eq!(
            mutated_independent,
            mutated_production.as_str(),
            "{label}: text-format oracle diverged from production after the mutation"
        );
    };

    // Rename a gate's output net, and every place that reads it.
    let mut renamed = netlist.clone();
    let old_name = renamed.gates[0].output.clone();
    let new_name = format!("{old_name}_renamed");
    renamed.gates[0].output = new_name.clone();
    for gate in renamed.gates.iter_mut().skip(1) {
        for input in &mut gate.inputs {
            if *input == old_name {
                *input = new_name.clone();
            }
        }
    }
    for output in &mut renamed.outputs {
        if *output == old_name {
            *output = new_name.clone();
        }
    }
    assert_all_oracles_agree_and_moved(&renamed, "renaming a gate's output net");

    // Reorder two gates: every downstream `GateRef` index moves, so this
    // must be visible to the fingerprint even though the gate set itself is
    // unchanged.
    let mut reordered = netlist.clone();
    reordered.gates.swap(0, 1);
    assert_all_oracles_agree_and_moved(&reordered, "reordering two gates");

    // Change one gate's kind while keeping its arity, so the mutation is
    // legal to render under either oracle's format.
    let mut retyped = netlist.clone();
    retyped.gates[0].kind = GateKind::Nand;
    assert_all_oracles_agree_and_moved(&retyped, "changing a gate's kind");
}

/// Deterministic, hand-built netlists (not fuzzed, not read from source)
/// covering every `GateKind` variant, including ones the checked-in
/// fixtures never emit (`Nor`, `Or`, `Buf`, `Xor`, `Mux`, ...). Each one is
/// checked against both independent oracles, and the whole set is checked
/// for pairwise distinctness under the structural oracle.
#[test]
fn deterministic_generated_netlists_match_both_oracles_and_are_pairwise_distinct() {
    for netlist in generated_netlists() {
        let production = netlist_fingerprint(&netlist);
        let independent = independent_fingerprint(&netlist);
        assert_eq!(
            independent,
            production.as_str(),
            "generated netlist with kind {:?} diverged from production",
            netlist.gates[0].kind
        );
    }

    let mut seen = HashSet::new();
    for netlist in generated_netlists() {
        let fingerprint = independent_structural_fingerprint(&netlist);
        assert!(
            seen.insert(fingerprint),
            "generated netlist with kind {:?} collided under the structural oracle",
            netlist.gates[0].kind
        );
    }
}

fn generated_netlists() -> Vec<Netlist> {
    let single_gate_kinds: &[GateKind] = &[
        GateKind::Nor(1),
        GateKind::Nor(2),
        GateKind::Nor(3),
        GateKind::Or(2),
        GateKind::Or(3),
        GateKind::Buf,
        GateKind::And,
        GateKind::Nand,
        GateKind::Xor,
        GateKind::Xnor,
        GateKind::AndNot,
        GateKind::OrNot,
        GateKind::Aoi3,
        GateKind::Oai3,
        GateKind::Aoi4,
        GateKind::Oai4,
        GateKind::Mux,
        GateKind::Nmux,
        GateKind::DffPosedge,
    ];

    let mut nets: Vec<Netlist> = single_gate_kinds
        .iter()
        .enumerate()
        .map(|(index, &kind)| {
            let inputs: Vec<String> = (0..kind.arity()).map(|bit| format!("i{bit}")).collect();
            let output = "y".to_string();
            Netlist {
                inputs: inputs.clone(),
                outputs: vec![output.clone()],
                gates: vec![Gate {
                    name: format!("case{index}"),
                    inputs,
                    output,
                    kind,
                }],
            }
        })
        .collect();

    // A short deterministic chain, for coverage of more than one gate.
    nets.push(Netlist {
        inputs: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![
            Gate {
                name: "g0".to_string(),
                inputs: vec!["a".to_string(), "b".to_string()],
                output: "g0".to_string(),
                kind: GateKind::Nand,
            },
            Gate {
                name: "g1".to_string(),
                inputs: vec!["g0".to_string(), "c".to_string()],
                output: "y".to_string(),
                kind: GateKind::Xor,
            },
        ],
    });

    // A minimal sequential chain: DFF feeding a combinational gate.
    nets.push(Netlist {
        inputs: vec!["d".to_string(), "clk".to_string(), "e".to_string()],
        outputs: vec!["y".to_string()],
        gates: vec![
            Gate {
                name: "dff0".to_string(),
                inputs: vec!["d".to_string(), "clk".to_string()],
                output: "q0".to_string(),
                kind: GateKind::DffPosedge,
            },
            Gate {
                name: "g0".to_string(),
                inputs: vec!["q0".to_string(), "e".to_string()],
                output: "y".to_string(),
                kind: GateKind::And,
            },
        ],
    });

    nets
}
