use reda::compile::fragment_synth::{
    benchmark::canonical_world_fingerprint, compile_fragment_synth,
    SynthesisBudget, SynthesisInput,
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};

#[test]
fn recursive_rom32x16_retains_the_qualified_world_and_consumer_contract() {
    let source = compile_systemverilog(
        &[SourceInput {
            name: "rom32",
            text: include_str!("fixtures/rom32_matrix.sv"),
        }],
        &CompileOptions::new("rom32"),
    ).unwrap().netlist;
    let mut fingerprint = None;
    for _ in 0..2 {
        let result = compile_fragment_synth(
            SynthesisInput { lowered: &source, source_provenance: None, pins: None },
            SynthesisBudget::Evaluations(0),
        ).unwrap();
        assert_eq!(result.evaluations_used, 0);
        let rewrite = result.compiled.physical_rewrite().expect("qualified wide root");
        assert_eq!(rewrite.source_input_positions().len(), 5);
        assert_eq!(rewrite.source_output_positions().len(), 16);
        assert_eq!(rewrite.source_output_driver_positions().len(), 16);
        assert_eq!(rewrite.implemented_observations().gate_output_positions.len(),
            rewrite.implemented().gates.len());
        let actual = canonical_world_fingerprint(&result.compiled.world);
        assert_eq!(actual.as_str(),
            "611cb93a85b558186988030a018b175f1b920c536330308cbbc1b22f56075fa8");
        if let Some(previous) = &fingerprint { assert_eq!(&actual, previous); }
        fingerprint = Some(actual);
        result.compiled.validate_rewrite_view(&source).unwrap();
        reda::compile::evaluation::CircuitEvaluation::from_synthesis(&source, &result).unwrap();
        assert!(reda::compile::selection::qualify_boundary(&source, &result.compiled).is_some());
        let observations = reda::timing::watch_all_nets(&result.compiled);
        let unique: std::collections::BTreeSet<_> = observations.iter().map(|(_, name)| name).collect();
        assert_eq!(unique.len(), observations.len());
    }
}
