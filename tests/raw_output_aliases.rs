use reda::compile::{
    compile_legacy, compile_planned, compile_without_rewrite,
    fragment_synth::torch_row::compile_player_torch_row, planner::PortPlacements, CompileError,
    CompiledCircuit, Gate, Netlist,
};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

fn assert_same_world(left: &CompiledCircuit, right: &CompiledCircuit) {
    assert_eq!(left.world.size(), right.world.size());
    for (flat, &index) in left.world.cells().iter().enumerate() {
        let (x, y, z) = left.world.decode(flat);
        assert_eq!(
            left.world.palette().get(index),
            Some(right.world.get(x, y, z))
        );
    }
}

fn planned(source: &Netlist) -> Result<CompiledCircuit, CompileError> {
    compile_planned(source, &PortPlacements::default())
}

#[test]
fn raw_output_aliases_keep_both_occurrences_at_the_actual_shared_lamp() {
    let single = Netlist {
        inputs: vec!["a".into()],
        outputs: vec!["same".into()],
        gates: vec![Gate::nor("same", &["a"])],
    };
    let aliases = Netlist {
        outputs: vec!["same".into(), "same".into()],
        ..single.clone()
    };
    for compiler in [compile_without_rewrite, planned, compile_legacy] {
        let before = compiler(&single).unwrap();
        let actual = compiler(&aliases).unwrap();
        assert_same_world(&before, &actual);
        assert!(actual.physical_rewrite().is_none());
        let lamp = actual.output_positions["same"];
        assert_eq!(
            actual.output_occurrences(),
            &[("same".into(), lamp), ("same".into(), lamp)]
        );
        assert_eq!(
            actual.world.get(lamp.0, lamp.1, lamp.2).kind,
            BlockKind::Lamp
        );
        let mut sim = Simulator::new(actual.world.clone());
        for from in [false, true] {
            for to in [false, true] {
                for input in [from, to] {
                    let (x, y, z) = actual.input_positions["a"];
                    let mut state = sim.world().get(x, y, z).clone();
                    state.lit = input;
                    sim.world_mut().set(x, y, z, state);
                    sim.run_until_stable(2000).unwrap();
                    for (_, (x, y, z)) in actual.output_occurrences() {
                        assert_eq!(sim.world().get(*x, *y, *z).lit, !input);
                    }
                }
            }
        }
    }
}

#[test]
fn raw_occurrences_follow_source_order_instead_of_map_order() {
    let source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["second".into(), "first".into(), "second".into()],
        gates: vec![Gate::nor("first", &["a"]), Gate::nor("second", &["b"])],
    };
    let actual = planned(&source).unwrap();
    assert_eq!(
        actual
            .output_occurrences()
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        source.outputs
    );
    for (name, position) in actual.output_occurrences() {
        assert_eq!(*position, actual.output_positions[name]);
    }
}

#[test]
fn rewrite_occurrences_keep_sealed_distinct_alias_lamps_and_truth() {
    let source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["same".into(), "other".into(), "same".into()],
        gates: vec![Gate::nor("same", &["a"]), Gate::nor("other", &["b"])],
    };
    let rewrite = compile_player_torch_row(&source, 100_000).unwrap();
    let original_world = rewrite.physical().world().clone();
    let sealed = rewrite.source_output_positions().to_vec();
    let actual = CompiledCircuit::from_certified_rewrite(rewrite);
    actual.validate_rewrite_view(&source).unwrap();
    assert_eq!(
        actual.output_occurrences(),
        source
            .outputs
            .iter()
            .cloned()
            .zip(sealed.iter().copied())
            .collect::<Vec<_>>()
    );
    assert_ne!(
        actual.output_occurrences()[0].1,
        actual.output_occurrences()[2].1
    );
    assert_eq!(
        actual.output_positions["same"],
        actual.output_occurrences()[2].1
    );
    for (flat, &index) in original_world.cells().iter().enumerate() {
        let (x, y, z) = original_world.decode(flat);
        assert_eq!(
            original_world.palette().get(index),
            Some(actual.world.get(x, y, z))
        );
    }
    let mut sim = Simulator::new(actual.world.clone());
    for from in 0u8..4 {
        for to in 0u8..4 {
            for input in [from, to] {
                for (bit, name) in source.inputs.iter().enumerate() {
                    let (x, y, z) = actual.input_positions[name];
                    let mut state = sim.world().get(x, y, z).clone();
                    state.lit = input & (1 << bit) != 0;
                    sim.world_mut().set(x, y, z, state);
                }
                sim.run_until_stable(2000).unwrap();
                for (signal, (x, y, z)) in actual.output_occurrences() {
                    let expected = input & (if signal == "same" { 1 } else { 2 }) == 0;
                    assert_eq!(sim.world().get(*x, *y, *z).kind, BlockKind::Lamp);
                    assert_eq!(sim.world().get(*x, *y, *z).lit, expected);
                }
            }
        }
    }
}

