use reda::compile::{
    compile_legacy, compile_planned, compile_without_rewrite,
    fragment_synth::torch_row::compile_player_torch_row, planner::PortPlacements, CompileError,
    CompiledCircuit, Gate, Netlist,
};
use reda::redstone::{simulator::Simulator, world::block::BlockKind};

fn planned(source: &Netlist) -> Result<CompiledCircuit, CompileError> {
    compile_planned(source, &PortPlacements::default())
}

#[test]
fn raw_occurrences_follow_source_order_instead_of_map_order() {
    let source = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs: vec!["second".into(), "first".into()],
        gates: vec![Gate::nor("first", &["a"]), Gate::nor("second", &["b"])],
    };
    for compiler in [compile_without_rewrite, planned, compile_legacy] {
        let actual = compiler(&source).unwrap();
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
