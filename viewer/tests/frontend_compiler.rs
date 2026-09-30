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
            "e37552944a6763942ff3d8f8b1ba5a62e3e351c51c0febc344f4c83b0e3ed2df",
            "25dc3b4313aef0861d5ae8501c6b6cd2173833f949c0670feb57cd12a5414d06",
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

/// Check the actual emitted channel world, beyond frontend netlist parity.
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
    let compiled = reda::compile::compile(&lowered).unwrap();
    assert_eq!(compiled.planner_kind(), reda::compile::PlannerKind::Legacy);
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&compiled.world)
            .as_str(),
        "700e2d4cebb096e3b79f18009237f0d7875b706f7665e4ec36ecdf7974946091"
    );
}
