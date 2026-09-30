use reda::frontend::{synthesize_selected, FrontendMode, SourceInput};
use std::process::Command;

#[test]
fn native_auto_and_forced_yosys_compile_same_function() {
    let source = SourceInput {
        name: "and4.v",
        text: include_str!("fixtures/and4.v"),
    };
    let native = synthesize_selected(source.clone(), "and4", FrontendMode::Auto).unwrap();
    assert_eq!(native.frontend, FrontendMode::Native);
    assert!(native.fallback_reason.is_none());
    let yosys = synthesize_selected(source, "and4", FrontendMode::Yosys).unwrap();
    assert_eq!(yosys.frontend, FrontendMode::Yosys);
    for mask in 0..16 {
        let inputs = ["a", "b", "c", "d"]
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name.to_string(), mask & (1 << i) != 0))
            .collect();
        for result in [&native, &yosys] {
            let values = reda::frontend::evaluate::Evaluator::new(&result.netlist)
                .unwrap()
                .evaluate(&inputs)
                .unwrap();
            assert_eq!(values[&result.outputs["y"]], mask == 15);
        }
    }
}

#[test]
fn explicit_unsupported_falls_back_but_native_rejects() {
    let source = SourceInput {
        name: "legacy.v",
        text: "module m(input a, output reg y); always @* y = a; endmodule",
    };
    assert!(synthesize_selected(source.clone(), "m", FrontendMode::Native).is_err());
    let result = synthesize_selected(source, "m", FrontendMode::Auto).unwrap();
    assert_eq!(result.frontend, FrontendMode::Yosys);
    assert!(result.fallback_reason.unwrap().contains("legacy.v:1:"));
}

#[test]
fn cli_reports_selection_and_preserves_errors_without_yosys() {
    let command = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mc_dump"))
            .args(args)
            .env("REDA_PYTHON", "/definitely/missing/python")
            .output()
            .unwrap()
    };
    for mode in ["auto", "native"] {
        let output = command(&["verilog:and4", "--frontend", mode]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("frontend: native"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("frontend:"));
    }
    let path = std::env::temp_dir().join(format!("reda-selection-{}.sv", std::process::id()));
    std::fs::write(&path, "module m(input a, output y); assign y = ; endmodule").unwrap();
    let output = command(&["verilog", path.to_str().unwrap(), "m", "--frontend", "auto"]);
    std::fs::remove_file(path).unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("expected"), "{error}");
    assert!(
        !error.contains("Python") && !error.contains("fallback"),
        "{error}"
    );
    assert!(!command(&["verilog:and4", "--frontend", "invalid"])
        .status
        .success());
    assert!(!command(&["verilog:and4", "--frontend", "yosys"])
        .status
        .success());
}

#[test]
fn build_circuit_verification_failure_does_not_retry_frontend() {
    let path =
        std::env::temp_dir().join(format!("reda-selection-pins-{}.json", std::process::id()));
    std::fs::write(&path, "invalid json").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_build_circuit"))
        .args([
            "verilog:and4",
            "--frontend",
            "auto",
            "--synth",
            "--pins",
            path.to_str().unwrap(),
        ])
        .env("REDA_PYTHON", "/definitely/missing/python")
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        error.contains("frontend: native") && error.contains("pins file"),
        "{error}"
    );
    assert!(
        !error.contains("fallback") && !error.contains("Python"),
        "{error}"
    );
}

#[test]
fn cli_exercises_real_yosys_fallback_and_forced_mode() {
    for args in [
        vec!["verilog:seven_segment", "--frontend", "auto"],
        vec!["verilog:and4", "--frontend", "yosys"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_mc_dump"))
            .args(&args)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{error}");
        assert!(error.contains("frontend: yosys"), "{error}");
        assert_eq!(
            error.contains("fallback reason:"),
            args[0] == "verilog:seven_segment"
        );
    }
}
