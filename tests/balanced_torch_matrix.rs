use reda::compile::{
    fragment_synth::{
        benchmark::canonical_world_fingerprint,
        torch_row::{compile_balanced_torch_matrix, TorchRowError},
    },
    Gate, Netlist,
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

fn native(text: &str, top: &str) -> Netlist {
    compile_systemverilog(
        &[SourceInput { name: top, text }],
        &CompileOptions::new(top),
    )
    .unwrap()
    .netlist
}

#[test]
fn full_source_boundaries_are_certified_without_name_selection() {
    for (text, top) in [
        (
            include_str!("fixtures/seven_segment.v"),
            "bcd_seven_segment",
        ),
        (include_str!("fixtures/rom16x7.sv"), "rom16x7"),
    ] {
        let source = native(text, top);
        let first = compile_balanced_torch_matrix(&source, 10_000_000).unwrap();
        assert_eq!(first.source().inputs, source.inputs);
        assert_eq!(first.source().outputs, source.outputs);
        assert_eq!(first.source_input_positions().len(), source.inputs.len());
        assert_eq!(first.source_output_positions().len(), source.outputs.len());
        assert_eq!(
            first.source_output_driver_positions().len(),
            source.outputs.len()
        );
        let compiled = reda::compile::CompiledCircuit::from_certified_rewrite(first.clone());
        assert_eq!(
            compiled.implementation_netlist(&source).outputs.len(),
            source.outputs.len()
        );
        let watched = reda::timing::watch_all_nets(&compiled);
        let labels: std::collections::BTreeSet<_> = watched.iter().map(|(_, name)| name).collect();
        assert_eq!(labels.len(), watched.len());
        let projected = reda::compile::primitive_graph::from_certified_rewrite(&first);
        let primitives: usize = first
            .implemented_candidate()
            .instances
            .instances
            .iter()
            .map(|instance| instance.expanded.topology.primitives.len())
            .sum();
        assert_eq!(
            projected.nodes.len(),
            primitives + source.inputs.len() + source.outputs.len()
        );
        let renamed = |name: &str| format!("arbitrary::{name}");
        let mut alternative = Netlist {
            inputs: source.inputs.iter().map(|s| renamed(s)).collect(),
            outputs: source.outputs.iter().map(|s| renamed(s)).collect(),
            gates: source
                .gates
                .iter()
                .map(|g| Gate {
                    name: renamed(&g.name),
                    output: renamed(&g.output),
                    inputs: g.inputs.iter().map(|s| renamed(s)).collect(),
                    kind: g.kind,
                })
                .collect(),
        };
        alternative.gates.reverse();
        let second = compile_balanced_torch_matrix(&alternative, 10_000_000).unwrap();
        assert_eq!(
            canonical_world_fingerprint(first.physical().world()),
            canonical_world_fingerprint(second.physical().world())
        );
        assert!(compile_balanced_torch_matrix(&source, 0).is_err());
    }
}

#[test]
fn invalid_stateful_and_out_of_range_boundaries_fail_closed() {
    let source = native(
        include_str!("fixtures/seven_segment.v"),
        "bcd_seven_segment",
    );
    let mut invalid = source.clone();
    invalid.gates[0].inputs[0] = "undriven".into();
    assert!(compile_balanced_torch_matrix(&invalid, 10_000_000).is_err());
    let mut stateful = source.clone();
    stateful.gates.push(Gate {
        name: "q".into(),
        output: "q".into(),
        inputs: vec![source.inputs[0].clone(), source.inputs[1].clone()],
        kind: reda::compile::topology::GateKind::DffPosedge,
    });
    assert!(compile_balanced_torch_matrix(&stateful, 10_000_000).is_err());
    let mut zero = source.clone();
    zero.gates.push(Gate::nor("always_high", &[]));
    zero.gates.push(Gate::nor("always_low", &["always_high"]));
    zero.outputs[0] = "always_low".into();
    let low = compile_balanced_torch_matrix(&zero, 10_000_000).unwrap();
    let compiled = reda::compile::CompiledCircuit::from_certified_rewrite(low);
    assert!(reda::compile::selection::qualify_boundary(&zero, &compiled).is_some());
    let mut too_wide = source.clone();
    too_wide.outputs.push(source.outputs[0].clone());
    assert!(matches!(
        compile_balanced_torch_matrix(&too_wide, 10_000_000),
        Err(TorchRowError::UnsupportedBoundary)
    ));
}

#[test]
fn collector_end_repeater_and_output_aliases_have_distinct_landings() {
    use reda::redstone::simulator::Simulator;
    let inputs: Vec<_> = (0..4).map(|bit| format!("address{bit}")).collect();
    for count in [3, 7, 11, 15] {
        let mut gates = vec![];
        for input in &inputs {
            gates.push(Gate::nor(format!("inverse_{input}"), &[input]));
        }
        let mut terms = vec![];
        for address in 0..count {
            let mismatch: Vec<_> = inputs
                .iter()
                .enumerate()
                .map(|(bit, input)| {
                    if address & (1 << bit) == 0 {
                        input.clone()
                    } else {
                        format!("inverse_{input}")
                    }
                })
                .collect();
            let name = format!("term{address}");
            gates.push(Gate::nor(
                &name,
                &mismatch.iter().map(String::as_str).collect::<Vec<_>>(),
            ));
            terms.push(name);
        }
        gates.push(Gate::merge(
            "value",
            &terms.iter().map(String::as_str).collect::<Vec<_>>(),
        ));
        let source = Netlist {
            inputs: inputs.clone(),
            outputs: vec!["value".into(); 7],
            gates,
        };
        let result = compile_balanced_torch_matrix(&source, 10_000_000).unwrap();
        let mut sim = Simulator::new(result.physical().world().clone());
        sim.run_until_stable(2000).unwrap();
        for from in 0..16 {
            for to in 0..16 {
                for address in [from, to] {
                    for (bit, &(x, y, z)) in result.source_input_positions().iter().enumerate() {
                        let mut state = sim.world().get(x, y, z).clone();
                        state.lit = address & (1 << bit) != 0;
                        sim.world_mut().set(x, y, z, state);
                    }
                    sim.run_until_stable(2000).unwrap();
                    for &(x, y, z) in result.source_output_positions() {
                        assert_eq!(sim.world().get(x, y, z).lit, address < count);
                    }
                    for &(x, y, z) in result.source_output_driver_positions() {
                        let state = sim.world().get(x, y, z);
                        let value = if state.kind
                            == reda::redstone::world::block::BlockKind::RedstoneWire
                        {
                            state.power > 0
                        } else {
                            state.lit
                        };
                        assert_eq!(value, address < count);
                    }
                }
            }
        }
    }
}

#[test]
fn all_low_high_and_mixed_constants_preserve_every_declared_output() {
    for words in [
        vec!["low"; 7],
        vec!["high"; 7],
        vec!["low", "high", "input0", "low", "high", "input3", "low"],
    ] {
        let source = Netlist {
            inputs: (0..4).map(|bit| format!("input{bit}")).collect(),
            outputs: words.iter().map(|word| word.to_string()).collect(),
            gates: vec![Gate::nor("high", &[]), Gate::nor("low", &["high"])],
        };
        let result = compile_balanced_torch_matrix(&source, 10_000_000).unwrap();
        assert_eq!(result.source_output_positions().len(), 7);
        assert_eq!(result.source_output_driver_positions().len(), 7);
        let compiled = reda::compile::CompiledCircuit::from_certified_rewrite(result);
        assert!(reda::compile::selection::qualify_boundary(&source, &compiled).is_some());
    }
}

#[test]
fn public_default_selects_the_complete_quality_checked_matrix() {
    for (text, top) in [
        (
            include_str!("fixtures/seven_segment.v"),
            "bcd_seven_segment",
        ),
        (include_str!("fixtures/rom16x7.sv"), "rom16x7"),
    ] {
        let source = native(text, top);
        let expected = compile_balanced_torch_matrix(&source, 10_000_000).unwrap();
        let actual = reda::compile::compile(&source).unwrap();
        assert_eq!(
            canonical_world_fingerprint(&actual.world),
            canonical_world_fingerprint(expected.physical().world())
        );
        assert!(actual.physical_rewrite().is_some());
        let recursive = reda::compile::fragment_synth::compile_fragment_synth(
            reda::compile::fragment_synth::SynthesisInput {
                lowered: &source, source_provenance: None, pins: None,
            },
            reda::compile::fragment_synth::SynthesisBudget::Evaluations(0),
        ).unwrap();
        assert_eq!(canonical_world_fingerprint(&recursive.compiled.world),
            canonical_world_fingerprint(&actual.world));
        assert_eq!(recursive.evaluations_used, 0);
        assert!(recursive.compiled.physical_rewrite().is_some());
        assert!(reda::compile::evaluation::CircuitEvaluation::from_synthesis(
            &source, &recursive,
        ).is_ok());
    }
}

#[test]
fn recursive_matrix_root_retains_constant_and_repeated_boundary_occurrences() {
    let mut source = Netlist {
        inputs: (0..4).map(|bit| format!("input{bit}")).collect(),
        outputs: ["low", "high", "copy0", "low", "high", "copy3", "low"]
            .into_iter().map(str::to_owned).collect(),
        gates: vec![Gate::nor("high", &[]), Gate::nor("low", &["high"]),
            Gate::merge("copy0", &["input0"]), Gate::merge("copy3", &["input3"])],
    };
    // A large source with unused internal work still has this complete public
    // boundary. The rewrite owns its implementation, not invented source sites.
    let mut previous = "input0".to_owned();
    for index in 0..33 {
        let next = format!("unused{index}");
        source.gates.push(Gate::nor(&next, &[&previous]));
        previous = next;
    }
    let result = reda::compile::fragment_synth::compile_fragment_synth(
        reda::compile::fragment_synth::SynthesisInput {
            lowered: &source, source_provenance: None, pins: None,
        }, reda::compile::fragment_synth::SynthesisBudget::Evaluations(0),
    ).unwrap();
    let rewrite = result.compiled.physical_rewrite().unwrap();
    assert_eq!(rewrite.source_output_positions().len(), source.outputs.len());
    assert_eq!(rewrite.source_output_driver_positions().len(), source.outputs.len());
    assert!(result.compiled.validate_rewrite_view(&source).is_ok());
    assert!(reda::compile::selection::qualify_boundary(&source, &result.compiled).is_some());
    assert!(reda::compile::evaluation::CircuitEvaluation::from_synthesis(&source, &result).is_ok());
    assert_eq!(result.evaluations_used, 0);
}
