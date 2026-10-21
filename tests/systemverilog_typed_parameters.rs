use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileArtifact, CompileOptions, SourceInput};

fn native(source: &str) -> CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "typed_parameters.sv",
            text: source,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap_or_else(|errors| panic!("{errors:?}"))
}

fn word(source: &str, width: u32) -> u64 {
    let artifact = native(source);
    let values = Evaluator::new(&artifact.netlist)
        .unwrap()
        .evaluate(&Default::default())
        .unwrap();
    (0..width).fold(0, |value, bit| {
        let name = if width == 1 {
            "y".to_owned()
        } else {
            format!("y[{bit}]")
        };
        value | ((values[&artifact.output_map()[&name]] as u64) << bit)
    })
}

#[test]
fn sized_inferred_and_explicit_parameters_preserve_width_and_value() {
    for (source, width, expected) in [
        ("module m #(parameter P=4'hf)(output [3:0] y); assign y=P; endmodule", 4, 15),
        ("module m #(parameter logic [3:0] P=8'hfa)(output [3:0] y); assign y=P; endmodule", 4, 10),
        ("module m #(parameter logic signed [7:0] P=4'shf)(output [7:0] y); assign y=P; endmodule", 8, 255),
        ("module m #(parameter logic [7:0] P=4'hf)(output [7:0] y); assign y=P; endmodule", 8, 15),
        ("module m #(parameter int unsigned P=32'hffffffff)(output [31:0] y); assign y=P; endmodule", 32, u32::MAX as u64),
        ("module m #(parameter P=64'hfedcba9876543210)(output [63:0] y); assign y=P; endmodule", 64, 0xfedcba9876543210),
        ("module m #(parameter signed P=8'hff)(output [7:0] y); assign y=P; endmodule", 8, 255),
        ("module m #(parameter logic [3:0] A=8'hfa,B=8'hfb)(output [3:0] y); assign y=B; endmodule", 4, 11),
    ] {
        assert_eq!(word(source, width), expected, "{source}");
    }
}

#[test]
fn integral_arithmetic_uses_common_width_signedness_and_modular_result() {
    for (expression, width, expected) in [
        ("4'hf+4'h1", 4, 0),
        ("4'h0-4'h1", 4, 15),
        ("4'shf+8'sh01", 8, 0),
        ("4'shf+8'h01", 8, 16),
        ("64'hffffffffffffffff*64'hffffffffffffffff", 64, 1),
        ("-8'sh80", 8, 128),
        ("-8'hff", 8, 1),
    ] {
        let source = format!(
            "module m #(parameter P={expression})(output [{}:0] y); assign y=P; endmodule",
            width - 1
        );
        assert_eq!(word(&source, width), expected, "{expression}");
    }
}

#[test]
fn overrides_are_coerced_in_child_scope_before_dependent_ranges() {
    let source = "module child #(parameter logic [3:0] WIDTH=4'h2, parameter logic signed [7:0] VALUE=8'h0)(input [WIDTH-1:0] a,output [WIDTH-1:0] y); localparam LAST=WIDTH-4'h1; assign y=a ^ VALUE[LAST:0]; endmodule module m(input [3:0] a,output [3:0] y); child #(.WIDTH(8'h14),.VALUE(4'shf)) u(.a(a),.y(y)); endmodule";
    let positional = source.replace("#(.WIDTH(8'h14),.VALUE(4'shf))", "#(8'h14,4'shf)");
    let artifact = native(source);
    let reference = native("module m(input [3:0] a,output [3:0] y); assign y=a ^ 4'hf; endmodule");
    assert_eq!(artifact.netlist, reference.netlist);
    assert_eq!(artifact.netlist, native(&positional).netlist);
    assert_eq!(artifact.netlist, native(source).netlist);
    assert_eq!(artifact.debug.to_json(), native(source).debug.to_json());
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..16 {
        let inputs: std::collections::BTreeMap<String, bool> = (0..4)
            .map(|bit| (format!("a[{bit}]"), address & (1 << bit) != 0))
            .collect();
        let values = evaluator.evaluate(&inputs).unwrap();
        for bit in 0..4 {
            assert_eq!(
                values[&artifact.output_map()[&format!("y[{bit}]")]],
                (address ^ 15) & (1 << bit) != 0
            );
        }
    }
}

