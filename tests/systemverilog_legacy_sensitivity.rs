use reda::frontend::evaluate::Evaluator;
use reda::frontend::{
    compile_systemverilog, CompileArtifact, CompileOptions, Diagnostic, DiagnosticKind, SourceInput,
};
fn result(source: &str) -> Result<CompileArtifact, Vec<Diagnostic>> {
    compile_systemverilog(
        &[SourceInput {
            name: "legacy_sensitivity.v",
            text: source,
        }],
        &CompileOptions::new("m"),
    )
}
fn native(source: &str) -> CompileArtifact {
    result(source).unwrap_or_else(|errors| panic!("{errors:?}"))
}

const ORDERED:&str="module worker(input [3:0] a,output reg [3:0] y,z); reg [3:0] t; always @(a) begin t=a ^ 4'h5; y=t; t=~t; z=t; end endmodule module m(input [3:0] a,output [3:0] y,z); worker w(.a(a),.y(y),.z(z)); endmodule";
const FIXED: &str =
    "module m(input [3:0] a,output [3:0] y,z); assign y=a ^ 4'h5; assign z=a ^ 4'ha; endmodule";

#[test]
fn explicit_or_comma_and_wildcard_processes_follow_literal_blocking_order() {
    // Preserves the original formerly unsupported frontend_selection source.
    for source in [
        "module m(input a, b, output reg y); always @(a or b) y = a & b; endmodule",
        "module m(input a, b, output reg y); always @(a,b) y = a & b; endmodule",
        "module m(input a,b,output reg y); always @(a or b) begin y=a; y=y ^ b; end endmodule",
        "module m(input a,b,output reg y); always @* begin y=a; y=y ^ b; end endmodule",
        "module m(input a,b,output logic y); always_comb begin y=a; y=y ^ b; end endmodule",
    ] {
        let artifact = native(source);
        let evaluator = Evaluator::new(&artifact.netlist).unwrap();
        for address in 0..4 {
            let a = address & 1 != 0;
            let b = address & 2 != 0;
            let values = evaluator
                .evaluate(&[("a".to_owned(), a), ("b".to_owned(), b)].into())
                .unwrap();
            let expected = if source.contains("y = a & b") {
                a && b
            } else {
                a ^ b
            };
            assert_eq!(values[&artifact.output_map()["y"]], expected, "{source}");
        }
    }
    let artifact = native(ORDERED);
    assert_eq!(artifact.netlist, native(FIXED).netlist);
    assert_eq!(artifact.netlist, native(ORDERED).netlist);
    assert_eq!(artifact.debug.to_json(), native(ORDERED).debug.to_json());
}

#[test]
fn concatenated_targets_read_one_rhs_snapshot_before_any_target_write() {
    let simple = native("module m(input a,b,output reg x,y); always @* {x,y}={a,b}; endmodule");
    assert_eq!(
        simple.netlist,
        native("module m(input a,b,output x,y); assign x=a; assign y=b; endmodule").netlist
    );
    let source="module m(input a,b,output reg [1:0] y); reg p,q; always @(a,b) begin {p,q}={a,b}; {p,q}={q,p}; y={p,q}; end endmodule";
    let artifact = native(source);
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..4 {
        let a = address & 1 != 0;
        let b = address & 2 != 0;
        let values = evaluator
            .evaluate(&[("a".to_owned(), a), ("b".to_owned(), b)].into())
            .unwrap();
        assert_eq!(values[&artifact.output_map()["y[0]"]], a);
        assert_eq!(values[&artifact.output_map()["y[1]"]], b);
    }
}

#[test]
fn sensitivity_and_read_after_write_use_flattened_alias_classes() {
    let reader="module r(inout p,q,output reg y); always @(p) y=~q; endmodule module m(input a,output y); r u(.p(a),.q(a),.y(y)); endmodule";
    let artifact = native(reader);
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for a in [false, true] {
        assert_eq!(
            evaluator.evaluate(&[("a".to_owned(), a)].into()).unwrap()[&artifact.output_map()["y"]],
            !a
        );
    }
    let writer="module r(input a,b,inout p,q,output reg y); always @(a,b) begin p=a & b; y=~q; end endmodule module m(input a,b,output y); wire shared; r u(.a(a),.b(b),.p(shared),.q(shared),.y(y)); endmodule";
    let artifact = native(writer);
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..4 {
        let a = address & 1 != 0;
        let b = address & 2 != 0;
        assert_eq!(
            evaluator
                .evaluate(&[("a".to_owned(), a), ("b".to_owned(), b)].into())
                .unwrap()[&artifact.output_map()["y"]],
            !(a && b)
        );
    }
}

