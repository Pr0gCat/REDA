//! Re-synthesizes every circuit in `reda::circuits::verilog`'s catalog and
//! rewrites its checked-in baked netlist.
//!
//! Run with `cargo run --release --bin bake_verilog` from the repo root (the
//! paths written are `VerilogCircuit::baked_path`, which are relative to it).
//!
//! # Why a baked netlist exists at all
//!
//! Synthesis needs Python, `yowasp-yosys`, a temp directory and the ability
//! to spawn a process. The browser viewer is a `wasm32-unknown-unknown`
//! build with none of the four, and a Verilog-derived netlist is the only
//! circuit this project has that is not pure NOR -- the only one whose
//! topology view shows anything but a wall of identical gates. So the
//! *netlist*, not the synthesis, is what has to reach the viewer, and this
//! is the point where Yosys can actually run to produce it. See
//! `VerilogCircuit::baked_netlist` for the full argument and, more
//! importantly, for the two tests that make a stale bake fail loudly rather
//! than quietly misrepresent the compiler.
//!
//! This is a developer tool, not part of any build: nothing in `cargo build`
//! or `cargo test` runs it. Re-run it by hand after changing a fixture under
//! `tests/fixtures/`, the genlib, or anything in `reda::frontend` that
//! changes what comes out the other side -- and commit the diff, which is
//! the whole reason the format is a readable one (see
//! `reda::circuits::verilog::baked`).

use std::process::ExitCode;

use reda::circuits::verilog::{baked, CIRCUITS};

fn main() -> ExitCode {
    let mut changed = 0usize;
    for circuit in CIRCUITS {
        eprintln!("synthesizing {} from {} ...", circuit.name, circuit.source_path);
        let (netlist, output_labels) = match circuit.synthesize() {
            Ok(result) => result,
            Err(error) => {
                // The one failure a user actually hits is a missing
                // toolchain, and `FrontendError`'s own Display already says
                // exactly that -- so print it and stop rather than leaving
                // half the catalog rewritten.
                eprintln!("error: {} could not be synthesized: {error}", circuit.name);
                return ExitCode::FAILURE;
            }
        };

        let rendered = match baked::render(circuit, &netlist, &output_labels) {
            Ok(text) => text,
            Err(error) => {
                eprintln!("error: {}'s netlist {error}", circuit.name);
                return ExitCode::FAILURE;
            }
        };

        let previous = std::fs::read_to_string(circuit.baked_path).unwrap_or_default();
        if previous == rendered {
            eprintln!("  {} is already up to date ({} gates)", circuit.baked_path, netlist.gates.len());
            continue;
        }
        if let Err(error) = std::fs::write(circuit.baked_path, &rendered) {
            eprintln!("error: could not write {}: {error}", circuit.baked_path);
            return ExitCode::FAILURE;
        }
        let merges = netlist.gates.iter().filter(|gate| gate.is_merge).count();
        eprintln!(
            "  wrote {} -- {} gates, {merges} of them wire merges",
            circuit.baked_path,
            netlist.gates.len()
        );
        changed += 1;
    }

    eprintln!("{changed} of {} baked netlist(s) rewritten.", CIRCUITS.len());
    ExitCode::SUCCESS
}
