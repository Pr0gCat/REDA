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
    assert!(!build_geometry.contains("pinMarkerGroup3d"));

    let apply_clip = page
        .split_once("function applyClip()")
        .unwrap()
        .1
        .split_once("const throttledClipInput")
        .unwrap()
        .0;
    assert!(apply_clip.contains("pinMarkerGroup3d"));
}

#[test]
fn three_d_pin_marker_is_a_visible_edge_frame() {
    let page = include_str!("../index.html");

    assert!(page.contains("new THREE.EdgesGeometry("));
    assert!(page.contains("new THREE.LineSegments("));
    assert!(page.contains("depthTest: false"));
    assert!(page.contains("depthWrite: false"));
    assert!(page.contains("marker.renderOrder = 1"));
}

#[test]
fn input_and_output_pin_colours_are_distinct_in_both_views() {
    let page = include_str!("../index.html");

    assert!(page.contains("const INPUT_PIN_COLOUR = '#be78ff';"));
    assert!(page.contains("const OUTPUT_PIN_COLOUR = '#42d7ff';"));
    assert!(page.contains("i < pinout.inputs.length ? inputPinMarkerMaterial3d : outputPinMarkerMaterial3d"));
    assert!(page.contains("[pinout.inputs, INPUT_PIN_COLOUR]"));
    assert!(page.contains("[pinout.outputs, OUTPUT_PIN_COLOUR]"));
}

#[test]
fn viewer_marks_explicit_pin_toward_in_the_ui_and_both_views() {
    let page = include_str!("../index.html");

    assert!(page.contains("function pinDirectionWorld(toward)"));
    assert!(page.contains("new THREE.ArrowHelper("));
    assert!(page.contains("function draw2DPinDirection(pin, cellSize)"));
    assert!(page.contains("draw2DPinDirection(pin, cellSize);"));
    assert!(page.contains("pin.toward.toUpperCase()"));
}
