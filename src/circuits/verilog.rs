//! The Verilog-source counterparts of this module's hand-written reference
//! circuits, as a small catalog the binaries can select from by name.
//!
//! Everything else in [`crate::circuits`] is a netlist this project builds
//! itself, gate by gate. These are not: they are HDL sources handed to
//! [`crate::frontend::synthesize_verilog`], which shells out to Yosys and
//! reads back the gate-level netlist ABC's logic optimisation leaves behind
//! (`compile::lowering` turns that into redstone, not ABC -- see
//! `crate::frontend`'s own doc comment). Same target functions, entirely
//! different provenance -- which is exactly why they are
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
//!
//! # And why the *netlists* are embedded too
//!
//! Everything above assumes the caller can run Yosys. The browser viewer
//! cannot: it is a `wasm32-unknown-unknown` build with no Python, no
//! subprocess and no filesystem, and it is also the one consumer for which
//! these circuits matter most -- every hand-written circuit is pure NOR, so
//! a synthesised netlist is the only thing its topology view has to show
//! that is not a wall of identical gates. So each entry also carries the
//! netlist Yosys produced, in the readable text format [`baked`] defines,
//! written by `cargo run --bin bake_verilog` and checked in.
//!
//! A checked-in synthesis result is a claim about this compiler's own
//! output, so the interesting question is not how it is produced but what
//! stops a stale one from quietly misrepresenting the compiler. See
//! [`VerilogCircuit::baked_netlist`] for the two tests that answer it.

use std::collections::HashMap;
use std::path::Path;

use crate::compile::Netlist;
use crate::frontend::{synthesize_verilog, FrontendError};

/// A Verilog source this project ships, plus the top module to synthesize
/// out of it and the synthesis result baked at development time.
pub struct VerilogCircuit {
    /// The name a binary's CLI selects this circuit by. Always
    /// `verilog:`-prefixed, so it can never be confused with a
    /// [`crate::circuits`] entry.
    pub name: &'static str,
    /// The HDL itself, embedded at build time.
    pub source: &'static str,
    /// Where [`VerilogCircuit::source`] was embedded from, relative to the
    /// repository root. Carried as data (rather than only appearing in an
    /// `include_str!` path, which is relative to *this file*) so that the
    /// baker can record it in a baked file's header and
    /// `source_is_the_fixture_the_acceptance_test_uses` can iterate the
    /// catalog instead of restating the pairing in a second table.
    pub source_path: &'static str,
    /// The module within `source` to synthesize. Not derivable from `name`:
    /// `verilog:seven_segment` is deliberately named after the hand-written
    /// circuit it mirrors, while its module is called `bcd_seven_segment`.
    pub top_module: &'static str,
    /// The netlist Yosys produced for this circuit, in [`baked`]'s own text
    /// format, embedded at build time. See [`VerilogCircuit::baked_netlist`]
    /// for why this exists at all and what keeps it honest.
    pub baked: &'static str,
    /// Where [`VerilogCircuit::baked`] was embedded from, relative to the
    /// repository root -- the file `bake_verilog` rewrites. Same reason as
    /// `source_path` for carrying it as data.
    pub baked_path: &'static str,
}

