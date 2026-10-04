use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, DiagnosticKind, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

fn native(text: &str) -> Result<reda::frontend::CompileArtifact, Vec<reda::frontend::Diagnostic>> {
    compile_systemverilog(
        &[SourceInput {
            name: "minimum.sv",
            text,
        }],
        &CompileOptions::new("m"),
    )
}

#[test]
fn minimum_integer_survives_dependencies_overrides_and_physical_switching() {
    let source = "module word #(parameter int VALUE=0)(input [3:0] a,output [3:0] q); localparam LAST=VALUE+2147483647; assign q=a ^ {VALUE[31],LAST[0],VALUE[0],VALUE[31]}; endmodule module m #(parameter int MIN=-(2147483648))(input [3:0] a,output [3:0] q); word #(.VALUE(MIN)) u(.a(a),.q(q)); endmodule";
    let artifact = native(source).unwrap();
    let repeat = native(source).unwrap();
    let fixed =
        native("module m(input [3:0] a,output [3:0] q); assign q=a ^ 4'b1101; endmodule").unwrap();
    assert_eq!(artifact.netlist, fixed.netlist);
    assert_eq!(artifact.netlist, repeat.netlist);
    assert_eq!(artifact.debug.to_json(), repeat.debug.to_json());
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let physical = compile(&lowered).unwrap();
    let again = compile(&lowered).unwrap();
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&physical.world),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&again.world)
    );
    physical.validate_rewrite_view(&lowered).unwrap();
    let implementation = physical.implementation_netlist(&lowered);
    let oracle = Evaluator::new(implementation).unwrap();
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
                for gate in &implementation.gates {
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
                        (address ^ 13) & (1 << bit) != 0
                    );
                }
                phases += 1;
            }
        }
    }
    assert_eq!(phases, 512);
}

#[test]
fn minimum_literal_does_not_admit_out_of_range_positive_or_negated_values() {
    for value in [
        "2147483648",
        "-2147483649",
        "-(-2147483648)",
        "-2147483648-1",
    ] {
        let source = format!(
            "module m #(parameter int VALUE={value})(output [31:0] q); assign q=VALUE; endmodule"
        );
        let errors = native(&source).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.kind == DiagnosticKind::Unsupported
                    && error.message.contains("signed 32-bit")),
            "{value}: {errors:?}"
        );
    }
}