#[test]
fn incomplete_events_storage_and_conflicting_processes_remain_rejected() {
    for (source, message) in [
        (
            "module m(input a,b,output reg y); always @(a) y=a & b; endmodule",
            "incomplete combinational sensitivity",
        ),
        (
            "module m(input [1:0] a,output reg y); always @(a[0]) y=a[1]; endmodule",
            "incomplete combinational sensitivity",
        ),
        (
            "module m(input a,output reg y); wire t; assign t=a; always @(a) y=t; endmodule",
            "incomplete combinational sensitivity",
        ),
        (
            "module m(input a,output reg y); reg t; always @(t) begin t=a; y=t; end endmodule",
            "incomplete combinational sensitivity",
        ),
        (
            "module m(input a,output reg y); reg t; always @(a) begin y=t; t=a; end endmodule",
            "read-before-write",
        ),
        (
            "module m(input a,output reg y); always @(a) y=y ^ a; endmodule",
            "read-before-write",
        ),
        (
            "module m(input a,output reg y); always @(a) y=a; always @(a) y=~a; endmodule",
            "more than one assignment",
        ),
        (
            "module m(input a,output reg y); always @(a) case(a) 1'b1:y=a; endcase endmodule",
            "default",
        ),
    ] {
        let errors = result(source).unwrap_err();
        assert!(
            errors.iter().any(|e| e.message.contains(message)),
            "{source}: {errors:?}"
        );
    }
    let errors =
        result("module m(input a,b,output reg y); always @(a or posedge b) y=a; endmodule")
            .unwrap_err();
    assert!(errors
        .iter()
        .any(|e| e.kind == DiagnosticKind::Unsupported && e.message.contains("mixed edge")));
}

#[test]
fn complete_explicit_case_uses_existing_case_lowering_and_literal_truth() {
    let source="module m(input a,b,output reg y); always @(a,b) case(a) 1'b0:y=b; default:y=~b; endcase endmodule";
    let artifact = native(source);
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..4 {
        let a = address & 1 != 0;
        let b = address & 2 != 0;
        assert_eq!(
            evaluator
                .evaluate(&[("a".to_owned(), a), ("b".to_owned(), b)].into())
                .unwrap()[&artifact.output_map()["y"]],
            a ^ b
        );
    }
}

#[test]
fn projected_temporary_reads_retain_each_source_bit_origin() {
    let source="module m(input [3:0] a,output reg [3:0] y); reg [3:0] t; always @(a) begin t=a ^ 4'h5; y=t; end endmodule";
    let artifact = native(source);
    let start = source.find("y=t").unwrap() + 2;
    let read = artifact
        .debug
        .source_nodes
        .iter()
        .position(|node| node.span.start as usize == start && node.span.end as usize == start + 1)
        .unwrap();
    let mut bits = std::collections::BTreeSet::new();
    for origin in artifact.debug.logic_origins.iter().chain(
        artifact
            .debug
            .extra_origins
            .iter()
            .map(|(_, origin)| origin),
    ) {
        if artifact.debug.elab_nodes[origin.elab.0 as usize].source.0 as usize == read {
            bits.insert(origin.bit);
        }
    }
    assert_eq!(bits, [0, 1, 2, 3].into());
}

#[test]
fn ordered_hierarchy_matches_fixed_physical_outputs_for_all_retained_transitions() {
    check_physical(
        ORDERED,
        FIXED,
        &["a[0]", "a[1]", "a[2]", "a[3]"],
        |address| {
            [("y", 5), ("z", 10)]
                .into_iter()
                .flat_map(|(port, mask)| {
                    (0..4).map(move |bit| {
                        (format!("{port}[{bit}]"), (address ^ mask) & (1 << bit) != 0)
                    })
                })
                .collect()
        },
    );
}

