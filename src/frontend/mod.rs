//! Verilog frontend: turn HDL into the [`Netlist`] the rest of this crate
//! already knows how to place, route, and simulate.
//!
//! This is not a Verilog parser -- it shells out to
//! [Yosys](https://github.com/YosysHQ/yosys) (via the `yowasp-yosys` Python
//! package, a WASM build of Yosys with ABC built in) to do the actual
//! parsing and logic optimisation, and stops at the **gate level**:
//! [`yosys_json`] reads Yosys's own `$_AND_`/`$_NAND_`/`$_XOR_`/`$_MUX_`
//! cells back as [`Netlist`] gates, one for one. How each of those becomes
//! redstone is `compile::topology`'s decision, applied by
//! `compile::lowering`.
//!
//! # Why this frontend stopped technology-mapping
//!
//! It used to run `abc -genlib redstone_nor.genlib`, which made ABC map the
//! design onto NOR gates and wire merges before this crate ever saw it. The
//! genlib existed to price those cells so ABC's mapper would choose the way
//! a human designing for redstone would: a NOR gate is one torch, so a 1-,
//! 2- or 3-input NOR has the *same* delay and only a little more area, and
//! an OR is free outright -- exactly backwards from CMOS, where more fan-in
//! means a slower, larger gate.
//!
//! Every word of that reasoning is still true, and it was still the wrong
//! place to act on it. Technology mapping collapses the gate level, and the
//! gate level is the input this project's own topology library needs: that
//! library's entire job is deciding how a gate becomes redstone, and it was
//! being handed a design where the decision had already been made. Teaching
//! ABC our costs through a price list was working around that rather than
//! fixing it.
//!
//! ABC still runs, and still does the half of its job that is genuinely
//! valuable here -- logic optimisation, which takes the hand-written
//! seven-segment decoder's 84 gates to 31. `redstone_nor.genlib` is gone: a
//! genlib describes a mapping target, nothing maps any more, and a cost
//! model nothing reads is a lie. Its derivation survives where it is
//! actually used, as `compile::topology::expansion_cost`.
//!
//! # The external dependency
//!
//! Yosys is not a Rust crate; it is an external tool this frontend shells
//! out to via `python` + `yowasp-yosys`. Neither the existing test suite nor
//! any other part of this crate depends on it -- only code that explicitly
//! calls [`synthesize_verilog`] does, and it fails with a specific, readable
//! [`FrontendError`] (not a panic or a bare `ModuleNotFoundError`
//! traceback) if `python` is missing, or if `python` is present but
//! `yowasp-yosys` is not installed.
//!
//! # The REDA SystemVerilog compiler
//!
//! Beside the Yosys path above lives [`compile_systemverilog`], a pure Rust
//! compiler for a synthesizable SystemVerilog subset. It shells out to
//! nothing, reads no files, and inspects no environment, so it runs
//! unchanged on native targets and on `wasm32-unknown-unknown`. Its output
//! stops at exactly the same boundary the Yosys bridge does -- a gate-level
//! [`Netlist`] -- plus a [`DebugDatabase`] sidecar recording where every
//! gate came from.
//!
//! The two frontends coexist on purpose. Yosys remains the production path
//! while the generator is changing; this one is proven against it, fixture by
//! fixture, and takes over only at the cutover the plan describes
//! (`docs/native-wasm-verilog-compiler-plan.md`). Nothing in this module
//! changes [`synthesize_verilog`] or any of its callers.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::compile::Netlist;

pub mod debug;
pub mod evaluate;
pub mod source;

mod ast;
mod elaborate;
mod lexer;
mod logic;
mod netlist;
mod parser;
mod rtl;
mod yosys_json;

pub use ast::{PortDirection, SourceKind, SourceNode, SourceNodeId};
pub use debug::DebugDatabase;
pub use elaborate::ElabNodeId;
pub use source::{line_column, ExpansionId, FileId, SourceFileInfo, SourceInput, Span};

/// The Python driver that actually invokes Yosys. Kept as a standalone
/// script (rather than a Rust-constructed `python -c "..."` one-liner) so it
/// can be read, run, and debugged on its own -- see the script's own doc
/// comment for the synthesis pipeline it runs.
const SYNTH_PY: &str = include_str!("synth.py");

