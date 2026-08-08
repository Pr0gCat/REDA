//! Verilog frontend: turn HDL into the [`Netlist`] the rest of this crate
//! already knows how to place, route, and simulate.
//!
//! This is not a Verilog parser -- it shells out to
//! [Yosys](https://github.com/YosysHQ/yosys) (via the `yowasp-yosys` Python
//! package, a WASM build of Yosys with ABC built in) to do the actual
//! parsing and logic synthesis, and technology-maps the result onto exactly
//! the cells this project's hardware can build: `redstone_nor.genlib`
//! describes NOR (1, 2, and 3 input variants) and OR (2 and 3 input,
//! realised as a free wire merge rather than a gate -- see that file's own
//! "OR2 / OR3: a wire merge, not a gate" section), priced in this project's
//! own units (see that file for exactly where each number comes from).
//! [`yosys_json`] then reads Yosys's mapped-netlist JSON back into a
//! [`Netlist`].
//!
//! # Why a cost model at all
//!
//! A generic synthesiser tuned for CMOS decomposes into small (usually
//! 2-input) gates, because in CMOS more fan-in means a slower, larger gate.
//! In this hardware a NOR gate is one redstone torch: a 1-, 2-, or 3-input
//! NOR has the *same* delay, and only a little more area, and an OR is
//! often free outright. Left to its default assumptions, ABC would optimise
//! in exactly the wrong direction -- decomposing multiplies gate count and
//! wire, and wire is what actually costs delay here (see
//! `src/compile/mod.rs`'s module doc comment). Feeding it this project's
//! real cell costs via `-genlib` is what corrects that.
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

#[cfg(test)]
mod tests {
    use crate::compile::topology::{self, GateKind, Library};

    /// `GENLIB`'s own `GATE`/`PIN` numbers for `cell_name`: `(area,
    /// block_delay)`, read straight out of the SIS Genlib text with no
    /// dependency on any crate parser -- deliberately dumb line-splitting,
    /// so this test does not share a bug with whatever eventually reads this
    /// format for real.
    ///
    /// Layout assumed (see `redstone_nor.genlib`'s own cells): a `GATE
    /// <name> <area> Y=<expr>;` line, followed by exactly one `PIN * <phase>
    /// <inputload> <maxload> <rise_block> <rise_fanout> <fall_block>
    /// <fall_fanout>` line. Every cell this reader supports models delay as
    /// a pure per-gate constant (`redstone_nor.genlib`'s own comment) --
    /// `rise_block == fall_block` for all of them -- so reading `rise_block`
    /// alone is reading "the" delay.
    fn genlib_area_and_delay(cell_name: &str) -> (u32, u64) {
        let gate_prefix = format!("GATE {cell_name} ");
        let lines: Vec<&str> = super::GENLIB.lines().collect();
        let gate_line_index = lines
            .iter()
            .position(|line| line.starts_with(&gate_prefix))
            .unwrap_or_else(|| panic!("no `GATE {cell_name}` line in redstone_nor.genlib"));

        let area: u32 = lines[gate_line_index]
            .split_whitespace()
            .nth(2)
            .unwrap_or_else(|| panic!("`GATE {cell_name}` line has no area field"))
            .parse()
            .unwrap_or_else(|e| panic!("`GATE {cell_name}` area is not a number: {e}"));

        let pin_line = lines[gate_line_index + 1..]
            .iter()
            .find(|line| line.trim_start().starts_with("PIN"))
            .unwrap_or_else(|| panic!("no `PIN` line after `GATE {cell_name}`"));
        // `PIN * <phase> <inputload> <maxload> <rise_block> <rise_fanout>
        // <fall_block> <fall_fanout>`: "PIN", "*", phase and the two load
        // fields are tokens 0..=4, so `rise_block` is token 5.
        let rise_block: u64 = pin_line
            .split_whitespace()
            .nth(5)
            .unwrap_or_else(|| panic!("`PIN` line for `{cell_name}` has no rise-block-delay field"))
            .parse()
            .unwrap_or_else(|e| panic!("`{cell_name}`'s rise-block-delay is not a number: {e}"));

        (area, rise_block)
    }

