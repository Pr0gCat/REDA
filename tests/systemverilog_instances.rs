use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, DiagnosticKind, SourceInput};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

fn native(text: &str, top: &str) -> reda::frontend::CompileArtifact {
    compile_systemverilog(
        &[SourceInput {
            name: "instances.sv",
            text,
        }],
        &CompileOptions::new(top),
    )
    .unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn nested_parameter_instances_reach_all_retained_physical_outputs() {
    let text = include_str!("fixtures/hierarchy_bits.sv");
    let artifact = native(text, "hierarchy_bits");
    let repeat = native(text, "hierarchy_bits");
    let fixed = native(
        "module hierarchy_bits(input [3:0] a, output [3:0] q); assign q=a ^ 4'b0101; endmodule",
        "hierarchy_bits",
    );
    assert_eq!(artifact.netlist, fixed.netlist);
    assert_eq!(artifact.netlist, repeat.netlist);
    assert_eq!(artifact.debug.to_json(), repeat.debug.to_json());
    assert_eq!(
        artifact.debug.signal_bits("low.core.q").unwrap().bits.len(),
        2
    );
    assert_eq!(
        artifact
            .debug
            .signal_bits("high.core.q")
            .unwrap()
            .bits
            .len(),
        2
    );
    let restored =
        reda::frontend::debug::DebugDatabase::from_json(&artifact.debug.to_json()).unwrap();
    assert_eq!(restored, artifact.debug);
    assert_eq!(
        restored.signal_bits("low.core.q"),
        artifact.debug.signal_bits("low.core.q")
    );
    assert!(fixed
        .debug
        .signals
        .iter()
        .all(|binding| binding.name.is_none()));
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
    let mut sim = Simulator::new(physical.bench.clone());
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
fn independent_overrides_preserve_scope_and_instance_provenance() {
    let text = "module word #(parameter W=2, MASK=W-1)(input [W-1:0] a,output [W-1:0] q); assign q=a ^ MASK[W-1:0]; endmodule module m(input [3:0] a, output [3:0] q); word #(.W(1)) one(.a(a[0]),.q(q[0])); word #(3,5) three(a[3:1],q[3:1]); endmodule";
    let artifact = native(text, "m");
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for address in 0..16 {
        let inputs = (0..4)
            .map(|i| (format!("a[{i}]"), address & (1 << i) != 0))
            .collect();
        let values = evaluator.evaluate(&inputs).unwrap();
        for bit in 0..4 {
            assert_eq!(
                values[&artifact.output_map()[&format!("q[{bit}]")]],
                (address ^ 10) & (1 << bit) != 0
            );
        }
    }
    let mut scopes = std::collections::BTreeSet::new();
    for node in &artifact.debug.elab_nodes {
        scopes.insert(node.instance.0);
    }
    assert_eq!(scopes, [0, 1, 2].into_iter().collect());
    assert_eq!(artifact.debug.signal_bits("one.q").unwrap().bits.len(), 1);
    assert_eq!(artifact.debug.signal_bits("three.q").unwrap().bits.len(), 3);
    assert_eq!(artifact.debug.signal_bits("a").unwrap().bits.len(), 4);
    assert_eq!(artifact.debug.to_json(), native(text, "m").debug.to_json());
}

#[test]
fn nested_register_clock_aliases_preserve_state_and_single_clock_contract() {
    let text="module regbit(input d,clk,en,output reg q); always @(posedge clk) if(en) q<=d; endmodule module wrapper(input d,clk,en,output q); regbit r(d,clk,en,q); endmodule module m(input d,clk,en,output q); wrapper w(.clk(clk),.en(en),.q(q),.d(d)); endmodule";
    let artifact = native(text, "m");
    let flat = native(
        "module m(input d,clk,en,output reg q); always @(posedge clk) if(en) q<=d; endmodule",
        "m",
    );
    assert_eq!(artifact.netlist, flat.netlist);
    let mut evaluator = Evaluator::new(&artifact.netlist).unwrap();
    let mut previous_clock = false;
    let mut expected = false;
    for index in 0..256 {
        let clk = index % 3 != 0;
        let d = index % 5 < 2;
        let en = index % 7 < 4;
        if clk && !previous_clock && en {
            expected = d;
        }
        previous_clock = clk;
        let values = evaluator
            .step(
                &[("d".into(), d), ("clk".into(), clk), ("en".into(), en)]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        assert_eq!(values[&artifact.output_map()["q"]], expected);
    }
}

#[test]
fn invalid_hierarchy_connections_and_drivers_are_rejected() {
    let child="module word #(parameter W=1)(input [W-1:0] a,output [W-1:0] q); localparam L=1; assign q=a; endmodule ";
    for (body, message) in [
        ("missing u(a,q);", "unknown module"),
        ("word u(.a(a),.q(q),.q(q));", "duplicate port"),
        ("word u(.a(a),.bad(q));", "unknown or excess port"),
        ("word #(.L(1)) u(a,q);", "local or excess parameter"),
        ("word #(.W(1),.W(1)) u(a,q);", "duplicate parameter"),
        ("word u(a,q),u(a,q);", "duplicate declaration"),
        ("word a(a,q);", "duplicate declaration"),
        ("word u(a,a); assign q=a;", "input"),
        ("word u(a,q),v(a,q);", "more than one assignment"),
        ("word u(q,q);", "combinational loop"),
        ("word u(a,q); assign q=a;", "more than one assignment"),
    ] {
        let text = format!("{child} module m(input a,output q); {body} endmodule");
        let errors = compile_systemverilog(
            &[SourceInput {
                name: "invalid.sv",
                text: &text,
            }],
            &CompileOptions::new("m"),
        )
        .unwrap_err();
        assert!(
            errors.iter().all(|e| e.kind == DiagnosticKind::Source),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.message.contains(message)),
            "expected {message}: {errors:?}"
        );
    }
    for text in [
        "module m(input a,output q); m recursive(a,q); endmodule",
        "module other(input a,output q); m inner(a,q); endmodule module m(input a,output q); other inner(a,q); endmodule",
    ] {
        let errors=compile_systemverilog(&[SourceInput{name:"recursive.sv",text}],&CompileOptions::new("m")).unwrap_err();
        assert!(errors.iter().any(|e|e.message.contains("recursive module")),"{errors:?}");
    }
}

#[test]
fn independent_instance_clocks_bind_to_their_own_parent_ports() {
    let text="module r(input d,c,output reg q); always @(posedge c) q<=d; endmodule module m(input d,c1,c2,output q1,q2); r a(d,c1,q1), b(d,c2,q2); endmodule";
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "clocks.sv",
            text,
        }],
        &CompileOptions::new("m"),
    )
    .expect("separate instances retain independent scalar clocks");
    let clocks = artifact
        .netlist
        .gates
        .iter()
        .filter(|gate| gate.kind == reda::compile::topology::GateKind::DffPosedge)
        .map(|gate| gate.inputs[1].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(clocks, ["c1", "c2"].into_iter().collect());
}

