use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use reda::compile::fragment_synth::benchmark::{verified_capture_commit, BenchmarkBaseline};
use reda::compile::fragment_synth::manifest::TransitionManifest;
use reda::compile::revisions::{
    cell_library_revision, physical_verifier_revision, simulator_revision,
};
use reda::compile::topology::Library;

const EVALUATOR_COMMIT: &str = "afe577d9d04c98f18c65f7d9fca1c5634629d237";
const CASE_NAMES: [&str; 6] = [
    "and4",
    "verilog:and4",
    "full_adder",
    "segment_a",
    "seven_segment",
    "pinned:verilog:seven_segment",
];

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fragment_synth_baseline.json")
}

fn read_baseline(path: &Path) -> BenchmarkBaseline {
    serde_json::from_slice(&std::fs::read(path).expect("baseline file is readable"))
        .expect("baseline file matches the immutable schema")
}

fn manifest_for_case(name: &str) -> TransitionManifest {
    let ports: &[&str] = match name {
        "and4" | "verilog:and4" => &["a", "b", "c", "d"],
        "full_adder" => &["a", "b", "cin"],
        "segment_a" | "seven_segment" | "pinned:verilog:seven_segment" => &["d3", "d2", "d1", "d0"],
        unknown => panic!("unexpected baseline case `{unknown}`"),
    };
    TransitionManifest::new(ports.iter().map(|port| (*port).to_string()).collect())
}

fn assert_schema(baseline: &BenchmarkBaseline) {
    assert!(!baseline.baseline_commit.is_empty());
    assert!(matches!(
        baseline.build_profile.as_str(),
        "debug" | "release"
    ));
    assert!(baseline.cargo_features.is_empty());
    assert_eq!(
        baseline.cell_library_revision,
        cell_library_revision(&Library::default_library())
    );
    assert_eq!(baseline.simulator_revision, simulator_revision());
    assert_eq!(baseline.verifier_revision, physical_verifier_revision());
    assert_eq!(
        baseline
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        CASE_NAMES
    );

    for case in &baseline.cases {
        let manifest = manifest_for_case(&case.name);
        assert!(!manifest.transitions().is_empty());
        assert_eq!(case.transition_count, manifest.transitions().len());
        assert!(case.transition_count > 0);
        assert_eq!(case.transition_manifest_hash, manifest.fingerprint());
        assert!(!case.lowered_netlist_hash.as_str().is_empty());
        assert!(!case.pin_manifest_hash.as_str().is_empty());

        if case.certified {
            assert!(!case
                .generated_world_fingerprint
                .as_ref()
                .expect("certified case records a world fingerprint")
                .as_str()
                .is_empty());
            assert!(
                case.physical
                    .as_ref()
                    .expect("certified case records physical metrics")
                    .non_air_blocks
                    > 0
            );
            assert!(case.max_observed_settle_game_ticks_on_manifest.is_some());
        } else {
            assert!(case.generated_world_fingerprint.is_none());
            assert!(case.physical.is_none());
            assert!(case.max_observed_settle_game_ticks_on_manifest.is_none());
        }
    }
}

fn fresh_temp_directory() -> PathBuf {
    let root = std::env::temp_dir();
    for suffix in 0..100 {
        let candidate = root.join(format!(
            "reda-fragment-baseline-test-{}-{suffix}",
            std::process::id()
        ));
        if std::fs::create_dir(&candidate).is_ok() {
            return candidate;
        }
    }
    panic!("could not allocate a fresh temporary baseline directory");
}

fn release_capture_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let built = Command::new(cargo)
                .args(["build", "--release", "--bin", "fragment_baseline"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("release fragment_baseline build launches");
            assert!(
                built.status.success(),
                "release fragment_baseline build failed:\n{}",
                String::from_utf8_lossy(&built.stderr)
            );

            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
            target
                .join("release")
                .join(format!("fragment_baseline{}", std::env::consts::EXE_SUFFIX))
        })
        .as_path()
}

fn run_capture(output: &Path, replace: bool) -> Output {
    let mut command = Command::new(release_capture_binary());
    command.arg("--output").arg(output);
    if replace {
        command.arg("--replace");
    }
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("fragment_baseline process launches")
}

fn certification_vector(baseline: &BenchmarkBaseline) -> Vec<bool> {
    baseline.cases.iter().map(|case| case.certified).collect()
}

fn verified_repository_head(absent_output: &Path) -> String {
    verified_capture_commit(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &std::env::current_dir().expect("current directory is readable"),
        absent_output,
    )
    .expect("the test runs from the exact clean repository root")
}

#[test]
fn checked_fixture_keeps_schema_order_revisions_hashes_and_new_coverage() {
    let baseline = read_baseline(&fixture_path());
    assert_schema(&baseline);
    assert_eq!(baseline.baseline_commit, EVALUATOR_COMMIT);
    assert_eq!(baseline.build_profile, "release");
    assert!(baseline.cases[..5].iter().all(|case| case.certified));
    assert!(!baseline.cases[5].certified);
}

#[test]
fn capture_refuses_to_overwrite_without_replace() {
    let temporary = fresh_temp_directory();
    let output = temporary.join("existing.json");
    std::fs::write(&output, b"immutable").unwrap();

    let refused = run_capture(&output, false);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--replace"));
    assert_eq!(std::fs::read(&output).unwrap(), b"immutable");

    std::fs::remove_dir_all(temporary).unwrap();
}

#[test]
fn two_fresh_capture_processes_are_byte_identical() {
    let temporary = fresh_temp_directory();
    let first_path = temporary.join("first.json");
    let second_path = temporary.join("second.json");

    let first = run_capture(&first_path, false);
    assert!(
        first.status.success(),
        "first capture failed:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = run_capture(&second_path, false);
    assert!(
        second.status.success(),
        "second capture failed:\n{}",
        String::from_utf8_lossy(&second.stderr)
    );

    assert_eq!(
        first
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        CASE_NAMES.len(),
        "capture prints one summary row per case"
    );
    let first_bytes = std::fs::read(&first_path).unwrap();
    let second_bytes = std::fs::read(&second_path).unwrap();
    assert_eq!(first_bytes, second_bytes);

    let checked = read_baseline(&fixture_path());
    let first_baseline = read_baseline(&first_path);
    let second_baseline = read_baseline(&second_path);
    assert_schema(&first_baseline);
    assert_schema(&second_baseline);
    assert_eq!(
        certification_vector(&first_baseline),
        certification_vector(&checked),
        "fresh capture must preserve all five certified legacy cases and explicit pinned coverage"
    );
    assert_eq!(
        certification_vector(&second_baseline),
        certification_vector(&checked)
    );
    let head = verified_repository_head(&temporary.join("absent-head-check.json"));
    assert_eq!(first_baseline.baseline_commit, head);
    assert_eq!(second_baseline.baseline_commit, head);

    std::fs::remove_dir_all(temporary).unwrap();
}