#[test]
fn original_formerly_unsupported_parameter_sources_are_native() {
    // Original frontend_selection rejection fixtures, preserved verbatim.
    for source in [
        "module m #(parameter logic [0:0] W=1'b1)(input a, output y); assign y = a; endmodule",
        "module m #(parameter int unsigned W=1)(input a,output y); assign y=a; endmodule",
    ] {
        let artifact = native(source);
        let evaluator = Evaluator::new(&artifact.netlist).unwrap();
        for a in [false, true] {
            assert_eq!(
                evaluator.evaluate(&[("a".to_owned(), a)].into()).unwrap()
                    [&artifact.output_map()["y"]],
                a
            );
        }
    }
}

#[test]
fn selections_and_unsupported_types_preserve_validation() {
    for source in [
        "module m #(parameter P=4'hf)(output y); assign y=P[4]; endmodule",
        "module m #(parameter logic [64:0] P=1)(output y); assign y=P[0]; endmodule",
        "module m #(parameter P=65'h0)(output y); assign y=P[0]; endmodule",
        "module m #(parameter logic [0:3] P=1)(output y); assign y=P[0]; endmodule",
        "module m #(parameter logic [P:0] P=1)(output y); assign y=P[0]; endmodule",
        "module m #(parameter real P=1)(output y); assign y=1'b0; endmodule",
    ] {
        assert!(
            compile_systemverilog(
                &[SourceInput {
                    name: "invalid.sv",
                    text: source
                }],
                &CompileOptions::new("m")
            )
            .is_err(),
            "{source}"
        );
    }
}

#[test]
fn typed_hierarchy_matches_fixed_physical_implementation_for_all_transitions() {
    use reda::compile::{compile, lowering::lower_optimised};
    use reda::redstone::{simulator::Simulator, world::block::BlockKind};
    let source = "module child #(parameter logic [3:0] WIDTH=4'h2, parameter logic signed [7:0] VALUE=8'h0)(input [WIDTH-1:0] a,output [WIDTH-1:0] y); localparam LAST=WIDTH-4'h1; assign y=a ^ VALUE[LAST:0]; endmodule module m(input [3:0] a,output [3:0] y); child #(.WIDTH(8'h14),.VALUE(4'shf)) u(.a(a),.y(y)); endmodule";
    let artifact = native(source);
    let fixed = native("module m(input [3:0] a,output [3:0] y); assign y=a ^ 4'hf; endmodule");
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let fixed_lowered = lower_optimised(&fixed.netlist).unwrap();
    assert_eq!(lowered, fixed_lowered);
    let physical = compile(&lowered).unwrap();
    let fixed_physical = compile(&fixed_lowered).unwrap();
    let fingerprint = reda::compile::metrics::canonical_world_fingerprint;
    assert_eq!(
        fingerprint(&physical.world),
        fingerprint(&fixed_physical.world)
    );
    let implementation = &lowered;
    let evaluator = Evaluator::new(implementation).unwrap();
    let mut simulator = Simulator::new(physical.world.clone());
    simulator.run_until_stable(2000).unwrap();
    let mut phases = 0;
    for from in 0..16 {
        for to in 0..16 {
            for address in [from, to] {
                let inputs: std::collections::BTreeMap<String, bool> = (0..4)
                    .map(|bit| (format!("a[{bit}]"), address & (1 << bit) != 0))
                    .collect();
                for (name, at) in &physical.input_positions {
                    let mut lever = simulator.world().get(at.0, at.1, at.2).clone();
                    lever.lit = inputs[name];
                    simulator.world_mut().set(at.0, at.1, at.2, lever);
                }
                simulator.run_until_stable(2000).unwrap();
                let expected = evaluator.settle(&inputs).unwrap();
                for gate in &implementation.gates {
                    let at = physical.gate_output_positions[&gate.output];
                    let block = simulator.world().get(at.0, at.1, at.2);
                    let actual = match block.kind {
                        BlockKind::RedstoneWire => block.power > 0,
                        BlockKind::Torch
                        | BlockKind::WallTorch
                        | BlockKind::Repeater
                        | BlockKind::Comparator => block.lit,
                        _ => panic!("unreadable implementation signal {}", gate.output),
                    };
                    assert_eq!(
                        actual, expected[&gate.output],
                        "phase {phases}: {}",
                        gate.output
                    );
                }
                for bit in 0..4 {
                    let at =
                        physical.output_positions[&artifact.output_map()[&format!("y[{bit}]")]];
                    assert_eq!(
                        simulator.world().get(at.0, at.1, at.2).lit,
                        (address ^ 15) & (1 << bit) != 0
                    );
                }
                phases += 1;
            }
        }
    }
    assert_eq!(phases, 512);
}
