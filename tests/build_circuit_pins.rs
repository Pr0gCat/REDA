//! Black-box tests for `build_circuit --pins`: the IO-terminals contract
//! (docs/superpowers/specs/2026-08-30-io-terminals.md) exercised through the
//! public command line, exactly as a caller with a pins file would drive it.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// One test's own scratch directory, created empty-or-reused. The binary is
/// run *in* it because the `output/` tree it writes is relative to the
/// working directory.
fn scratch_dir(test: &str) -> PathBuf {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    std::fs::create_dir_all(&scratch).expect("the scratch directory is creatable");
    scratch
}

/// Run the binary with `args` in `test`'s scratch directory, returning the
/// process output plus that directory for reading the sidecars back.
fn run_in_scratch(test: &str, args: &[&str]) -> (Output, PathBuf) {
    let scratch = scratch_dir(test);
    let output = Command::new(env!("CARGO_BIN_EXE_build_circuit"))
        .args(args)
        .current_dir(&scratch)
        .output()
        .expect("build_circuit must run");
    (output, scratch)
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Every way a pins file can be wrong exits before any compile, with a
/// message that names the defect -- the file is hand-written, and a --grown
/// run costs minutes on the circuits pins exist for.
#[test]
fn pins_file_defects_exit_by_name_before_any_compile() {
    // A file that is not there is named by its path.
    let (output, _) = run_in_scratch(
        "pins-missing-file",
        &["and4", "--grown", "--pins", "no-such-pins.json"],
    );
    assert!(!output.status.success(), "a missing pins file must not compile anything");
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("could not read the pins file") && stderr.contains("no-such-pins.json"),
        "the refusal names the file: {stderr}"
    );

    // A malformed pin is named through the CLI surface, port and defect both.
    let scratch = scratch_dir("pins-bad-facing");
    let bad_facing = scratch.join("bad-facing.pins.json");
    std::fs::write(&bad_facing, r#"{"inputs": {"a": {"at": [1,1,1], "outside": "up"}}}"#)
        .expect("the pins file is writable");
    let (output, _) = run_in_scratch(
        "pins-bad-facing",
        &["and4", "--grown", "--pins", bad_facing.to_str().expect("utf-8 path")],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("pins file") && stderr.contains("\"up\"") && stderr.contains("\"a\""),
        "the refusal names the port and the bad facing: {stderr}"
    );

    // An output label the circuit does not declare is refused by name, with
    // the labels that would have worked.
    let scratch = scratch_dir("pins-bad-label");
    let bad_label = scratch.join("bad-label.pins.json");
    std::fs::write(&bad_label, r#"{"outputs": {"q": {"at": [5,1,2], "outside": "north"}}}"#)
        .expect("the pins file is writable");
    let (output, _) = run_in_scratch(
        "pins-bad-label",
        &["and4", "--grown", "--pins", bad_label.to_str().expect("utf-8 path")],
    );
    assert!(!output.status.success());
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("\"q\"") && stderr.contains('y'),
        "the refusal names the label and offers the real ones: {stderr}"
    );

    // Pins compile through the generation front door only.
    let scratch = scratch_dir("pins-without-grown");
    let lawful = scratch.join("lawful.pins.json");
    std::fs::write(&lawful, r#"{"inputs": {"a": {"at": [21,1,62], "outside": "south"}}}"#)
        .expect("the pins file is writable");
    let (output, _) = run_in_scratch(
        "pins-without-grown",
        &["and4", "--pins", lawful.to_str().expect("utf-8 path")],
    );
    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("--pins requires --grown"),
        "the refusal names the missing flag: {}",
        stderr_of(&output)
    );
}

/// The round trip: a pins file in, `compile_grown` under `--grown --pins`,
/// and a sidecar out that records each pinned port as its terminal cell plus
/// its `outside` facing -- the output keyed by the display label the caller
/// pinned it under, unpinned ports keeping today's bare `[x,y,z]` -- while
/// the world itself carries the terminal dust with the outside cell empty.
///
/// and4, because it grows at iteration 1 in milliseconds; the pin geometry
/// mirrors the acceptance tests' shape (input row south of the free layout,
/// output north of it, outsides facing away from the circuit).
#[test]
fn a_pinned_and4_round_trips_through_the_flags() {
    let scratch = scratch_dir("pins-round-trip");
    let pins = scratch.join("and4.pins.json");
    std::fs::write(
        &pins,
        r#"{"inputs": {"a": {"at": [21, 1, 62], "outside": "south"}},
            "outputs": {"y": {"at": [53, 1, 10], "outside": "north"}}}"#,
    )
    .expect("the pins file is writable");

    let (output, scratch) = run_in_scratch(
        "pins-round-trip",
        &["and4", "--grown", "--pins", pins.to_str().expect("utf-8 path")],
    );
    assert!(
        output.status.success(),
        "the pinned and4 compiles through the flags:\n{}",
        stderr_of(&output)
    );

    let sidecar = std::fs::read_to_string(scratch.join("output/and4.grown.pinout.json"))
        .expect("the pinout sidecar is written");
    assert!(
        sidecar.contains(r#""a":{"at":[21,1,62],"outside":"south"}"#),
        "the pinned input records its terminal cell and facing: {sidecar}"
    );
    assert!(
        sidecar.contains(r#""y":{"at":[53,1,10],"outside":"north"}"#),
        "the pinned output is keyed by its display label: {sidecar}"
    );
    assert!(
        !sidecar.contains("g6"),
        "the internal signal name does not leak into a pinned entry: {sidecar}"
    );
    assert!(
        sidecar.contains(r#""b":["#) && sidecar.contains(r#""c":["#),
        "unpinned ports keep today's bare coordinates: {sidecar}"
    );

    // The world behind the sidecar honours the contract at both pinned
    // cells: one dust at the terminal, nothing in the outside cell.
    let dump = std::fs::read_to_string(scratch.join("output/and4.grown.blocks.txt"))
        .expect("the block dump is written");
    let cell = |x: i32, y: i32, z: i32| -> Option<String> {
        let prefix = format!("{x} {y} {z} ");
        dump.lines().find(|line| line.starts_with(&prefix)).map(str::to_string)
    };
    for (x, y, z) in [(21, 1, 62), (53, 1, 10)] {
        let terminal = cell(x, y, z).unwrap_or_else(|| panic!("({x}, {y}, {z}) holds a block"));
        assert!(
            terminal.contains("RedstoneWire"),
            "the terminal at ({x}, {y}, {z}) is one redstone dust: {terminal}"
        );
    }
    assert_eq!(cell(21, 1, 63), None, "`a`'s outside cell ships empty");
    assert_eq!(cell(53, 1, 9), None, "`y`'s outside cell ships empty");
}