/// Every Verilog circuit this project ships, in the same smallest-to-largest
/// order as [`crate::circuits`]'s own size ladder.
pub const CIRCUITS: &[VerilogCircuit] = &[
    VerilogCircuit {
        name: "verilog:and4",
        source: include_str!("../../tests/fixtures/and4.v"),
        source_path: "tests/fixtures/and4.v",
        top_module: "and4",
        baked: include_str!("baked/and4.netlist"),
        baked_path: "src/circuits/baked/and4.netlist",
    },
    VerilogCircuit {
        name: "verilog:seven_segment",
        source: include_str!("../../tests/fixtures/seven_segment.v"),
        source_path: "tests/fixtures/seven_segment.v",
        top_module: "bcd_seven_segment",
        baked: include_str!("baked/seven_segment.netlist"),
        baked_path: "src/circuits/baked/seven_segment.netlist",
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

    /// This circuit's netlist **without running Yosys** -- the baked copy in
    /// [`VerilogCircuit::baked`], parsed. Same return shape as
    /// [`VerilogCircuit::synthesize`].
    ///
    /// # Why a baked copy exists
    ///
    /// [`synthesize`] needs a Python interpreter, the `yowasp-yosys`
    /// package, a writable temp directory and the ability to spawn a process.
    /// The browser viewer (`viewer/`) is a `wasm32-unknown-unknown` build
    /// with none of the four, and yet a Verilog-derived netlist is the only
    /// circuit this project has that is not pure NOR -- the only one whose
    /// topology view shows anything besides a wall of identical gates. So
    /// the netlist, not the synthesis, is what has to reach the viewer, and
    /// something has to produce it at a point where Yosys can actually run.
    /// That point is a developer's machine: `cargo run --bin bake_verilog`
    /// synthesizes each catalog entry and writes the result to its
    /// `baked_path`, and the checked-in result is embedded here.
    ///
    /// # Why a stale bake cannot go unnoticed
    ///
    /// A baked netlist is a claim about what this project's compiler
    /// produces, so a stale one does not merely go out of date -- it
    /// misrepresents the compiler, in a view whose entire purpose is showing
    /// what a circuit really is. Two tests stand between that and a reader:
    ///
    /// * `tests/verilog_frontend.rs`'s `the_baked_netlists_match_fresh_synthesis`
    ///   re-runs Yosys on the same embedded source and fails unless the
    ///   result renders byte-for-byte identically to the checked-in file.
    ///   That is the real check, and it runs on every `cargo test` on a
    ///   machine with the toolchain (the same machines the rest of
    ///   `tests/verilog_frontend.rs` already requires one on).
    /// * `baked_header_agrees_with_its_catalog_entry` below is the cheap
    ///   half, needing no toolchain at all: a baked file records which
    ///   circuit, source path and top module it was produced from, so
    ///   editing the catalog without re-baking fails immediately rather than
    ///   waiting for a machine that can run Yosys.
    ///
    /// # Panics
    ///
    /// If the embedded text does not parse. It is checked in, produced by
    /// [`baked::render`], and `every_baked_file_parses` covers every catalog
    /// entry -- so this is unreachable short of hand-editing a file the
    /// header tells you not to hand-edit.
    pub fn baked_netlist(&self) -> (Netlist, Vec<(String, String)>) {
        let baked = baked::parse(self.baked).unwrap_or_else(|error| {
            panic!("{}'s baked netlist ({}) does not parse: {error}", self.name, self.baked_path)
        });
        (baked.netlist, baked.output_labels)
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

/// The on-disk form of a synthesised netlist: a line-oriented text format,
/// the writer that produces it, and the reader that turns it back into a
/// [`Netlist`].
///
/// # Why a text format of our own, and not JSON
///
/// A baked file is a *reviewed artifact*. It is checked in, it is the only
/// thing standing between the browser viewer and a circuit it cannot
/// synthesize for itself (see [`VerilogCircuit::baked_netlist`]), and the
/// way anyone ever notices it changed is a diff. `gate merge g21 <- g14 g17`
/// says what it is on one line; the same gate as a JSON object spread over
/// five lines of `"name"`/`"inputs"`/`"output"`/`"kind"` says the same
/// thing with the shape of the change buried in punctuation. The format is
/// also the reason no `Serialize` derive had to be bolted onto
/// [`crate::compile::Netlist`] -- the compiler's own input type stays a
/// plain data structure with no serialization opinion attached.
///
/// # The format
///
/// One directive per line; `#` comments and blank lines are ignored. Every
/// name is whitespace-free (`NetlistBuilder` numbers gates `g0`, `g1`, ...
/// and every other name is a Verilog identifier), which is what lets the
/// reader just split on whitespace.
///
/// ```text
/// circuit verilog:and4          -- the catalog name this was baked from
/// source tests/fixtures/and4.v  -- the HDL it was synthesized out of
/// top and4                      -- the top module within that HDL
/// input a                       -- one per Netlist::inputs, in order
/// gate nor g0 <- a b            -- one per Netlist::gates, in order
/// gate merge g4 <- g2 g3        -- a wire merge: no torch, no gate body
/// gate nand g7 <- g4 g5         -- a gate-level cell, not yet realised
/// output g5                     -- one per Netlist::outputs, in order
/// label y g5                    -- port name -> internal signal
/// ```
///
/// The three header directives are not decoration: they are what lets a
/// machine with no Yosys still catch a catalog edited without re-baking
/// (see [`VerilogCircuit::baked_netlist`]'s "Why a stale bake cannot go
/// unnoticed").
pub mod baked {
    use std::fmt;

    use super::VerilogCircuit;
    use crate::compile::topology::GateKind;
    use crate::compile::{Gate, Netlist};

    /// One parsed baked file: the netlist and its output labels, plus the
    /// three header directives that say what it was baked from.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct BakedNetlist {
        /// The `circuit` directive -- a [`VerilogCircuit::name`].
        pub circuit: String,
        /// The `source` directive -- a [`VerilogCircuit::source_path`].
        pub source_path: String,
        /// The `top` directive -- a [`VerilogCircuit::top_module`].
        pub top_module: String,
        pub netlist: Netlist,
        /// `(port name, internal signal name)`, exactly as
        /// [`super::synthesize`] returns it.
        pub output_labels: Vec<(String, String)>,
    }

    /// Why a baked file could not be read, or a netlist could not be
    /// written as one.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FormatError {
        /// A line of a baked file could not be read. `line` is 1-based, so
        /// it matches what an editor shows.
        Parse { line: usize, message: String },
        /// A netlist could not be written in this format at all -- see
        /// [`render`] for the two things it requires of one.
        Unrenderable(String),
    }

    impl fmt::Display for FormatError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                FormatError::Parse { line, message } => write!(f, "line {line}: {message}"),
                FormatError::Unrenderable(message) => write!(f, "cannot be baked: {message}"),
            }
        }
    }

    impl std::error::Error for FormatError {}

    fn parse_error(line: usize, message: impl Into<String>) -> FormatError {
        FormatError::Parse { line, message: message.into() }
    }

    /// Write `netlist` (and its output labels) as the baked file for
    /// `circuit`, header and all -- the exact bytes `bake_verilog` writes to
    /// [`VerilogCircuit::baked_path`] and
    /// `the_baked_netlists_match_fresh_synthesis` compares against.
    ///
    /// Fails rather than producing an unreadable file if the netlist breaks
    /// either thing this format assumes: no name may contain whitespace (the
    /// reader splits on it), and a gate's `name` must equal its `output`
    /// (every gate this project builds comes from `NetlistBuilder`, which
    /// sets both from the same generated name -- so the format spends no
    /// line on a field that has never differed, and says so here instead of
    /// silently dropping it).
    pub fn render(
        circuit: &VerilogCircuit,
        netlist: &Netlist,
        output_labels: &[(String, String)],
    ) -> Result<String, FormatError> {
        let check = |name: &str, what: &str| -> Result<(), FormatError> {
            if name.is_empty() || name.split_whitespace().count() != 1 {
                Err(FormatError::Unrenderable(format!("{what} `{name}` is empty or contains whitespace")))
            } else {
                Ok(())
            }
        };

        let mut out = String::new();
        out.push_str("# reda baked netlist -- generated, do not edit by hand.\n");
        out.push_str("# Regenerate with: cargo run --release --bin bake_verilog\n");
        out.push_str("#\n");
        out.push_str("# The gate-level netlist Yosys produced for the source named below, read\n");
        out.push_str("# back by `reda::frontend`. It is checked in so a build that cannot run\n");
        out.push_str("# Yosys -- the browser viewer's wasm32 build above all -- can still load a\n");
        out.push_str("# synthesised circuit. A stale copy would misrepresent this project's own\n");
        out.push_str("# compiler, so `the_baked_netlists_match_fresh_synthesis` re-synthesizes\n");
        out.push_str("# and fails unless a fresh run renders byte-for-byte identically to this\n");
        out.push_str("# file.\n");
        out.push_str("#\n");
        out.push_str("# A gate's kind is `topology::GateKind::wire_name`. Two of them are what\n");
        out.push_str("# redstone builds directly: `gate nor y <- a b` is NOR(a, b) driving `y`,\n");
        out.push_str("# and `gate merge y <- a b` is a wire merge -- no torch and no gate body at\n");
        out.push_str("# all, just the point where `a`'s and `b`'s dust are allowed to touch.\n");
        out.push_str("# Every other kind (`and`, `nand`, `xor`, `mux`, ...) is gate level, and\n");
        out.push_str("# `compile::lowering` expands it into those two.\n");
        out.push_str("#\n");
        // Counts in the header are derived, never read back -- they exist so
        // that "this circuit grew two gates and lost a merge" is visible in
        // a diff of the header alone.
        let plural = |count: usize, singular: &str| {
            format!("{count} {singular}{}", if count == 1 { "" } else { "s" })
        };
        out.push_str(&format!(
            "# {}, {}, {}.\n",
            plural(netlist.gates.len(), "gate"),
            plural(netlist.inputs.len(), "input"),
            plural(netlist.outputs.len(), "output")
        ));
        out.push_str(&format!("# cells: {}\n", crate::compile::lowering::format_histogram(netlist)));

        check(circuit.name, "circuit name")?;
        check(circuit.source_path, "source path")?;
        check(circuit.top_module, "top module")?;
        out.push_str(&format!("circuit {}\n", circuit.name));
        out.push_str(&format!("source {}\n", circuit.source_path));
        out.push_str(&format!("top {}\n", circuit.top_module));

        for name in &netlist.inputs {
            check(name, "input")?;
            out.push_str(&format!("input {name}\n"));
        }
        for gate in &netlist.gates {
            if gate.name != gate.output {
                return Err(FormatError::Unrenderable(format!(
                    "gate `{}` has name `{}`, which this format does not carry separately",
                    gate.output, gate.name
                )));
            }
            check(&gate.output, "gate output")?;
            for input in &gate.inputs {
                check(input, "gate input")?;
            }
            out.push_str(&format!("gate {} {} <-", gate.kind.wire_name(), gate.output));
            for input in &gate.inputs {
                out.push(' ');
                out.push_str(input);
            }
            out.push('\n');
        }
        for name in &netlist.outputs {
            check(name, "output")?;
            out.push_str(&format!("output {name}\n"));
        }
        for (port, signal) in output_labels {
            check(port, "label port")?;
            check(signal, "label signal")?;
            out.push_str(&format!("label {port} {signal}\n"));
        }
        Ok(out)
    }

    /// Read back what [`render`] wrote. Strict about everything it can be
    /// strict about -- an unknown directive, a missing or repeated header,
    /// a gate line without its `<-` -- because a baked file is machine-
    /// written, so anything unexpected in one means it was hand-edited or
    /// produced by a different version of this format, and either way
    /// reading on would produce a netlist that is not the one Yosys built.
    pub fn parse(text: &str) -> Result<BakedNetlist, FormatError> {
        let mut circuit: Option<String> = None;
        let mut source_path: Option<String> = None;
        let mut top_module: Option<String> = None;
        let mut netlist = Netlist { inputs: Vec::new(), outputs: Vec::new(), gates: Vec::new() };
        let mut output_labels: Vec<(String, String)> = Vec::new();

        for (index, raw) in text.lines().enumerate() {
            let line_number = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut words = line.split_whitespace();
            let directive = words.next().expect("a non-empty trimmed line has a first word");

            let once = |slot: &mut Option<String>, value: Option<&str>, what: &str| -> Result<(), FormatError> {
                let value = value.ok_or_else(|| parse_error(line_number, format!("`{what}` needs a value")))?;
                if slot.is_some() {
                    return Err(parse_error(line_number, format!("`{what}` appears more than once")));
                }
                *slot = Some(value.to_string());
                Ok(())
            };

            match directive {
                "circuit" => once(&mut circuit, words.next(), "circuit")?,
                "source" => once(&mut source_path, words.next(), "source")?,
                "top" => once(&mut top_module, words.next(), "top")?,
                "input" => {
                    let name = words.next().ok_or_else(|| parse_error(line_number, "`input` needs a name"))?;
                    netlist.inputs.push(name.to_string());
                }
                "output" => {
                    let name = words.next().ok_or_else(|| parse_error(line_number, "`output` needs a name"))?;
                    netlist.outputs.push(name.to_string());
                }
                "label" => {
                    let port = words.next().ok_or_else(|| parse_error(line_number, "`label` needs a port name"))?;
                    let signal =
                        words.next().ok_or_else(|| parse_error(line_number, "`label` needs a signal name"))?;
                    output_labels.push((port.to_string(), signal.to_string()));
                }
                "gate" => {
                    let kind_name =
                        words.next().ok_or_else(|| parse_error(line_number, "`gate` needs a kind"))?.to_string();
                    let output = words.next().ok_or_else(|| parse_error(line_number, "`gate` needs an output"))?;
                    match words.next() {
                        Some("<-") => {}
                        other => {
                            return Err(parse_error(
                                line_number,
                                format!("expected `<-` after the gate output, found {other:?}"),
                            ))
                        }
                    }
                    let inputs: Vec<String> = words.map(str::to_string).collect();
                    // A kind's arity comes from the input list on the same
                    // line, which is what lets `nor`/`merge` stay
                    // arity-free in the text while a fixed-arity kind is
                    // still checked: `and g0 <- a b c` is rejected rather
                    // than silently reinterpreted.
                    let kind = GateKind::from_wire_name(&kind_name, inputs.len())
                        .ok_or_else(|| parse_error(line_number, format!("unknown gate kind `{kind_name}`")))?;
                    if kind.arity() != inputs.len() {
                        return Err(parse_error(
                            line_number,
                            format!(
                                "`{kind_name}` takes {} input(s), but this gate has {}",
                                kind.arity(),
                                inputs.len()
                            ),
                        ));
                    }
                    netlist.gates.push(Gate { name: output.to_string(), inputs, output: output.to_string(), kind });
                    continue; // `gate` consumed the rest of the line by design
                }
                other => {
                    return Err(parse_error(line_number, format!("unknown directive `{other}`")));
                }
            }

            if let Some(extra) = words.next() {
                return Err(parse_error(line_number, format!("unexpected extra word `{extra}`")));
            }
        }

        let missing = |what: &str| FormatError::Parse { line: 0, message: format!("no `{what}` directive") };
        Ok(BakedNetlist {
            circuit: circuit.ok_or_else(|| missing("circuit"))?,
            source_path: source_path.ok_or_else(|| missing("source"))?,
            top_module: top_module.ok_or_else(|| missing("top"))?,
            netlist,
            output_labels,
        })
    }
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
    ///
    /// The same check applies to the baked netlist for a second reason: a
    /// `baked_path` that names a different file from the `include_str!`
    /// beside it would leave `bake_verilog` faithfully rewriting a file
    /// nothing reads.
    #[test]
    fn source_is_the_fixture_the_acceptance_test_uses() {
        for circuit in CIRCUITS {
            for (embedded, path, what) in [
                (circuit.source, circuit.source_path, "source"),
                (circuit.baked, circuit.baked_path, "baked netlist"),
            ] {
                let on_disk = std::fs::read_to_string(path)
                    .unwrap_or_else(|err| panic!("{path} must be readable: {err}"));
                assert_eq!(
                    embedded, on_disk,
                    "{}'s embedded {what} has drifted from {path}",
                    circuit.name
                );
            }
        }
    }

    /// Every baked file parses, which is what makes
    /// [`VerilogCircuit::baked_netlist`]'s `expect` unreachable rather than
    /// merely unlikely.
    #[test]
    fn every_baked_file_parses() {
        for circuit in CIRCUITS {
            let (netlist, labels) = circuit.baked_netlist();
            assert!(!netlist.gates.is_empty(), "{}'s baked netlist has no gates", circuit.name);
            assert!(!netlist.outputs.is_empty(), "{}'s baked netlist declares no output", circuit.name);
            assert_eq!(
                labels.len(),
                netlist.outputs.len(),
                "{}: every declared output needs a port label",
                circuit.name
            );
            for (port, signal) in &labels {
                assert!(
                    netlist.outputs.contains(signal),
                    "{}: label `{port}` names `{signal}`, which is not a declared output",
                    circuit.name
                );
            }
            // The frontend guarantees this (see `NetlistBuilder`), and the
            // format relies on it -- a gate line carries one name, not two.
            for gate in &netlist.gates {
                assert_eq!(gate.name, gate.output, "{}: a gate's name and output must agree", circuit.name);
            }
        }
    }

    /// A baked file records which catalog entry, source and top module it
    /// was produced from. Editing the catalog without re-running
    /// `bake_verilog` therefore fails here -- on any machine, with no Yosys
    /// involved -- rather than waiting for
    /// `the_baked_netlists_match_fresh_synthesis`, which can only run where
    /// the toolchain exists.
    #[test]
    fn baked_header_agrees_with_its_catalog_entry() {
        for circuit in CIRCUITS {
            let baked = baked::parse(circuit.baked).expect("a checked-in baked file parses");
            assert_eq!(baked.circuit, circuit.name, "{}: baked under a different name", circuit.name);
            assert_eq!(
                baked.source_path, circuit.source_path,
                "{}: baked from a different source path",
                circuit.name
            );
            assert_eq!(
                baked.top_module, circuit.top_module,
                "{}: baked from a different top module",
                circuit.name
            );
        }
    }

    /// The one thing this catalog exists to provide that nothing else in the
    /// project does: a circuit that is *not* NOR by construction. Every
    /// hand-written circuit here is pure NOR, and until the frontend stopped
    /// technology-mapping, so was every synthesised one -- `abc -genlib
    /// redstone_nor.genlib` collapsed the gate level inside Yosys, and the
    /// most this test could ask for was that some of those NORs were merges
    /// instead (it pinned 31 gates, 11 of them merges).
    ///
    /// It can ask for the real thing now: the decoder arrives as the gate
    /// level Yosys actually produced, and this pins that histogram exactly.
    /// If it ever collapses back to two kinds, something has started mapping
    /// again upstream and the number to re-check is not the gate count.
    #[test]
    fn the_baked_seven_segment_is_a_gate_level_circuit_not_a_wall_of_nors() {
        use crate::compile::topology::GateKind;

        let (netlist, _) = find("verilog:seven_segment").expect("catalog entry must exist").baked_netlist();
        assert_eq!(netlist.gates.len(), 31, "gate count has moved -- re-check the whole size ladder");
        assert_eq!(
            crate::compile::lowering::format_histogram(&netlist),
            "nor2:3 merge2:6 and:5 nand:9 andnot:6 ornot:1 mux:1",
            "the decoder's cell-type histogram has moved"
        );

        let gate_level = netlist.gates.iter().filter(|gate| !gate.kind.is_realisable()).count();
        assert_eq!(gate_level, 22, "22 of the 31 cells have no redstone realisation of their own");
        assert!(
            netlist.gates.iter().any(|gate| gate.kind == GateKind::Mux),
            "the decoder's one $_MUX_ is the cell the cost-table spec left as an open question -- \
             losing it silently would lose the only test coverage its expansion has"
        );

        // And it still lowers to something redstone builds, entirely.
        let lowered = crate::compile::lowering::lower(&netlist).expect("the decoder lowers");
        assert!(lowered.gates.iter().all(|gate| gate.kind.is_realisable()));
        assert_eq!(
            crate::compile::lowering::format_histogram(&lowered),
            "nor1:23 nor2:16 merge2:17",
            "the lowered decoder's shape has moved -- re-check the whole size ladder"
        );
    }

    /// Round-trip: whatever `render` writes, `parse` reads back as the same
    /// netlist. This is what lets `the_baked_netlists_match_fresh_synthesis`
    /// compare *rendered text* and still be a statement about netlists.
    #[test]
    fn render_and_parse_round_trip_every_catalog_entry() {
        for circuit in CIRCUITS {
            let (netlist, labels) = circuit.baked_netlist();
            let rendered = baked::render(circuit, &netlist, &labels).expect("a baked netlist re-renders");
            assert_eq!(rendered, circuit.baked, "{}: re-rendering is not the identity", circuit.name);
            let reparsed = baked::parse(&rendered).expect("what render wrote, parse reads");
            assert_eq!(reparsed.netlist, netlist, "{}: netlist did not survive the round trip", circuit.name);
            assert_eq!(reparsed.output_labels, labels, "{}: labels did not survive the round trip", circuit.name);
        }
    }

    /// A baked file is machine-written, so the reader treats anything it did
    /// not write as a reason to stop rather than something to skip past --
    /// see [`baked::parse`]'s own doc comment.
    #[test]
    fn the_reader_rejects_what_the_writer_would_never_produce() {
        let good = "circuit c\nsource s\ntop t\ninput a\ngate nor g0 <- a\noutput g0\nlabel y g0\n";
        assert!(baked::parse(good).is_ok(), "the minimal well-formed file must parse");

        for (bad, why) in [
            ("circuit c\nsource s\ntop t\nwidget a\n", "unknown directive"),
            ("circuit c\nsource s\ntop t\ngate xor g0 <- a\n", "unknown gate kind"),
            ("circuit c\nsource s\ntop t\ngate nor g0 a\n", "missing `<-`"),
            ("circuit c\nsource s\ntop t\ninput a b\n", "extra word"),
            ("circuit c\nsource s\ntop t\ncircuit d\n", "repeated header"),
            ("source s\ntop t\n", "missing `circuit`"),
            ("circuit c\ntop t\n", "missing `source`"),
            ("circuit c\nsource s\n", "missing `top`"),
        ] {
            assert!(baked::parse(bad).is_err(), "{why} must be rejected");
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