    /// The whole point of `topology::genlib_cost`: `redstone_nor.genlib`'s
    /// hand-written `GATE`/`PIN` numbers for every cell it maps a Yosys cell
    /// type onto are exactly what deriving cost from that cell's own
    /// `topology::Template` produces. This is what keeps the genlib (ABC's
    /// cost model) and the topology library (this crate's own realisation
    /// model) from drifting apart the way the task that added this test was
    /// written to close -- if `place_nor_gate`'s footprint or a template's
    /// own shape changes without the other, this fails immediately instead
    /// of silently mapping to the wrong cost.
    ///
    /// `OR2`/`OR3` check **every** registered entry for `Or(2)`/`Or(3)`, not
    /// just the one `Library::choose` would pick -- both `or_bare_entry` and
    /// `or_isolated_entry` have to agree with the genlib's one `GATE OR2`/
    /// `GATE OR3` line, since (per `genlib_cost`'s own "OR: one number,
    /// honestly standing for two realisations") that one line is meant to
    /// honestly price both. Checking only the first entry, the way NOR/BUF's
    /// loop originally did (harmlessly, since they only ever have one entry
    /// each), would not have caught it if a future change made the two OR
    /// entries disagree with each other.
    #[test]
    fn genlib_numbers_match_what_the_topology_library_derives() {
        let library = Library::default_library();
        for &(cell_name, kind) in &[
            ("NOR1", GateKind::Nor(1)),
            ("NOR2", GateKind::Nor(2)),
            ("NOR3", GateKind::Nor(3)),
            ("BUF", GateKind::Buf),
            ("OR2", GateKind::Or(2)),
            ("OR3", GateKind::Or(3)),
        ] {
            let entries = library.entries_for(kind);
            assert!(!entries.is_empty(), "{cell_name} ({kind:?}) has no library entry");
            let (genlib_area, genlib_delay) = genlib_area_and_delay(cell_name);
            for entry in entries {
                let cost = topology::genlib_cost(entry);
                assert_eq!(
                    cost.area, genlib_area,
                    "{cell_name} ({}): derived area disagrees with redstone_nor.genlib",
                    entry.name
                );
                assert_eq!(
                    cost.delay_game_ticks, genlib_delay,
                    "{cell_name} ({}): derived delay disagrees with redstone_nor.genlib",
                    entry.name
                );
            }
        }
    }

    /// Every Yosys cell type `redstone_nor.genlib` can produce maps to a
    /// `GateKind` this library actually ships an entry for -- otherwise
    /// `yosys_json::Context::build_cell` would map a cell to a `GateKind`
    /// and then find nothing to build it from, a bug this test exists to
    /// catch instead of letting it surface as a runtime panic mid-synthesis.
    #[test]
    fn every_mapped_yosys_cell_has_a_library_entry() {
        let library = Library::default_library();
        for &cell_name in &["NOR1", "NOR2", "NOR3", "BUF", "OR2", "OR3"] {
            let kind = topology::gate_kind_for_yosys_cell(cell_name)
                .unwrap_or_else(|| panic!("{cell_name} has no GateKind mapping"));
            assert!(library.choose(kind).is_some(), "{cell_name} maps to {kind:?}, which has no library entry");
        }
    }

    /// A cell type `redstone_nor.genlib` never produces -- including the
    /// constant drivers, which this library deliberately has no realisation
    /// for -- has no `GateKind` mapping at all, so the frontend's "unmapped
    /// cell" error path is reached by data, not by falling through every
    /// known name.
    #[test]
    fn an_unmapped_cell_type_has_no_gate_kind() {
        assert!(topology::gate_kind_for_yosys_cell("$__ZERO").is_none());
        assert!(topology::gate_kind_for_yosys_cell("$__ONE").is_none());
        assert!(topology::gate_kind_for_yosys_cell("DFF").is_none());
        assert!(topology::gate_kind_for_yosys_cell("").is_none());
    }
}