/// Everything that can go wrong turning Verilog into a [`Netlist`].
#[derive(Debug)]
pub enum FrontendError {
    /// `python` could not be launched at all -- most likely it is not
    /// installed, or not on `PATH`.
    PythonNotFound(std::io::Error),
    /// The synthesis driver ran but did not produce an output JSON. This is
    /// the umbrella for every Yosys-level failure -- a missing top module,
    /// invalid Verilog, ABC unable to map the design -- because Yosys
    /// itself does not distinguish them at the process level (see
    /// `synth.py`'s doc comment: `run_yosys` never raises on a Yosys-level
    /// error, it just fails to produce output). `stderr` carries whatever
    /// diagnostic `synth.py` could recover, most commonly the `ERROR:`
    /// lines out of Yosys's own log.
    SynthesisFailed { stderr: String },
    /// Reading or writing one of the frontend's own temporary files failed.
    Io(std::io::Error),
    /// Yosys's JSON output was not parseable as JSON at all.
    Json(serde_json::Error),
    /// The JSON parsed fine, but described something this frontend does not
    /// know how to turn into a [`Netlist`] -- an unrecognized cell type, a
    /// constant nothing here can drive, a bit width this reader does not
    /// handle, and so on. Deliberately a hard error rather than a
    /// silent skip: a dropped cell is a netlist that still compiles, just
    /// to the wrong circuit.
    Unsupported(String),
}

impl fmt::Display for FrontendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrontendError::PythonNotFound(err) => write!(
                f,
                "could not launch `python` ({err}) -- the Verilog frontend needs Python 3 with \
                 `yowasp-yosys` installed (`pip install yowasp-yosys`, or see requirements.txt)"
            ),
            FrontendError::SynthesisFailed { stderr } => {
                write!(f, "yosys synthesis failed:\n{stderr}")
            }
            FrontendError::Io(err) => write!(f, "I/O error in the Verilog frontend: {err}"),
            FrontendError::Json(err) => write!(f, "could not parse yosys's JSON output: {err}"),
            FrontendError::Unsupported(message) => write!(f, "unsupported construct: {message}"),
        }
    }
}

impl std::error::Error for FrontendError {}

impl From<std::io::Error> for FrontendError {
    fn from(err: std::io::Error) -> Self {
        FrontendError::Io(err)
    }
}

impl From<serde_json::Error> for FrontendError {
    fn from(err: serde_json::Error) -> Self {
        FrontendError::Json(err)
    }
}

// ---------------------------------------------------------------------
// The pure SystemVerilog entry point
// ---------------------------------------------------------------------

/// How severe a [`Diagnostic`] is. Version 1 only ever produces errors;
/// the enum exists so that adding a warning later is not an API break, and
/// so nothing here has to invent a warning it does not mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    Error,
}

/// One compiler message, located by byte offsets into one source file.
///
/// Byte offsets are the canonical representation: a native host and a
/// browser host can each derive line and column with [`line_column`] when
/// they need to show a position, rather than every token carrying two
/// coordinate systems it mostly does not use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    /// `name:line:column: message`, given the same sources the compile was
    /// handed. Hosts that show diagnostics differently can ignore this and
    /// read the span directly.
    pub fn render(&self, sources: &[SourceInput<'_>]) -> String {
        let file = sources.get(self.span.file.0 as usize);
        let (name, text) = match file {
            Some(source) => (source.name, source.text),
            None => ("<unknown>", ""),
        };
        let (line, column) = line_column(text, self.span.start);
        format!("{name}:{line}:{column}: {}", self.message)
    }
}

/// What to compile. One selected top module, which is all version 1
/// elaborates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileOptions {
    pub top: String,
}

impl CompileOptions {
    pub fn new(top: impl Into<String>) -> CompileOptions {
        CompileOptions { top: top.into() }
    }
}

/// One port bit of the top module, and the [`Netlist`] signal it is.
///
/// A scalar port is its own name; bit `i` of a vector port is `name[i]`,
/// LSB-first -- the same spelling the Yosys bridge produces, so a caller
/// cannot tell the two frontends apart by how they label a port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    /// The port bit's name, e.g. `"y"` or `"q[3]"`.
    pub name: String,
    pub direction: PortDirection,
    /// The signal this port bit is in [`Netlist`].
    pub signal: String,
    /// The elaborated signal this bit belongs to.
    pub elab: ElabNodeId,
    /// Which bit of that signal, LSB-first.
    pub bit: u32,
}

/// Everything one successful compile produced.
#[derive(Debug, Clone)]
pub struct CompileArtifact {
    pub netlist: Netlist,
    /// Every port bit, sorted by name for deterministic transport.
    pub ports: Vec<PortBinding>,
    pub debug: DebugDatabase,
}

impl CompileArtifact {
    /// The signal a named port bit drives, e.g. `ports_for("y")`.
    pub fn port(&self, name: &str) -> Option<&PortBinding> {
        self.ports.iter().find(|port| port.name == name)
    }

