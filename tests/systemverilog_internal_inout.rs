use reda::frontend::evaluate::Evaluator;
use reda::frontend::{
    compile_systemverilog, CompileArtifact, CompileOptions, Diagnostic, DiagnosticKind, SourceInput,
};

fn result(source: &str) -> Result<CompileArtifact, Vec<Diagnostic>> {
    compile_systemverilog(
        &[SourceInput {
            name: "internal_inout.sv",
            text: source,
        }],
        &CompileOptions::new("m"),
    )
}
fn native(source: &str) -> CompileArtifact {
    result(source).unwrap_or_else(|errors| panic!("{errors:?}"))
}

const HIERARCHY: &str = "module driver(input [3:0] a,inout wire [3:0] bus); assign bus=a ^ 4'h5; endmodule module reader(inout wire [3:0] bus,output [3:0] y); assign y=bus; endmodule module inverted_reader(inout wire [3:0] bus,output [3:0] y); assign y=~bus; endmodule module wrapper(input [3:0] a,inout wire [3:0] bus,output [3:0] y,z); driver d(.a(a),.bus(bus)); reader r(.bus(bus),.y(y)); inverted_reader n(.bus(bus),.y(z)); endmodule module m(input [3:0] a,output [3:0] y,z); wire [3:0] shared; wrapper w(.a(a),.bus(shared),.y(y),.z(z)); endmodule";
const FIXED: &str =
    "module m(input [3:0] a,output [3:0] y,z); assign y=a ^ 4'h5; assign z=~y; endmodule";

#[test]
fn shared_internal_inout_is_one_driver_and_many_readers_across_scopes() {
    let artifact = native(HIERARCHY);
    let fixed = native(FIXED);
    assert_eq!(artifact.netlist, fixed.netlist);
    assert_eq!(artifact.netlist, native(HIERARCHY).netlist);
    assert_eq!(artifact.debug.to_json(), native(HIERARCHY).debug.to_json());
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..16 {
        let inputs = (0..4)
            .map(|bit| (format!("a[{bit}]"), address & (1 << bit) != 0))
            .collect();
        let values = evaluator.evaluate(&inputs).unwrap();
        for bit in 0..4 {
            assert_eq!(
                values[&artifact.output_map()[&format!("y[{bit}]")]],
                (address ^ 5) & (1 << bit) != 0
            );
            assert_eq!(
                values[&artifact.output_map()[&format!("z[{bit}]")]],
                (address ^ 10) & (1 << bit) != 0
            );
        }
    }
}

#[test]
fn input_driven_aliases_selections_and_concatenations_preserve_bit_order() {
    let source = "module reader(inout wire [3:0] p,output [3:0] y); assign y=p; endmodule module m(input [3:0] a,output [3:0] y); reader r(.p({a[1:0],a[3:2]}),.y(y)); endmodule";
    let artifact = native(source);
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..16 {
        let inputs = (0..4)
            .map(|bit| (format!("a[{bit}]"), address & (1 << bit) != 0))
            .collect();
        let values = evaluator.evaluate(&inputs).unwrap();
        let swapped = ((address & 3) << 2) | ((address >> 2) & 3);
        for bit in 0..4 {
            assert_eq!(
                values[&artifact.output_map()[&format!("y[{bit}]")]],
                swapped & (1 << bit) != 0
            );
        }
    }
}

#[test]
fn alias_class_validation_rejects_writers_cycles_and_undriven_nets() {
    for (source, message) in [
        ("module d(input a,inout b); assign b=a; endmodule module m(input a,c,output y); wire b; d x(.a(a),.b(b)); d z(.a(c),.b(b)); assign y=b; endmodule", "more than one assignment"),
        ("module d(inout b); assign b=1'b1; endmodule module m(input a,output y); d x(.b(a)); assign y=a; endmodule", "primary input and an assignment"),
        ("module d(inout b); assign b=~b; endmodule module m(output y); wire b; d x(.b(b)); assign y=b; endmodule", "depends on itself"),
        ("module r(inout b,output y); assign y=b; endmodule module m(output y); wire b; r x(.b(b),.y(y)); endmodule", "no driver"),
        ("module r(inout [1:0] b,output y); assign y=b[0]; endmodule module m(input a,output y); r x(.b(a),.y(y)); endmodule", "inout `b` width mismatch"),
        ("module r(inout b,output y); assign y=b; endmodule module m(input a,output y); r x(.b(~a),.y(y)); endmodule", "assignment target"),
    ] {
        let errors = result(source).unwrap_err();
        assert!(errors.iter().any(|error| error.kind == DiagnosticKind::Source && error.message.contains(message)), "{source}: {errors:?}");
    }
}

#[test]
fn top_level_inout_remains_explicitly_unsupported_without_external_drive_contract() {
    let errors = result("module m(inout a,output y); assign y=a; endmodule").unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.kind == DiagnosticKind::Unsupported
            && error
                .message
                .contains("external read/drive/enable contract")));
}

#[test]
fn internal_inout_hierarchy_matches_fixed_physical_world_for_all_transitions() {
    use reda::compile::{compile, lowering::lower_optimised};
    use reda::redstone::{simulator::Simulator, world::block::BlockKind};
    let artifact = native(HIERARCHY);
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let fixed = lower_optimised(&native(FIXED).netlist).unwrap();
    assert_eq!(lowered, fixed);
    let physical = compile(&lowered).unwrap();
    let control = compile(&fixed).unwrap();
    let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    assert_eq!(fingerprint(&physical.world), fingerprint(&control.world));
    physical.validate_rewrite_view(&lowered).unwrap();
    let implementation = physical.implementation_netlist(&lowered);
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
                        _ => panic!("unreadable signal {}", gate.output),
                    };
                    assert_eq!(
                        actual, expected[&gate.output],
                        "phase {phases}: {}",
                        gate.output
                    );
                }
                for (port, mask) in [("y", 5), ("z", 10)] {
                    for bit in 0..4 {
                        let at = physical.output_positions
                            [&artifact.output_map()[&format!("{port}[{bit}]")]];
                        assert_eq!(
                            simulator.world().get(at.0, at.1, at.2).lit,
                            (address ^ mask) & (1 << bit) != 0
                        );
                    }
                }
                phases += 1;
            }
        }
    }
    assert_eq!(phases, 512);
}

#[test]
fn native_and_auto_cli_generate_internal_inout_and_typed_parameters_without_yosys() {
    let path = std::env::temp_dir().join(format!("reda-inout-native-{}.sv", std::process::id()));
    for source in [HIERARCHY,
        "module c #(parameter logic [3:0] W=4'h4, parameter logic signed [7:0] V=4'shf)(input [W-1:0] a,output [W-1:0] y); assign y=a ^ V[W-4'h1:0]; endmodule module m(input [3:0] a,output [3:0] y); c #(.W(8'h14)) u(.a(a),.y(y)); endmodule",
    ] {
        std::fs::write(&path,source).unwrap();
        for mode in ["native","auto"] {
            let output=std::process::Command::new(env!("CARGO_BIN_EXE_mc_dump"))
                .args(["verilog",path.to_str().unwrap(),"m","--frontend",mode])
                .env("REDA_PYTHON","/definitely/missing/python").output().unwrap();
            let error=String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(),"{mode}: {error}");
            assert!(error.contains("frontend: native"),"{error}");
            assert!(!output.stdout.is_empty());
        }
    }
    std::fs::remove_file(path).unwrap();
}
