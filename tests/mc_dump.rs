//! Black-box regression tests for the public `mc_dump` command.

use std::process::Command;

fn record_count(dump: &str, record: &str) -> usize {
    dump.lines().filter(|line| line.starts_with(record)).count()
}

/// Replacing `lower_optimised` with ordinary `lower` regresses this dump to
/// 56 gates and 12,348 blocks.  Execute the binary rather than asserting on
/// its implementation so the dump consumed by the Minecraft harness remains
/// the contract.
#[test]
fn official_verilog_seven_segment_dump_uses_optimised_lowering() {
    let output = Command::new(env!("CARGO_BIN_EXE_mc_dump"))
        .arg("verilog:seven_segment")
        .output()
        .expect("mc_dump binary must run");

    assert!(
        output.status.success(),
        "mc_dump failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dump = String::from_utf8(output.stdout).expect("mc_dump must emit UTF-8 text");
    assert_eq!(record_count(&dump, "GATE "), 47);
    assert_eq!(record_count(&dump, "GATEOUT "), 47);
    assert_eq!(record_count(&dump, "BLOCK "), 10_088);
}
