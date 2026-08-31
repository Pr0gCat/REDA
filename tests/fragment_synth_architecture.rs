use reda::circuits::and4::build_and4_netlist;
use reda::compile::{
    compile_fragment_synth, PlannerKind, SynthesisBudget, SynthesisInput, SynthesisResult,
};
use reda::redstone::simulator::Simulator;

const MAX_TICKS: u64 = 500;

fn assert_and4_truth_table(result: &SynthesisResult, output: &str) {
    let mut simulator = Simulator::new(result.compiled.world.clone());
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("fragment seed must settle initially");
    let inputs =
        ["a", "b", "c", "d"].map(|name| *result.compiled.input_positions.get(name).unwrap());
    let output = *result.compiled.output_positions.get(output).unwrap();

    for combination in 0u8..16 {
        let bits = [
            combination & 8 != 0,
            combination & 4 != 0,
            combination & 2 != 0,
            combination & 1 != 0,
        ];
        for (position, value) in inputs.iter().zip(bits) {
            let mut state = simulator
                .world()
                .get(position.0, position.1, position.2)
                .clone();
            state.lit = value;
            simulator
                .world_mut()
                .set(position.0, position.1, position.2, state);
        }
        simulator
            .run_until_stable(MAX_TICKS)
            .expect("fragment seed must settle after a vector change");
        assert_eq!(
            simulator.world().get(output.0, output.1, output.2).lit,
            bits.iter().all(|value| *value),
            "wrong and4 value for {bits:?}"
        );
    }
}

#[test]
fn the_public_zero_budget_api_is_independent_deterministic_and_truthful() {
    let (netlist, output) = build_and4_netlist();
    let input = || SynthesisInput {
        lowered: &netlist,
        source_provenance: None,
        pins: None,
    };
    let first = compile_fragment_synth(input(), SynthesisBudget::Evaluations(0))
        .expect("zero budget must still return the certified independent seed");
    let repeated = compile_fragment_synth(input(), SynthesisBudget::Evaluations(0))
        .expect("the same zero-budget case must repeat");

    assert_eq!(first.compiled.planner_kind(), PlannerKind::FragmentSynth);
    assert_eq!(repeated.compiled.planner_kind(), PlannerKind::FragmentSynth);
    assert_eq!(first.evaluations_used, 0);
    assert_eq!(repeated.evaluations_used, 0);
    assert!(first.trace.is_empty());
    assert!(repeated.trace.is_empty());
    assert_eq!(first.case_fingerprint, repeated.case_fingerprint);
    assert_eq!(
        first.metrics.candidate_fingerprint,
        repeated.metrics.candidate_fingerprint
    );
    assert_eq!(first.compiled.world.size(), repeated.compiled.world.size());
    assert_eq!(
        first.compiled.world.cells(),
        repeated.compiled.world.cells()
    );
    assert_eq!(
        first.compiled.world.palette().entries(),
        repeated.compiled.world.palette().entries()
    );

    assert_and4_truth_table(&first, &output);
    assert_and4_truth_table(&repeated, &output);
}
