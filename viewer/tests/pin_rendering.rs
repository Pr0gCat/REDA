#[test]
fn viewer_wires_pin_markers_and_lamp_tinting() {
    let page = include_str!("../index.html");

    assert!(page.contains("const LAMP_ID = 11;"));
    assert!(page.contains(
        "kindId === REDSTONE_WIRE_ID || kindId === LAMP_ID"
    ));
    assert!(page.contains("function build3DPinMarkers()"));
    assert!(page.contains("function draw2DPinMarkers(cellSize)"));
    assert!(page.contains("Math.max(1, cellSize - ctx.lineWidth)"));
    assert!(page.contains("draw2DPinMarkers(cellSize);\n  if (cellSize < 6) return;"));

    let build_geometry = page
        .split_once("function build3DGeometry")
        .unwrap()
        .1
        .split_once("function update3DStrengths")
        .unwrap()
        .0;
    assert!(!build_geometry.contains("pinMarkerMesh3d"));

    let apply_clip = page
        .split_once("function applyClip()")
        .unwrap()
        .1
        .split_once("const throttledClipInput")
        .unwrap()
        .0;
    assert!(apply_clip.contains("pinMarkerMesh3d"));
}
