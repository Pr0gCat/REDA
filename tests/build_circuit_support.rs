//! `build_circuit` must not write a layout that Minecraft would take apart:
//! a component with nothing to stand or hang on. The default path used to
//! write one for `a ^ b` (dust on dust) and for the built-in `full_adder`,
//! and REDA's own simulator called both correct.

use std::path::Path;
use std::process::Command;

use reda::compile::fragment_synth::certification::unsupported_component;
use reda::formats::litematic;

const XOR2: &str = "module xor2(input a, input b, output y);\n  assign y = a ^ b;\nendmodule\n";

/// Runs `build_circuit` with `args` in a scratch directory and checks the
/// invariant: either the layout is written and every component is supported,
/// or the run fails, says why, and writes nothing.
fn assert_default_path_is_sound(name: &str, args: &[&str]) {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("build-circuit-support");
    std::fs::create_dir_all(&scratch).expect("the scratch directory is creatable");
    std::fs::write(scratch.join("xor2.v"), XOR2).expect("the source is writable");
    let written = scratch.join(format!("output/{name}.litematic"));
    let _ = std::fs::remove_file(&written);

    let output = Command::new(env!("CARGO_BIN_EXE_build_circuit"))
        .args(args)
        .current_dir(&scratch)
        .output()
        .expect("build_circuit must run");

    if output.status.success() {
        let world = litematic::load(&written).expect("a written layout must load");
        assert_eq!(
            unsupported_component(&world),
            None,
            "{name}: build_circuit wrote a layout with an unsupported component"
        );
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("nothing to stand or hang on"),
            "{name}: a refusal must say why:\n{stderr}"
        );
        assert!(
            !written.exists(),
            "{name}: a refused layout must not be written"
        );
    }
}

#[test]
fn default_path_never_writes_a_layout_with_an_unsupported_component() {
    assert_default_path_is_sound("xor2", &["verilog", "xor2.v", "xor2"]);
    assert_default_path_is_sound("full_adder", &["full_adder"]);
}