    /// `port name -> netlist signal` for the output ports only -- the same
    /// shape [`synthesize_verilog`] returns, so a caller can switch
    /// frontends without changing how it finds its own outputs.
    pub fn output_map(&self) -> HashMap<String, String> {
        self.ports
            .iter()
            .filter(|port| port.direction == PortDirection::Output)
            .map(|port| (port.name.clone(), port.signal.clone()))
            .collect()
    }
}

/// Compile `sources` into a gate-level [`Netlist`] plus its provenance.
///
/// This is the cross-platform entry point: pure, synchronous, and free of
/// target-specific types, so a browser worker and a native file watcher can
/// each wrap it without the compiler knowing which one it is running in.
///
/// Version 1 normally receives one source. It takes a slice anyway, because
/// includes and multi-file projects would otherwise change the meaning of
/// every [`FileId`] in every span and debug record -- the one thing the
/// provenance contract must not have to do twice.
///
/// # Errors
///
/// Returns every [`Diagnostic`] the compile produced, sorted by position.
/// Lexing and parsing stop at the first error (version 1 has no syntax
/// recovery); elaboration accumulates independent item-level errors.
pub fn compile_systemverilog(
    sources: &[SourceInput<'_>],
    options: &CompileOptions,
) -> Result<CompileArtifact, Vec<Diagnostic>> {
    let files: Vec<(FileId, &str)> = sources
        .iter()
        .enumerate()
        .map(|(index, source)| (FileId(index as u32), source.text))
        .collect();

    let set = parser::parse_sources(&files).map_err(|diagnostic| vec![diagnostic])?;
    let design = elaborate::elaborate(&set, &options.top)?;
    let blasted = logic::build(&design)?;
    let emitted = netlist::emit(&design, &blasted)?;

    // The same structural checks any consumer of a `Netlist` depends on.
    // Reaching one of these is a compiler bug, not a source error, so it is
    // reported as one instead of being left for the placer to trip over.
    if let Err(error) = evaluate::validate(&emitted.netlist) {
        return Err(vec![Diagnostic {
            severity: Severity::Error,
            message: format!("internal compiler error: emitted netlist is invalid: {error}"),
            span: Span::new(FileId(0), 0, 0),
        }]);
    }

    let mut transformations = blasted.graph.transformations;
    transformations.extend(emitted.transformations);

    let debug = DebugDatabase {
        files: sources
            .iter()
            .map(|source| SourceFileInfo {
                name: source.name.to_string(),
                len: source.text.len() as u32,
            })
            .collect(),
        source_nodes: set.nodes,
        elab_nodes: design.nodes,
        gates: emitted.gates,
        logic_origins: blasted.graph.origins,
        extra_origins: blasted.graph.extra_origins,
        realisations: emitted.realisations,
        transformations,
        signals: emitted.signals,
        netlist_fingerprint: debug::netlist_fingerprint(&emitted.netlist),
    };

    Ok(CompileArtifact {
        netlist: emitted.netlist,
        ports: emitted.ports,
        debug,
    })
}

/// Synthesize `verilog_source`'s `top_module` into a **gate-level**
/// [`Netlist`] -- one gate per Yosys cell, in Yosys's own vocabulary. Run it
/// through `compile::lowering::lower` (as `compile::compile` does) to get
/// the NOR gates and wire merges redstone actually builds.
///
/// Returns the netlist together with a lookup from each of `top_module`'s
/// declared output port names (e.g. `"y"`, or `"q[3]"` for bit 3 of a
/// multi-bit port `q`) to that output's actual signal name in
/// `netlist.outputs` -- the same shape [`crate::circuits::seven_segment::build_seven_segment_netlist`]
/// already returns, and for the same reason: gate-tree construction invents
/// internal names, so callers need this to find their own ports again.
///
/// # Errors
///
/// See [`FrontendError`]. In particular: this needs `python` on `PATH` with
/// `yowasp-yosys` installed, and returns a specific, readable error instead
/// of panicking if either is missing.
pub fn synthesize_verilog(
    verilog_source: &str,
    top_module: &str,
) -> Result<(Netlist, HashMap<String, String>), FrontendError> {
    let work_dir = make_work_dir()?;

    let verilog_path = work_dir.join("top.v");
    let synth_py_path = work_dir.join("synth.py");
    let output_json_path = work_dir.join("out.json");

    std::fs::write(&verilog_path, verilog_source)?;
    std::fs::write(&synth_py_path, SYNTH_PY)?;

    let result = run_synth(&synth_py_path, &verilog_path, top_module, &output_json_path);

    let netlist_result = match result {
        Ok(()) => {
            let json_text = std::fs::read_to_string(&output_json_path)?;
            let json: serde_json::Value = serde_json::from_str(&json_text)?;
            yosys_json::netlist_from_json(&json, top_module)
        }
        Err(err) => Err(err),
    };

    // Keep the work directory around on failure -- it is the only record of
    // what was actually fed to Yosys, and someone debugging a synthesis
    // failure needs it. Clean up on success so temp directories do not pile
    // up across repeated runs.
    if netlist_result.is_ok() {
        let _ = std::fs::remove_dir_all(&work_dir);
    }

    netlist_result
}

/// A fresh, empty scratch directory under the OS temp directory. Not using
/// the `tempfile` crate here keeps this frontend's own dependency footprint
/// as small as the rest of the crate's -- one extra directory per call, best
/// effort, named from the process id and a monotonic counter so concurrent
/// calls (e.g. two tests running in parallel) never collide.
fn make_work_dir() -> Result<std::path::PathBuf, FrontendError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("reda-verilog-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Run `synth.py`, translating its process-level failure modes into
/// [`FrontendError`]. Does not touch the filesystem beyond spawning the
/// child process and reading back its captured output.
fn run_synth(
    synth_py: &Path,
    verilog_path: &Path,
    top_module: &str,
    output_json_path: &Path,
) -> Result<(), FrontendError> {
    let python = std::env::var("REDA_PYTHON").unwrap_or_else(|_| "python".to_string());

    let output = Command::new(&python)
        .arg(synth_py)
        .arg(verilog_path)
        .arg(top_module)
        .arg(output_json_path)
        .output()
        .map_err(FrontendError::PythonNotFound)?;

    if output.status.success() && output_json_path.exists() {
        return Ok(());
    }

    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if stderr.trim().is_empty() {
        stderr = String::from_utf8_lossy(&output.stdout).into_owned();
    }
    if stderr.trim().is_empty() {
        stderr = format!(
            "synth.py exited with status {:?} and produced no diagnostic output",
            output.status.code()
        );
    }
    Err(FrontendError::SynthesisFailed { stderr })
}

#[cfg(test)]
mod tests {
    use crate::compile::topology::{self, GateKind, Library};

    /// The genlib is gone, so the test that used to live here -- checking
    /// `redstone_nor.genlib`'s hand-written `GATE`/`PIN` numbers against
    /// `topology::genlib_cost`'s derivation of the same fact -- has nothing
    /// left to compare. Its successor lives in `topology` itself
    /// (`entry_cost_and_expansion_cost_agree_for_every_realisable_kind`),
    /// between the two cost models that remain.
    ///
    /// What is still this module's business is the boundary it owns: every
    /// combinational Yosys cell type the frontend accepts has to be something
    /// the rest of the pipeline can actually build. Stateful cells are named
    /// boundaries: they are retained for the sequential compiler phase, not
    /// expanded through the combinational primitive library.
    #[test]
    fn every_accepted_yosys_cell_type_lowers_to_something_the_library_can_place() {
        let library = Library::default_library();
        for (cell_type, kind) in topology::known_yosys_cell_types() {
            if kind.is_sequential() {
                assert_eq!(
                    kind,
                    GateKind::DffPosedge,
                    "only the named DFF is stateful today"
                );
                continue;
            }
            let expansion = topology::expansion_for(kind);
            assert!(
                !expansion.steps.is_empty(),
                "{cell_type} ({kind:?}) has no expansion"
            );

            // Every step of every expansion is a NOR or a merge of an arity
            // `Library` ships an entry for -- so nothing the frontend
            // accepts can reach `primitive_graph::expand` with no way to be
            // turned into primitives.
            for step in &expansion.steps {
                let realised = match step {
                    topology::Step::Nor(operands) => GateKind::Nor(operands.len()),
                    topology::Step::Merge(operands) => GateKind::Or(operands.len()),
                };
                assert!(
                    library.choose(realised).is_some(),
                    "{cell_type} ({kind:?}) expands through {realised:?}, which has no library entry"
                );
            }
        }
    }

    /// A cell type the frontend never accepts has no `GateKind` at all. The
    /// positive-edge DFF is deliberately the exception: it has a named
    /// sequential boundary, while other state, tri-state and constant cells
    /// still take the "unsupported construct" path.
    #[test]
    fn an_unmapped_cell_type_has_no_gate_kind() {
        assert!(topology::gate_kind_for_yosys_cell("$__ZERO").is_none());
        assert!(topology::gate_kind_for_yosys_cell("$__ONE").is_none());
        assert_eq!(
            topology::gate_kind_for_yosys_cell("$_DFF_P_"),
            Some(GateKind::DffPosedge)
        );
        assert!(topology::gate_kind_for_yosys_cell("$_DLATCH_P_").is_none());
        assert!(topology::gate_kind_for_yosys_cell("$_TBUF_").is_none());
        assert!(topology::gate_kind_for_yosys_cell("").is_none());
    }
}