#[test]
fn hierarchy_links_modules_across_files_and_concatenated_output_targets() {
    let artifact = compile_systemverilog(
        &[
            SourceInput {
                name: "parent.sv",
                text: "module m(input a,b,output x,y); child u(.q({x,y}),.a({a,b})); endmodule",
            },
            SourceInput {
                name: "child.sv",
                text: "module child(input [1:0] a,output [1:0] q); assign q=~a; endmodule",
            },
        ],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let evaluator = Evaluator::new(&artifact.netlist).unwrap();
    for (a, b) in [(false, false), (false, true), (true, false), (true, true)] {
        let values = evaluator
            .evaluate(&[("a".into(), a), ("b".into(), b)].into_iter().collect())
            .unwrap();
        assert_eq!(values[&artifact.output_map()["x"]], !a);
        assert_eq!(values[&artifact.output_map()["y"]], !b);
    }
}

#[test]
fn full_seven_segment_hierarchy_preserves_the_independent_flat_netlist() {
    let hierarchical = native(
        include_str!("fixtures/hierarchy_seven_segment.sv"),
        "hierarchy_seven_segment",
    );
    let flat = native(
        include_str!("fixtures/seven_segment.v"),
        "bcd_seven_segment",
    );
    assert_eq!(hierarchical.netlist, flat.netlist);
    assert_eq!(hierarchical.output_map(), flat.output_map());
    assert_eq!(
        hierarchical.debug.to_json(),
        native(
            include_str!("fixtures/hierarchy_seven_segment.sv"),
            "hierarchy_seven_segment"
        )
        .debug
        .to_json()
    );
}

#[test]
fn unsupported_open_and_implicit_connections_preserve_fallback_classification() {
    for connection in [".a(a)", ".a(a),.q()", ".a,.q(q)", ".*"] {
        let text=format!("module child(input a,output q); assign q=a; endmodule module m(input a,output q); child u({connection}); endmodule");
        let errors = compile_systemverilog(
            &[SourceInput {
                name: "ports.sv",
                text: &text,
            }],
            &CompileOptions::new("m"),
        )
        .unwrap_err();
        assert!(
            errors.iter().all(|e| e.kind == DiagnosticKind::Unsupported),
            "{errors:?}"
        );
    }
}
