use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, DiagnosticKind, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

fn native(text: &str, top: &str) -> reda::frontend::CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "parameters.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn parameter_widths_indices_and_values_reach_retained_physical_outputs() {
    let text = include_str!("fixtures/parameter_bits.sv");
    let artifact = native(text, "parameter_bits");
    let repeat = native(text, "parameter_bits");
    let fixed = native(
        "module parameter_bits(input [3:0] a, output [3:0] q); assign q=a ^ 4'b0101; endmodule",
        "parameter_bits",
    );
    assert_eq!(artifact.netlist, fixed.netlist);
    assert_eq!(artifact.netlist, repeat.netlist);
    assert_eq!(artifact.debug.to_json(), repeat.debug.to_json());
    assert_eq!(artifact.netlist.inputs.len(), 4);
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let physical = compile(&lowered).unwrap();
    let again = compile(&lowered).unwrap();
    assert_eq!(physical.world.size(), again.world.size());
    for (i, _) in physical.world.cells().iter().enumerate() {
        let (x, y, z) = physical.world.decode(i);
        assert_eq!(physical.world.get(x, y, z), again.world.get(x, y, z));
    }
    let oracle = Evaluator::new(&lowered).unwrap();
    let mut sim = Simulator::new(physical.world.clone());
    sim.run_until_stable(2000).unwrap();
    let mut phases = 0;
    for from in 0..16 {
        for to in 0..16 {
            for address in [from, to] {
                let inputs: std::collections::BTreeMap<String, bool> = (0..4)
                    .map(|bit| (format!("a[{bit}]"), address & (1 << bit) != 0))
                    .collect();
                for (name, at) in &physical.input_positions {
                    let mut lever = sim.world().get(at.0, at.1, at.2).clone();
                    lever.lit = inputs[name];
                    sim.world_mut().set(at.0, at.1, at.2, lever);
                }
                sim.run_until_stable(2000).unwrap();
                let expected = oracle.settle(&inputs).unwrap();
                for gate in &lowered.gates {
                    let at = physical.gate_output_positions[&gate.output];
                    let state = sim.world().get(at.0, at.1, at.2);
                    let actual = match state.kind {
                        BlockKind::RedstoneWire => state.power > 0,
                        BlockKind::Torch
                        | BlockKind::WallTorch
                        | BlockKind::Repeater
                        | BlockKind::Comparator => state.lit,
                        _ => panic!("unreadable implementation signal {}", gate.output),
                    };
                    assert_eq!(actual, expected[&gate.output]);
                }
                for bit in 0..4 {
                    let signal = &artifact.output_map()[&format!("q[{bit}]")];
                    let at = physical.output_positions[signal];
                    assert_eq!(
                        sim.world().get(at.0, at.1, at.2).lit,
                        (address ^ 5) & (1 << bit) != 0
                    );
                }
                phases += 1;
            }
        }
    }
    assert_eq!(phases, 512);
}

#[test]
fn constant_dependencies_are_scoped_and_arithmetic_has_correct_precedence() {
    let text = "module other #(parameter W=9)(input a,output y); assign y=a; endmodule module m(input [W-1:0] a, output y); localparam W = NEXT - 2; parameter NEXT = 2 + 2 * 3; assign y=a[W-1]; endmodule";
    let artifact = native(text, "m");
    assert_eq!(artifact.netlist.inputs.len(), 6);
    let oracle = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..64 {
        let inputs = (0..6)
            .map(|bit| (format!("a[{bit}]"), address & (1 << bit) != 0))
            .collect();
        assert_eq!(
            oracle.evaluate(&inputs).unwrap()[&artifact.output_map()["y"]],
            address & 32 != 0
        );
    }
    let word = native(
        "module m #(parameter int VALUE = -1)(output [31:0] y); assign y=VALUE; endmodule",
        "m",
    );
    let values = Evaluator::new(&word.netlist)
        .unwrap()
        .evaluate(&Default::default())
        .unwrap();
    assert!(word.output_map().values().all(|signal| values[signal]));
}

#[test]
fn invalid_constant_dependencies_ranges_and_writes_are_not_admitted() {
    for (text, message, kind) in [
        (
            "module m #(parameter W=0)(input [W-1:0] a, output y); assign y=a[0]; endmodule",
            "[N-1:0]",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter W=5000)(input [W-1:0] a, output y); assign y=a[0]; endmodule",
            "supported maximum",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter A=B, B=A)(input a,output y); assign y=a; endmodule",
            "cyclic parameter",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter W=UNKNOWN)(input a,output y); assign y=a; endmodule",
            "unknown parameter",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter W=1,W=2)(input a,output y); assign y=a; endmodule",
            "more than once",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter W=1)(input W,output y); assign y=W; endmodule",
            "already declared",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter W=1)(input a,output y); assign W=a; assign y=a; endmodule",
            "cannot be assigned",
            DiagnosticKind::Source,
        ),
        (
            "module m #(parameter W=2147483647+1)(input a,output y); assign y=a; endmodule",
            "signed 32-bit",
            DiagnosticKind::Unsupported,
        ),
        (
            "module m #(parameter W=65536*65536)(input a,output y); assign y=a; endmodule",
            "signed 32-bit",
            DiagnosticKind::Unsupported,
        ),
    ] {
        let errors = compile_systemverilog(
            &[SourceInput {
                name: "bad.sv",
                text,
            }],
            &CompileOptions::new("m"),
        )
        .unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains(message) && e.kind == kind),
            "{message}: {errors:?}"
        );
    }
}
