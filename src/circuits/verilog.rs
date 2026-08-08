//! The Verilog-source counterparts of this module's hand-written reference
//! circuits, as a small catalog the binaries can select from by name.
//!
//! Everything else in [`crate::circuits`] is a netlist this project builds
//! itself, gate by gate. These are not: they are HDL sources handed to
//! [`crate::frontend::synthesize_verilog`], which shells out to Yosys and
//! technology-maps the result onto the same NOR cell library. Same target
//! functions, entirely different provenance -- which is exactly why they are
//! kept in their own catalog with their own `verilog:`-prefixed names rather
//! than being added to `available_circuits()` alongside `and4` and
//! `seven_segment` in `mc_dump`/`build_circuit`:
//!
//! * They cannot be built by a `fn() -> Netlist`. Synthesis can fail (no
//!   `python`, no `yowasp-yosys`, a Yosys-level error), so every one of these
//!   is fallible in a way no hand-written circuit is, and the callers have to
//!   see that difference rather than have it hidden behind an `expect`.
//! * They are slow. A hand-written netlist is built in microseconds; each of
//!   these spawns a Python process running a WASM build of Yosys. A
//!   `build_circuit` with no arguments compiles every hand-written circuit
//!   just to print its bounding box -- doing that to these would turn a
//!   listing into a multi-second Yosys run that fails outright on a machine
//!   without the toolchain.
//! * `verilog:and4` and `and4` are genuinely two different circuits that
//!   compute the same function. Giving them one name, or giving the
//!   synthesised one a name that looks like a `src/circuits/` entry, would
//!   make a conformance result ambiguous about which of the two it came
//!   from -- and telling those two apart is the entire point of running the
//!   synthesised one through a real server.
//!
//! # Why the sources are `include_str!`'d out of `tests/fixtures/`
//!
//! Those files are already the canonical Verilog for these two circuits, and
//! `tests/verilog_frontend.rs` checks the netlists synthesised from them
//! against the same truth tables the hand-written circuits are checked
//! against. Embedding the very same bytes (rather than copying them to a
//! second location, or reading them off disk relative to the current working
//! directory) is what guarantees the circuit a binary emits into Minecraft is
//! synthesised from exactly the source the acceptance test proved correct in
//! the simulator -- and it keeps these binaries runnable from any directory.
//! `source_is_the_fixture_the_acceptance_test_uses` below fails if that ever
//! stops being true.

use std::collections::HashMap;
use std::path::Path;

use crate::compile::Netlist;
use crate::frontend::{synthesize_verilog, FrontendError};

/// A Verilog source this project ships, plus the top module to synthesize
/// out of it.
pub struct VerilogCircuit {
    /// The name a binary's CLI selects this circuit by. Always
    /// `verilog:`-prefixed, so it can never be confused with a
    /// [`crate::circuits`] entry.
    pub name: &'static str,
    /// The HDL itself, embedded at build time.
    pub source: &'static str,
    /// The module within `source` to synthesize. Not derivable from `name`:
    /// `verilog:seven_segment` is deliberately named after the hand-written
    /// circuit it mirrors, while its module is called `bcd_seven_segment`.
    pub top_module: &'static str,
}

/// Every Verilog circuit this project ships, in the same smallest-to-largest
/// order as [`crate::circuits`]'s own size ladder.
pub const CIRCUITS: &[VerilogCircuit] = &[
    VerilogCircuit {
        name: "verilog:and4",
        source: include_str!("../../tests/fixtures/and4.v"),
        top_module: "and4",
    },
    VerilogCircuit {
        name: "verilog:seven_segment",
        source: include_str!("../../tests/fixtures/seven_segment.v"),
        top_module: "bcd_seven_segment",
    },
];

/// Look up a shipped Verilog circuit by its CLI name, e.g.
/// `"verilog:seven_segment"`.
pub fn find(name: &str) -> Option<&'static VerilogCircuit> {
    CIRCUITS.iter().find(|circuit| circuit.name == name)
}

