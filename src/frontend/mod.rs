//! Verilog frontend: turn HDL into the [`Netlist`] the rest of this crate
//! already knows how to place, route, and simulate.
//!
//! This is not a Verilog parser -- it shells out to
//! [Yosys](https://github.com/YosysHQ/yosys) (via the `yowasp-yosys` Python
//! package, a WASM build of Yosys with ABC built in) to do the actual
//! parsing and logic synthesis, and technology-maps the result onto exactly
//! the cells this project's hardware can build: `redstone_nor.genlib`
//! describes a NOR-only library with 1, 2, and 3 input variants, priced in
//! this project's own units (see that file for exactly where each number
//! comes from). [`yosys_json`] then reads Yosys's mapped-netlist JSON back
//! into a [`Netlist`].
//!
//! # Why a cost model at all
//!
//! A generic synthesiser tuned for CMOS decomposes into small (usually
//! 2-input) gates, because in CMOS more fan-in means a slower, larger gate.
//! In this hardware a NOR gate is one redstone torch: a 1-, 2-, or 3-input
//! NOR has the *same* delay, and only a little more area. Left to its
//! default assumptions, ABC would optimise in exactly the wrong direction --
//! decomposing multiplies gate count and wire, and wire is what actually
//! costs delay here (see `src/compile/mod.rs`'s module doc comment). Feeding
//! it this project's real cell costs via `-genlib` is what corrects that.
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

/// The frontend's cost model, embedded at build time. See the file itself
/// for the derivation of every number in it.
const GENLIB: &str = include_str!("redstone_nor.genlib");

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
    /// know how to turn into a NOR-only [`Netlist`] -- an unrecognized cell
    /// type, a constant this library cannot drive, a bit width this reader
    /// does not handle, and so on. Deliberately a hard error rather than a
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

/// Synthesize `verilog_source`'s `top_module` into a [`Netlist`], mapped
/// entirely onto this project's real NOR cell library.
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
    let genlib_path = work_dir.join("redstone_nor.genlib");
    let synth_py_path = work_dir.join("synth.py");
    let output_json_path = work_dir.join("out.json");

    std::fs::write(&verilog_path, verilog_source)?;
    std::fs::write(&genlib_path, GENLIB)?;
    std::fs::write(&synth_py_path, SYNTH_PY)?;

    let result = run_synth(&synth_py_path, &verilog_path, top_module, &genlib_path, &output_json_path);

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
    genlib_path: &Path,
    output_json_path: &Path,
) -> Result<(), FrontendError> {
    let python = std::env::var("REDA_PYTHON").unwrap_or_else(|_| "python".to_string());

    let output = Command::new(&python)
        .arg(synth_py)
        .arg(verilog_path)
        .arg(top_module)
        .arg(genlib_path)
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
