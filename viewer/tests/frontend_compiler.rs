//! Run the same compiler fixtures natively and in Node/WASM. Expected hashes
//! are measured on the native frontend, including the provenance sidecar.
use reda::compile::metrics::canonical_fingerprint;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn compiler_netlists_and_source_maps_agree_across_targets() {
    for (top, source, netlist_hash, debug_hash) in [
        (
            "and4",
            include_str!("../../tests/fixtures/and4.sv"),
            "19f8ad95588dd5cbe0f1ec3a8c083b6086257b86ccd14c09dbc5fa19e21363fe",
            "e13faa51271f4875e95f42361a0cfbaf6e69561d0d9f18e99ab5b054366d7451",
        ),
        (
            "seven_segment",
            include_str!("../../tests/fixtures/seven_segment.sv"),
            "f430a376ad3940e1245848d01b0cce09ce9af2e1d28bc09c8c7dfd14a38165b8",
            "7e0669a265b13e383a86485d7b4fc3356766270b13e1fe6f4e774c85c452f7c5",
        ),
        (
            "dff",
            include_str!("../../tests/fixtures/dff.sv"),
            "511b00f12b8455ec6b4c4bd18a12777abe2e271d7ced6c58a1c2590ddd11c440",
            "48849e4402903b38214b82779dd563a225b1d7bade0925ed4d8d177150c55ac6",
        ),
        (
            "dff_enable",
            include_str!("../../tests/fixtures/dff_enable.sv"),
            "8d3ae623ef7d06caa798188cbcac092c8c8d1a00fe9837c258ff948430f7f3a2",
            "67e5fe035cf1e75208a0d150c295cf3ef146942022287aa6c6fad309e70badda",
        ),
    ] {
        let artifact = compile_systemverilog(
            &[SourceInput {
                name: "fixture.sv",
                text: source,
            }],
            &CompileOptions::new(top),
        )
        .unwrap();
        let debug = artifact.debug.to_json();
        assert_eq!(
            artifact.debug.netlist_fingerprint.as_str(),
            netlist_hash,
            "{top}: netlist drift"
        );
        assert_eq!(
            canonical_fingerprint(debug.as_bytes()).as_str(),
            debug_hash,
            "{top}: source map drift"
        );
        assert!(artifact.debug.matches(&artifact.netlist));
    }
}

/// Retain the original channel world as a control, beyond frontend parity.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn compact_channel_world_agrees_across_targets() {
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "seven_segment.sv",
            text: include_str!("../../tests/fixtures/seven_segment.sv"),
        }],
        &CompileOptions::new("seven_segment"),
    )
    .unwrap();
    let lowered = reda::compile::lowering::lower_optimised(&artifact.netlist).unwrap();
    let compiled = reda::compile::compile_without_rewrite(&lowered).unwrap();
    assert_eq!(compiled.planner_kind(), reda::compile::PlannerKind::Legacy);
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world)
            .as_str(),
        "7c6c51d831ca72e249d590fd11b7306bdddd1c1f8f76c73bac13ada0e6cb39d4"
    );
}

