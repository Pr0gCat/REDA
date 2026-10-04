use reda::compile::fragment_synth::support_composition::compile_disjoint_support;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

const SOURCE: &str = "module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=~(c&d); assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";

#[test]
fn six_inputs_constants_direct_aliases_and_repeated_function_compose() {
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "support.sv",
            text: SOURCE,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let source = &artifact.netlist;
    assert_eq!(source.inputs.len(), 6);
    let result = compile_disjoint_support(source, 1_000_000).unwrap();
    let certificate = result
        .certify_runtime(
            &reda::compile::fragment_synth::config::CertificationConfig::from_search(
                &reda::compile::fragment_synth::config::SearchConfig::checked_defaults(),
            ),
        )
        .unwrap();
    assert!(certificate.metrics().transition_count > 0);
    assert_eq!(result.source(), source);
    assert_eq!(result.implemented().inputs.len(), 6);
    assert_eq!(result.source_output_positions().len(), 9);
    assert_eq!(
        result
            .implemented_observations()
            .gate_output_positions
            .len(),
        result.implemented().gates.len()
    );
}

#[test]
fn public_alias_occurrences_keep_distinct_lamps_through_all_8192_retained_phases() {
    use reda::frontend::evaluate::Evaluator;
    use reda::redstone::{simulator::Simulator, world::block::BlockKind};
    use std::collections::{BTreeMap, BTreeSet};
    let original = SOURCE.replace("assign w=~(c&d);", "assign w=c&d;");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "aliases.sv",
            text: &original,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let public: Vec<_> = artifact.ordered_output_ports.iter().collect();
    // The core now retains every public output occurrence; historical eight-output
    // failure and full physical evidence remain in the retained logs.
    assert_eq!(artifact.netlist.outputs.len(), 9);
    assert_eq!(public.len(), 9);
    assert_eq!(
        artifact.port("v").unwrap().signal,
        artifact.port("w").unwrap().signal
    );
    let mut source = artifact.netlist.clone();
    source.outputs = public.iter().map(|p| p.signal.clone()).collect();
    let rewrite = compile_disjoint_support(&source, 1_000_000).unwrap();
    assert_eq!(
        rewrite
            .source_output_positions()
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        9
    );
    let compiled = reda::compile::CompiledCircuit::from_certified_rewrite(rewrite.clone());
    compiled.validate_rewrite_view(&source).unwrap();
    assert_eq!(compiled.output_occurrences().len(), public.len());
    for ((signal, position), (port, sealed)) in compiled
        .output_occurrences()
        .iter()
        .zip(public.iter().zip(rewrite.source_output_positions()))
    {
        assert_eq!(signal, &port.signal);
        assert_eq!(position, sealed);
    }
    let evaluator = Evaluator::new(rewrite.implemented()).unwrap();
    let mut simulator = Simulator::new(compiled.world.clone());
    simulator.run_until_stable(2000).unwrap();
    let mut phases = 0;
    for from in 0..64u32 {
        for to in 0..64u32 {
            for address in [from, to] {
                let values: BTreeMap<_, _> = ["a", "b", "c", "d", "e", "f"]
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| (name.to_string(), address & (1 << bit) != 0))
                    .collect();
                for (index, name) in source.inputs.iter().enumerate() {
                    let at = rewrite.source_input_positions()[index];
                    let mut lever = simulator.world().get(at.0, at.1, at.2).clone();
                    lever.lit = values[name];
                    simulator.world_mut().set(at.0, at.1, at.2, lever);
                }
                simulator.run_until_stable(2000).unwrap();
                let expected = evaluator.settle(&values).unwrap();
                for gate in &rewrite.implemented().gates {
                    let at = rewrite.implemented_observations().gate_output_positions[&gate.output];
                    let state = simulator.world().get(at.0, at.1, at.2);
                    let actual = match state.kind {
                        BlockKind::RedstoneWire => state.power > 0,
                        BlockKind::Torch
                        | BlockKind::WallTorch
                        | BlockKind::Repeater
                        | BlockKind::Comparator => state.lit,
                        _ => panic!("unreadable {}", gate.output),
                    };
                    assert_eq!(
                        actual, expected[&gate.output],
                        "phase {phases}: {}",
                        gate.output
                    );
                }
                let a = values["a"];
                let b = values["b"];
                let c = values["c"];
                let d = values["d"];
                let e = values["e"];
                let f = values["f"];
                let truth = BTreeMap::from([
                    ("u", a | b),
                    ("z", false),
                    ("o", true),
                    ("v", c & d),
                    ("w", c & d),
                    ("x", c),
                    ("y", e ^ f),
                    ("q", !(e | f)),
                    ("r", f),
                ]);
                for (index, port) in public.iter().enumerate() {
                    let at = rewrite.source_output_positions()[index];
                    assert_eq!(
                        simulator.world().get(at.0, at.1, at.2).lit,
                        truth[port.name.as_str()],
                        "phase {phases}: {}",
                        port.name
                    );
                }
                phases += 1;
            }
        }
    }
    assert_eq!(phases, 8192);
}

#[test]
fn declaration_and_identifier_changes_preserve_the_composed_world() {
    use reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "original.sv",
            text: SOURCE,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let first = compile_disjoint_support(&artifact.netlist, 1_000_000).unwrap();
    let mut changed = artifact.netlist.clone();
    changed.gates.reverse();
    for gate in &mut changed.gates {
        gate.name = format!("renamed_{}", gate.name);
        gate.output = format!("renamed_{}", gate.output);
        for input in &mut gate.inputs {
            *input = format!("renamed_{input}");
        }
    }
    for name in changed.inputs.iter_mut().chain(&mut changed.outputs) {
        *name = format!("renamed_{name}");
    }
    let second = compile_disjoint_support(&changed, 1_000_000).unwrap();
    assert_eq!(
        canonical_world_fingerprint(first.physical().world()),
        canonical_world_fingerprint(second.physical().world())
    );
}

#[test]
fn public_positive_budget_observes_a_certified_structural_candidate() {
    use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisBudget, SynthesisInput};
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "consumer.sv",
            text: SOURCE,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let lowered = reda::compile::lowering::lower_optimised(&artifact.netlist).unwrap();
    let input = || SynthesisInput {
        lowered: &lowered,
        source_provenance: None,
        pins: None,
    };
    let baseline = compile_fragment_synth(input(), SynthesisBudget::Evaluations(0)).unwrap();
    let searched = compile_fragment_synth(input(), SynthesisBudget::Evaluations(3)).unwrap();
    assert_eq!(baseline.evaluations_used, 0);
    assert!(baseline.trace.is_empty());
    assert_eq!(searched.trace.len(), 3);
    assert!(
        searched.trace[2].certified_quality.is_some(),
        "{:?}",
        searched.trace[2]
    );
    searched.compiled.validate_rewrite_view(&lowered).unwrap();
}
