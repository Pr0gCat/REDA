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
    let mut phases = 0;
    let quality = reda::compile::selection::qualify_boundary_with_phases(&n, &public, |p| {
        phases += 1;
        assert_eq!(p.expected.len(), 7);
        assert_eq!(p.driver, p.expected);
        assert_eq!(p.lamp, p.expected);
    })
    .unwrap();
    assert_eq!(phases, 512);
    assert_eq!(quality.target[0], 44);
    assert_eq!(quality.target[1], 8823);
    assert_eq!(quality.setup[1], 8786);
    let selected = reda::compile::compile(&n).unwrap();
    assert!(
        selected.physical_rewrite().is_some(),
        "default selects the qualified structural refresh correction"
    );
    let improved = reda::compile::selection::qualify_boundary(&n, &selected).unwrap();
    assert!(improved.dominates(&quality));
    assert!(improved.target[1] <= 8671, "retained target settle cap");
    assert!(improved.setup[1] <= 8636, "retained setup settle cap");
    let zero = reda::compile::selection::compile_selected_with_refresh_budget(&n, 0).unwrap();
    assert_eq!(zero.world.size(), public.world.size());
    for flat in 0..zero.world.cells().len() {
        let (x, y, z) = zero.world.decode(flat);
        assert_eq!(zero.world.get(x, y, z), public.world.get(x, y, z));
    }
    assert_eq!(
        reda::compile::selection::qualify_boundary(&n, &zero),
        Some(quality)
    );

    // Draft reduction preserves identity and updates all shared-prefix branches.
    use reda::compile::fragment_synth::refresh::{removable_flat_refreshes, remove_flat_refresh};
    let before = public.physical_rewrite().unwrap().implemented_candidate();
    let sites = removable_flat_refreshes(before);
    assert!(!sites.is_empty());
    assert!(sites.windows(2).all(|pair| pair[0] < pair[1]));
    let (route_id, at) = sites[0];
    let mut reduced = before.clone();
    assert!(remove_flat_refresh(&mut reduced, route_id, at));
    assert_eq!(reduced.instances, before.instances);
    assert_eq!(reduced.placements, before.placements);
    assert_eq!(reduced.boundaries, before.boundaries);
    assert_eq!(reduced.connections, before.connections);
    assert_eq!(reduced.observations, before.observations);
    for (id, route) in &before.routes {
        let current = &reduced.routes[id];
        assert_eq!(current.floors, route.floors);
        assert_eq!(current.branches.len(), route.branches.len());
        for (old, new) in route.branches.iter().zip(&current.branches) {
            let mut expected = old.clone();
            if *id == route_id && old.path.contains(&at) {
                expected.terminal.repeaters -= 1;
            }
            assert_eq!(*new, expected);
        }
        for (old, new) in route.cells.iter().zip(&current.cells) {
            if *id == route_id && old.at == at {
                assert_eq!(new.at, old.at);
                assert_eq!(
                    new.state.kind,
                    reda::redstone::world::block::BlockKind::RedstoneWire
                );
            } else {
                assert_eq!(new, old);
            }
        }
    }
    let reduced_routes = reduced.routes.clone();
    assert!(!remove_flat_refresh(&mut reduced, route_id, at));
    assert_eq!(reduced.routes, reduced_routes);
    let mut observed = before.clone();
    observed.observations.values_mut().next().unwrap().site.at = at;
    assert!(!remove_flat_refresh(&mut observed, route_id, at));
    assert_eq!(observed.routes, before.routes);

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

#[test]
fn qualification_checks_complete_boundaries_and_constant_outputs() {
    use reda::compile::{selection::qualify_boundary_with_phases, Gate, Netlist};
    for width in [1, 3, 6] {
        let source = Netlist {
            inputs: (0..width).map(|i| format!("i{i}")).collect(),
            outputs: ["negative", "positive", "one", "zero"]
                .map(String::from)
                .to_vec(),
            gates: vec![
                Gate::nor("negative", &["i0"]),
                Gate::nor("positive", &["negative"]),
                Gate::nor("one", &[]),
                Gate::nor("zero", &["one"]),
            ],
        };
        let raw = compile_without_rewrite(&source).unwrap();
        assert!(qualify_boundary_with_phases(&source, &raw, |_| panic!(
            "unsealed baseline must not qualify publicly"
        ))
        .is_none());
        let candidate = reda::compile::selection::compile_flat_refresh_candidate(&source).unwrap();
        let mut phases = 0;
        let quality = qualify_boundary_with_phases(&source, &candidate, |p| {
            let states = 1 << width;
            assert_eq!(
                (p.from, p.to, p.phase),
                (phases / (2 * states), (phases / 2) % states, phases % 2)
            );
            assert_eq!(p.address, if p.phase == 0 { p.from } else { p.to });
            let bit = p.address & 1 != 0;
            assert_eq!(p.expected, [!bit, bit, true, false]);
            assert_eq!(p.driver, p.expected);
            assert_eq!(p.lamp, p.expected);
            phases += 1;
        });
        assert!(quality.is_some());
        assert_eq!(phases, 2 * (1 << width) * (1 << width));
    }
}
