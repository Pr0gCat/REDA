use reda::compile::{Gate, Netlist};
use reda::compile::fragment_synth::{compile_fragment_synth, SynthesisInput, SynthesisBudget};

#[test]
fn recursive_five_input_constants_and_aliases_preserve_every_public_occurrence() {
    for outputs in [
        vec!["low"; 16],
        vec!["high"; 16],
        vec!["low", "high", "input0", "inverse", "input4", "input0", "low", "high",
             "inverse", "input4", "input1", "input2", "input3", "low", "high", "inverse"],
    ] {
        let source = Netlist {
            inputs: (0..5).map(|bit| format!("input{bit}")).collect(),
            outputs: outputs.iter().map(|name| name.to_string()).collect(),
            gates: vec![Gate::nor("high", &[]), Gate::nor("low", &["high"]),
                        Gate::nor("inverse", &["input0"])],
        };
        let result = compile_fragment_synth(SynthesisInput {
            lowered: &source, source_provenance: None, pins: None,
        }, SynthesisBudget::Evaluations(0)).unwrap();
        assert_eq!(result.evaluations_used, 0);
        reda::compile::evaluation::CircuitEvaluation::from_synthesis(&source, &result).unwrap();
        let rewrite = result.compiled.physical_rewrite().expect("aliases require complete retained rewrite boundaries");
        assert_eq!(rewrite.source_input_positions().len(), 5);
        assert_eq!(rewrite.source_output_positions().len(), 16);
        assert_eq!(rewrite.source_output_driver_positions().len(), 16);
        assert_eq!(rewrite.implemented_observations().gate_output_positions.len(), rewrite.implemented().gates.len());
        let compiled = result.compiled;
        compiled.validate_rewrite_view(&source).unwrap();
        assert!(reda::compile::selection::qualify_boundary(&source, &compiled).is_some());
    }
}
