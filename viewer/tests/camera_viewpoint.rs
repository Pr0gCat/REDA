#[test]
fn pinned_input_rebuild_preserves_3d_viewpoint() {
    let page = include_str!("../index.html");

    assert!(page.contains("function build3DGeometry(frameView = true)"));
    assert!(page.contains("if (frameView) frameCamera();"));
    assert_eq!(page.matches("build3DGeometry(true);").count(), 2);
    assert_eq!(page.matches("build3DGeometry(false);").count(), 1);
}