fn check_physical(
    source: &str,
    reference: &str,
    input_names: &[&str],
    truth: impl Fn(u32) -> std::collections::BTreeMap<String, bool>,
) {
    use reda::compile::{compile, lowering::lower_optimised};
    use reda::redstone::{simulator::Simulator, world::block::BlockKind};
    let artifact = native(source);
    let lowered = lower_optimised(&artifact.netlist).unwrap();
    let fixed = lower_optimised(&native(reference).netlist).unwrap();
    assert_eq!(lowered, fixed);
    let physical = compile(&lowered).unwrap();
    let control = compile(&fixed).unwrap();
    let fingerprint = reda::compile::metrics::canonical_world_fingerprint;
    assert_eq!(fingerprint(&physical.world), fingerprint(&control.world));
    let implementation = &lowered;
    let evaluator = Evaluator::new(implementation).unwrap();
    let mut sim = Simulator::new(physical.world.clone());
    sim.run_until_stable(2000).unwrap();
    let mut phases = 0;
    for from in 0..16 {
        for to in 0..16 {
            for address in [from, to] {
                let inputs: std::collections::BTreeMap<String, bool> = input_names
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| ((*name).to_owned(), address & (1 << bit) != 0))
                    .collect();
                for (name, at) in &physical.input_positions {
                    let mut lever = sim.world().get(at.0, at.1, at.2).clone();
                    lever.lit = inputs[name];
                    sim.world_mut().set(at.0, at.1, at.2, lever);
                }
                sim.run_until_stable(2000).unwrap();
                let implemented_inputs = physical
                    .input_positions
                    .iter()
                    .map(|(name, at)| {
                        let lever = sim.world().get(at.0, at.1, at.2);
                        assert_eq!(lever.kind, BlockKind::Lever);
                        (name.clone(), lever.lit)
                    })
                    .collect();
                let expected = evaluator.settle(&implemented_inputs).unwrap();
                for gate in &implementation.gates {
                    let at = physical.gate_output_positions[&gate.output];
                    let block = sim.world().get(at.0, at.1, at.2);
                    let actual = match block.kind {
                        BlockKind::RedstoneWire => block.power > 0,
                        BlockKind::Torch
                        | BlockKind::WallTorch
                        | BlockKind::Repeater
                        | BlockKind::Comparator => block.lit,
                        _ => panic!("unreadable {}", gate.output),
                    };
                    assert_eq!(
                        actual, expected[&gate.output],
                        "phase {phases}: {}",
                        gate.output
                    );
                }
                for (port, expected) in truth(address) {
                    let at = physical.output_positions[&artifact.output_map()[&port]];
                    assert_eq!(
                        sim.world().get(at.0, at.1, at.2).lit,
                        expected,
                        "phase {phases}: {port}"
                    );
                }
                phases += 1;
            }
        }
    }
    assert_eq!(phases, 512);
}

#[test]
fn full_legacy_seven_segment_explicit_events_match_the_original_physical_control() {
    let control =
        include_str!("fixtures/seven_segment_legacy.v").replace("module seven_segment", "module m");
    let source = control.replace("always @(*)", "always @(d3 or d2 or d1 or d0)");
    check_physical(&source, &control, &["d0", "d1", "d2", "d3"], |digit| {
        ["a", "b", "c", "d", "e", "f", "g"]
            .into_iter()
            .enumerate()
            .map(|(bit, name)| {
                let value = reda::circuits::seven_segment::TRUTH_TABLE
                    .get(digit as usize)
                    .map_or(false, |row| row[bit] != 0);
                (name.to_owned(), value)
            })
            .collect()
    });
}

#[test]
fn actual_native_and_auto_cli_generate_complete_explicit_lists_without_yosys() {
    let path = std::env::temp_dir().join(format!("reda-legacy-events-{}.v", std::process::id()));
    std::fs::write(&path, ORDERED).unwrap();
    for mode in ["native", "auto"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_mc_dump"))
            .args(["verilog", path.to_str().unwrap(), "m", "--frontend", mode])
            .env("REDA_PYTHON", "/definitely/missing/python")
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{mode}: {error}");
        assert!(error.contains("frontend: native"), "{error}");
        assert!(!output.stdout.is_empty());
    }
    std::fs::remove_file(path).unwrap();
}
