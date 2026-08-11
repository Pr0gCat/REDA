//! The pasteable schematic and the RCON/debug dump must compile the same
//! optimized Verilog circuit.  A `.litematic` that silently takes the older
//! lowering path is especially dangerous: the player sees a different circuit
//! from the one the simulator and conformance harness measured.

use std::process::Command;

fn gate_count(stdout: &str) -> usize {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("gate count: "))
        .expect("build_circuit must print its placed gate count")
        .parse()
        .expect("gate count must be an integer")
}

#[test]
fn verilog_litematic_uses_the_same_optimised_lowering_as_mc_dump() {
    let scratch = std::env::temp_dir().join(format!(
        "reda-verilog-litematic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time must be after the Unix epoch")
            .as_nanos(),
    ));
    std::fs::create_dir_all(&scratch).expect("create isolated litematic output directory");

    let build = Command::new(env!("CARGO_BIN_EXE_build_circuit"))
        .current_dir(&scratch)
        .arg("verilog:seven_segment")
        .output()
        .expect("run build_circuit");
    assert!(
        build.status.success(),
        "build_circuit failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let dump = Command::new(env!("CARGO_BIN_EXE_mc_dump"))
        .arg("verilog:seven_segment")
        .output()
        .expect("run mc_dump");
    assert!(
        dump.status.success(),
        "mc_dump failed:\n{}",
        String::from_utf8_lossy(&dump.stderr)
    );

    let litematic_gates = gate_count(&String::from_utf8_lossy(&build.stdout));
    let dumped_gates = String::from_utf8_lossy(&dump.stdout)
        .lines()
        .filter(|line| line.starts_with("GATE "))
        .count();
    assert_eq!(
        litematic_gates, dumped_gates,
        "the pasteable .litematic and mc_dump must use one lowering path"
    );
    assert!(
        scratch.join("output/verilog_seven_segment.litematic").is_file(),
        "build_circuit must actually write the requested schematic"
    );

    std::fs::remove_dir_all(&scratch).expect("remove isolated litematic output directory");
}
