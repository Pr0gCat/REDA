use reda::compile::{
    compile_without_rewrite,
    fragment_synth::{
        legacy_adapter::LegacyCandidateAdapter, refresh::flatten_refreshes,
        rewrite::realise_physical_rewrite,
    },
    lowering::lower_optimised,
    topology::Library,
};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
#[test]
fn staircase_refresh_relocation_makes_the_actual_decoder_manifest_certifiable() {
    let source = include_str!("fixtures/seven_segment.sv");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "decoder.sv",
            text: source,
        }],
        &CompileOptions::new("seven_segment"),
    )
    .unwrap();
    let n = lower_optimised(&artifact.netlist).unwrap();
    let compiled = compile_without_rewrite(&n).unwrap();
    let mut adapted = LegacyCandidateAdapter::adapt(&n, &compiled).unwrap();
    let before = adapted.candidate.clone();
    assert!(realise_physical_rewrite(
        &n,
        &n,
        &before,
        &Library::default_library(),
        &n.inputs,
        &n.outputs,
        1_000_000
    )
    .is_err());
    let original_branches: Vec<_> = before
        .routes
        .values()
        .flat_map(|r| &r.branches)
        .cloned()
        .collect();
    assert_eq!(flatten_refreshes(&mut adapted.candidate), 1);
    assert_eq!(
        flatten_refreshes(&mut adapted.candidate),
        0,
        "normalization is idempotent"
    );
    assert_eq!(adapted.candidate.observations, before.observations);
    let current_branches: Vec<_> = adapted
        .candidate
        .routes
        .values()
        .flat_map(|r| &r.branches)
        .cloned()
        .collect();
    assert_eq!(
        original_branches, current_branches,
        "paths and terminal refresh counts are preserved"
    );
    let certified = realise_physical_rewrite(
        &n,
        &n,
        &adapted.candidate,
        &Library::default_library(),
        &n.inputs,
        &n.outputs,
        1_000_000,
    )
    .unwrap();
    assert_eq!(certified.source(), &n);
    assert_eq!(certified.implemented(), &n);
    let public = reda::compile::selection::compile_flat_refresh_candidate(&n).unwrap();
    assert!(public.physical_rewrite().is_some());
    assert_eq!(public.world.size(), compiled.world.size());
    public.validate_rewrite_view(&n).unwrap();
    let graph =
        reda::compile::primitive_graph::from_certified_rewrite(public.physical_rewrite().unwrap());
    reda::compile::equivalence::verify_expansion_matches_compiled(&n, &graph, &public).unwrap();
    assert!(
        reda::compile::world_partition::partition_world(&n, &graph, &public)
            .unwrap()
            .unexplained
            .is_empty()
    );
}