/// Exercise the actual default separately from the immutable channel control.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn default_seven_segment_world_and_continuous_events_agree_across_targets() {
    use std::collections::BTreeMap;

    use reda::circuits::seven_segment::{INPUT_NAMES, TRUTH_TABLE};
    use reda::frontend::evaluate::Evaluator;
    use reda::redstone::simulator::Simulator;
    use reda::redstone::world::block::BlockKind;
    use reda::redstone::simulator::position::Position;
    use reda::timing::measure_transition;

    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "seven_segment.sv",
            text: include_str!("../../tests/fixtures/seven_segment.sv"),
        }],
        &CompileOptions::new("seven_segment"),
    )
    .unwrap();
    let lowered = reda::compile::lowering::lower_optimised(&artifact.netlist).unwrap();
    assert_eq!(
        lowered
            .inputs
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        INPUT_NAMES
    );
    let compiled = reda::compile::compile(&lowered).unwrap();
    assert_eq!(
        compiled.planner_kind(),
        reda::compile::PlannerKind::FragmentSynth
    );
    compiled.validate_rewrite_view(&lowered).unwrap();
    let rewrite = compiled
        .physical_rewrite()
        .expect("default is strictly certified");
    // The existing balanced-matrix control implements the same decoder with
    // the same ordered boundary; this is not a newly measured golden hash.
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world)
            .as_str(),
        "a8b17f9c38cbc8b363d326f8dd1633162489f6dc421e0c624d296c68485f07f7"
    );
    assert_eq!(rewrite.source_output_positions().len(), 7);
    assert_eq!(rewrite.source_output_driver_positions().len(), 7);
    assert_eq!(
        compiled.gate_output_positions.len(),
        rewrite.implemented().gates.len()
    );
    let implementation = Evaluator::new(rewrite.implemented()).unwrap();
    let position = |(x, y, z)| Position::new(x, y, z);
    let drivers = rewrite.source_output_driver_positions().to_vec();
    let lamps = rewrite.source_output_positions().to_vec();
    let mut watched = compiled
        .gate_output_positions
        .iter()
        .map(|(name, &at)| (position(at), format!("implemented:{name}")))
        .collect::<Vec<_>>();
    for (kind, sites) in [("driver", &drivers), ("lamp", &lamps)] {
        watched.extend(
            sites
                .iter()
                .enumerate()
                .map(|(bit, &at)| (position(at), format!("{kind}:{bit}"))),
        );
    }
    let signal = |sim: &Simulator, (x, y, z)| {
        let state = sim.world().get(x, y, z);
        match state.kind {
            BlockKind::RedstoneWire => state.power > 0,
            BlockKind::Torch
            | BlockKind::WallTorch
            | BlockKind::Repeater
            | BlockKind::Comparator
            | BlockKind::Lever
            | BlockKind::Lamp => state.lit,
            other => panic!("observation is not a signal: {other:?}"),
        }
    };
    let mut sim = Simulator::new(compiled.world.clone());
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(watched);
    let mut worst = 0;
    let mut phases = 0;
    for from in 0..16 {
        for to in 0..16 {
            for value in [from, to] {
                let measured = measure_transition(&mut sim, 2000, |sim| {
                    for (bit, name) in INPUT_NAMES.iter().enumerate() {
                        let (x, y, z) = compiled.input_positions[*name];
                        let mut state = sim.world().get(x, y, z).clone();
                        state.lit = value & (1 << (3 - bit)) != 0;
                        sim.world_mut().set(x, y, z, state);
                    }
                })
                .unwrap();
                worst = worst.max(measured.settle_game_ticks);
                phases += 1;
                let inputs = rewrite
                    .rewrite_certificate()
                    .rewritten_inputs_in_source_order()
                    .iter()
                    .enumerate()
                    .map(|(bit, name)| (name.clone(), value & (1 << (3 - bit)) != 0))
                    .collect::<BTreeMap<_, _>>();
                let ideal = implementation.settle(&inputs).unwrap();
                for (name, &at) in &compiled.gate_output_positions {
                    assert_eq!(signal(&sim, at), ideal[name], "{from}/{to}/{value}: {name}");
                }
                let expected = TRUTH_TABLE.get(value).copied().unwrap_or([0; 7]);
                for (bit, &level) in expected.iter().enumerate() {
                    assert_eq!(signal(&sim, drivers[bit]), level != 0);
                    assert_eq!(signal(&sim, lamps[bit]), level != 0);
                }
                assert!(
                    measured.nets.values().all(|net| net.change_count() <= 1),
                    "public or implemented-gate extra transitions at {from}/{to}/{value}"
                );
            }
        }
    }
    assert_eq!(phases, 512);
    assert_eq!(worst, 22, "existing complete-history settlement control");
}
