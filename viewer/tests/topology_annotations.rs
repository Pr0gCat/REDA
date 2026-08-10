#[test]
fn topology_draws_only_real_primitives_and_edges() {
    let page = include_str!("../index.html");

    for synthetic_annotation in [
        "computeMergeJunctions",
        "drawMergeGlyphs",
        "strokeMergeSpoke",
        "activeMerges",
    ] {
        assert!(
            !page.contains(synthetic_annotation),
            "topology must not render synthetic merge annotation `{synthetic_annotation}`"
        );
    }
}
