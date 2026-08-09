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

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::compile::Netlist;

mod yosys_json;

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
        stderr = format!("synth.py exited with status {:?} and produced no diagnostic output", output.status.code());
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
    /// Yosys cell type the frontend accepts has to be something the rest of
    /// the pipeline can actually build.
    #[test]
    fn every_accepted_yosys_cell_type_lowers_to_something_the_library_can_place() {
        let library = Library::default_library();
        for (cell_type, kind) in topology::known_yosys_cell_types() {
            let expansion = topology::expansion_for(kind);
            assert!(!expansion.steps.is_empty(), "{cell_type} ({kind:?}) has no expansion");

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

    /// A cell type the frontend never accepts has no `GateKind` at all, so
    /// the "unsupported construct" error path is reached by data rather than
    /// by falling through every known name.
    #[test]
    fn an_unmapped_cell_type_has_no_gate_kind() {
        assert!(topology::gate_kind_for_yosys_cell("$__ZERO").is_none());
        assert!(topology::gate_kind_for_yosys_cell("$__ONE").is_none());
        assert!(topology::gate_kind_for_yosys_cell("$_DFF_P_").is_none());
        assert!(topology::gate_kind_for_yosys_cell("").is_none());
    }
}