impl VerilogCircuit {
    /// Synthesize this circuit. See [`synthesize`] for the return shape.
    pub fn synthesize(&self) -> Result<(Netlist, Vec<(String, String)>), FrontendError> {
        synthesize(self.source, self.top_module)
    }
}

/// Synthesize `top_module` out of `source`, returning the netlist together
/// with its output labels: `(port name, internal signal name)` pairs sorted
/// by port name.
///
/// That pair list is the same shape `mc_dump`/`build_circuit` already carry
/// for the hand-written circuits, and exists for the same reason -- gate-tree
/// construction invents the names in [`Netlist::outputs`], so `y` or `a`..`g`
/// has to be recovered from somewhere. For a synthesised netlist that
/// somewhere is the frontend's port map, whose keys are the top module's own
/// declared output ports; sorting by port name is what makes `a`..`g` come
/// out in segment order rather than `HashMap` order.
pub fn synthesize(
    source: &str,
    top_module: &str,
) -> Result<(Netlist, Vec<(String, String)>), FrontendError> {
    let (netlist, port_map) = synthesize_verilog(source, top_module)?;
    Ok((netlist, sorted_output_labels(port_map)))
}

/// Read `path` and synthesize `top_module` out of it -- the same as
/// [`synthesize`], for a Verilog file that is not one of the ones this
/// project ships.
pub fn synthesize_file(
    path: &Path,
    top_module: &str,
) -> Result<(Netlist, Vec<(String, String)>), FrontendError> {
    let source = std::fs::read_to_string(path)?;
    synthesize(&source, top_module)
}

fn sorted_output_labels(port_map: HashMap<String, String>) -> Vec<(String, String)> {
    let mut labels: Vec<(String, String)> = port_map.into_iter().collect();
    labels.sort();
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every catalog name is unique and `verilog:`-prefixed, so `find` is
    /// unambiguous and no entry can ever shadow a hand-written circuit's
    /// name in a binary that searches both lists.
    #[test]
    fn catalog_names_are_unique_and_prefixed() {
        let mut names: Vec<&str> = CIRCUITS.iter().map(|c| c.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "two catalog entries share a name");
        for circuit in CIRCUITS {
            assert!(
                circuit.name.starts_with("verilog:"),
                "{} is not verilog:-prefixed",
                circuit.name
            );
            assert!(find(circuit.name).is_some(), "{} is not findable", circuit.name);
        }
        assert!(find("and4").is_none(), "a hand-written circuit's name must not resolve here");
    }

    /// The embedded source is byte-identical to the fixture
    /// `tests/verilog_frontend.rs` reads off disk. If that ever stops being
    /// true, the circuit a binary emits into Minecraft is no longer the one
    /// the acceptance test proved correct in the simulator -- which is the
    /// entire justification for embedding these rather than copying them
    /// (see this module's doc comment).
    #[test]
    fn source_is_the_fixture_the_acceptance_test_uses() {
        for (name, fixture) in [
            ("verilog:and4", "tests/fixtures/and4.v"),
            ("verilog:seven_segment", "tests/fixtures/seven_segment.v"),
        ] {
            let circuit = find(name).expect("catalog entry must exist");
            let on_disk = std::fs::read_to_string(fixture)
                .unwrap_or_else(|err| panic!("{fixture} must be readable: {err}"));
            assert_eq!(
                circuit.source, on_disk,
                "{name}'s embedded source has drifted from {fixture}"
            );
        }
    }

    /// The top module named here is the one the fixture actually declares --
    /// a typo would otherwise only surface as a Yosys error at the moment
    /// someone tries to emit the circuit.
    #[test]
    fn each_top_module_is_declared_by_its_own_source() {
        for circuit in CIRCUITS {
            let declaration = format!("module {}(", circuit.top_module);
            assert!(
                circuit.source.contains(&declaration),
                "{}'s source declares no `{declaration}`",
                circuit.name
            );
        }
    }
}