#[test]
fn explicit_alias_ownership_rejects_independent_corruptions() {
    use reda::compile::fragment_synth::identity::{ObservationId, PhysicalEndpointId, PortId};
    use reda::compile::fragment_synth::instance_graph::PhysicalSink;
    use reda::compile::fragment_synth::{
        legacy_adapter::LegacyCandidateAdapter, verify::certify_expanded_structure,
    };
    use reda::compile::topology::Library;
    let source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["same".into(), "other".into(), "same".into()],
        gates: vec![Gate::nor("same", &["a"]), Gate::nor("other", &["b"])],
    };
    let compiled = compile_legacy(&source).unwrap();
    let candidate = LegacyCandidateAdapter::adapt(&source, &compiled)
        .unwrap()
        .candidate;
    let library = Library::default_library();
    let certificate = certify_expanded_structure(&candidate, &source, &library).unwrap();
    let timing = reda::compile::fragment_synth::timing_graph::RealisedTimingGraph::derive(
        &candidate,
        &certificate,
    )
    .unwrap();
    let timing = timing.analyse().unwrap();
    use reda::compile::fragment_synth::identity::TimingNodeId;
    assert_eq!(
        timing.head[&TimingNodeId::DeclaredOutput(PortId(0))],
        timing.head[&TimingNodeId::DeclaredOutput(PortId(2))]
    );
    assert_eq!(
        timing.tail[&TimingNodeId::DeclaredOutput(PortId(0))],
        timing.tail[&TimingNodeId::DeclaredOutput(PortId(2))]
    );
    assert_eq!(
        candidate.output_boundary_aliases.get(&PortId(2)),
        Some(&PortId(0))
    );
    for mutation in 0..8 {
        let mut broken = candidate.clone();
        match mutation {
            0 => {
                broken.output_boundary_aliases.clear();
            }
            1 => {
                broken.output_boundary_aliases.insert(PortId(2), PortId(1));
            }
            2 => {
                broken.output_boundary_aliases.insert(PortId(2), PortId(2));
            }
            3 => {
                broken.output_boundary_aliases.insert(PortId(0), PortId(2));
            }
            4 => {
                broken
                    .observations
                    .get_mut(&ObservationId::DeclaredOutput(PortId(2)))
                    .unwrap()
                    .site
                    .at
                    .x += 1;
            }
            5 => {
                let block = broken.boundaries[&PhysicalEndpointId::DeclaredOutput(PortId(0))]
                    .blocks[0]
                    .clone();
                broken
                    .boundaries
                    .get_mut(&PhysicalEndpointId::DeclaredOutput(PortId(2)))
                    .unwrap()
                    .blocks
                    .push(block);
            }
            6 => {
                broken
                    .boundaries
                    .get_mut(&PhysicalEndpointId::DeclaredOutput(PortId(0)))
                    .unwrap()
                    .blocks
                    .clear();
            }
            7 => {
                let driver = broken
                    .instances
                    .assignments
                    .iter()
                    .find(|a| a.sink == PhysicalSink::DeclaredOutput(PortId(1)))
                    .unwrap()
                    .driver
                    .clone();
                broken
                    .instances
                    .assignments
                    .iter_mut()
                    .find(|a| a.sink == PhysicalSink::DeclaredOutput(PortId(2)))
                    .unwrap()
                    .driver = driver;
            }
            _ => unreachable!(),
        }
        assert_ne!(candidate.fingerprint(), broken.fingerprint());
        assert!(
            certify_expanded_structure(&broken, &source, &library).is_err(),
            "mutation {mutation} escaped certification"
        );
    }
}

#[test]
fn pinned_aliases_preserve_the_same_actual_external_port_and_world() {
    use reda::compile::geometry::Anchor;
    use reda::redstone::world::block::Facing;
    let single = Netlist {
        inputs: vec!["a".into()],
        outputs: vec!["same".into()],
        gates: vec![Gate::nor("same", &["a"])],
    };
    let aliases = Netlist {
        outputs: vec!["same".into(), "same".into()],
        ..single.clone()
    };
    let mut pins = PortPlacements::default();
    pins.pin("same", Anchor { x: 8, y: 1, z: 44 }, Facing::South);
    let actual = compile_planned(&aliases, &pins).unwrap();
    let plan = reda::compile::planner::plan_from_netlist(&aliases, &pins).unwrap();
    let original_emission =
        reda::compile::planner::emit_candidate(&plan, &aliases, actual.world.size()).unwrap();
    for (flat, &index) in original_emission.world.cells().iter().enumerate() {
        let (x, y, z) = original_emission.world.decode(flat);
        assert_eq!(
            original_emission.world.palette().get(index),
            Some(actual.world.get(x, y, z))
        );
    }
    assert_eq!(
        actual.output_occurrences(),
        &[("same".into(), (8, 1, 44)), ("same".into(), (8, 1, 44))]
    );
    assert_eq!(actual.world.get(8, 1, 44).kind, BlockKind::Air);
}

#[test]
fn frozen_six_input_nine_occurrence_default_compilation_preserves_aliases() {
    use reda::compile::{compile, lowering::lower_optimised};
    use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
    let text = "module m(input a,b,c,d,e,f, output u,z,o,v,w,x,y,q,r); assign u=a|b; assign z=1'b0; assign o=1'b1; assign v=c&d; assign w=c&d; assign x=c; assign y=e^f; assign q=~(e|f); assign r=f; endmodule";
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "source.sv",
            text,
        }],
        &CompileOptions::new("m"),
    )
    .unwrap();
    let source = lower_optimised(&artifact.netlist).unwrap();
    assert_eq!(source.inputs.len(), 6);
    assert_eq!(source.outputs.len(), 9);
    assert_eq!(source.outputs[3], source.outputs[4]);
    let compiled = compile(&source).unwrap();
    assert_eq!(compiled.output_occurrences().len(), 9);
    assert_eq!(
        compiled.output_occurrences()[3].1,
        compiled.output_occurrences()[4].1
    );
    for (output, (signal, position)) in source.outputs.iter().zip(compiled.output_occurrences()) {
        assert_eq!(output, signal);
        assert_eq!(compiled.output_positions[output], *position);
    }
}
