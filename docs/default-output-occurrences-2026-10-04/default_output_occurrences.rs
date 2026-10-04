use reda::compile::{
    selection::{compile_selected_with_refresh_budget, qualify_boundary_with_phases},
    Gate, Netlist,
};
use std::collections::BTreeSet;

#[test]
fn default_boundary_preserves_primary_repeated_and_constant_occurrences_at_zero_and_positive_budget(
) {
    for inputs in [4, 5] {
        let source = Netlist {
            inputs: (0..inputs).map(|bit| format!("input{bit}")).collect(),
            outputs: [
                "low", "high", "input0", "inverse", "input0", "low", "high", "inverse",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            gates: vec![
                Gate::nor("high", &[]),
                Gate::nor("low", &["high"]),
                Gate::nor("inverse", &["input0"]),
            ],
        };
        let mut fingerprints = Vec::new();
        for budget in [0, 256] {
            let compiled = compile_selected_with_refresh_budget(&source, budget).unwrap();
            let repeat = compile_selected_with_refresh_budget(&source, budget).unwrap();
            compiled.validate_rewrite_view(&source).unwrap();
            let rewrite = compiled
                .physical_rewrite()
                .expect("public occurrences require retained certified boundaries");
            assert_eq!(rewrite.source_input_positions().len(), inputs);
            assert_eq!(
                rewrite.source_output_positions().len(),
                source.outputs.len()
            );
            assert_eq!(
                rewrite.source_output_driver_positions().len(),
                source.outputs.len()
            );
            assert_eq!(
                rewrite
                    .source_output_positions()
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len(),
                source.outputs.len()
            );
            assert_eq!(
                rewrite
                    .implemented_observations()
                    .gate_output_positions
                    .len(),
                rewrite.implemented().gates.len()
            );
            let mut phases = 0;
            let quality = qualify_boundary_with_phases(&source, &compiled, |phase| {
                assert_eq!(phase.expected.len(), source.outputs.len());
                assert_eq!(phase.driver, phase.expected);
                assert_eq!(phase.lamp, phase.expected);
                assert_eq!(
                    phase.expected,
                    [
                        false,
                        true,
                        phase.address & 1 != 0,
                        phase.address & 1 == 0,
                        phase.address & 1 != 0,
                        false,
                        true,
                        phase.address & 1 == 0
                    ]
                );
                phases += 1;
            })
            .expect("every implemented gate and public output must qualify in continuous history");
            assert_eq!(phases, 2 * (1 << inputs) * (1 << inputs));
            assert_eq!(quality.setup[2], 0);
            assert_eq!(quality.target[2], 0);
            let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                &compiled.world,
            );
            assert_eq!(
                fingerprint,
                reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                    &repeat.world
                )
            );
            fingerprints.push(fingerprint);
        }
        assert_eq!(
            fingerprints[0], fingerprints[1],
            "budget cannot erase required boundary occurrences"
        );
    }
}
