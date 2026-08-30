//! Headless acceptance test for the Verilog-derived circuits reaching the
//! wasm-facing API -- the same "no browser, no wasm runtime, just `cargo
//! test`" arrangement as `and4_truth_table.rs`, and the same reasons for it.
//!
//! # What is actually being proved here
//!
//! These circuits cannot be synthesized in a `wasm32` build: no Python, no
//! Yosys, no subprocess. They reach this crate as `reda::circuits::verilog`'s
//! *baked* netlists instead (see `VerilogCircuit::baked_netlist`). That makes
//! two things worth checking that no existing test covers:
//!
//! 1. **The baked netlist is a working circuit, not just a well-formed
//!    file.** `reda`'s own `tests/verilog_frontend.rs` proves the *freshly
//!    synthesized* netlist against the truth table, and separately proves the
//!    baked file matches a fresh synthesis -- but both of those need Yosys.
//!    This drives the baked one through `Session`, all 16 input combinations,
//!    against the same truth table, on any machine.
//! 2. **The circuit the viewer loads is the one the size ladder names.**
//!    `verilog:seven_segment` is 31 logical cells, lowered through the
//!    official global-polarity path into 47 physical gates and 10088 blocks;
//!    if any of those moved, the viewer would be showing a different circuit
//!    from the project’s official artefacts.
//!
//! Like `and4_truth_table.rs`, this never calls `Session::pinout` or
//! `Session::legend` (both return a `JsValue`, which aborts outside a real
//! wasm host), and learns output coordinates by compiling the same netlist
//! directly instead.

use reda::circuits::seven_segment::TRUTH_TABLE;
use reda::circuits::verilog;
use reda::compile::lowering::lower_optimised;
use reda::compile::planner::{Anchor, PortPin, PortRole};
use reda::compile::{compile, input_terminal_reader, output_terminal_handover};
use reda::formats::litematic;
use reda::redstone::simulator::position::Position;
use reda::redstone::world::block::{BlockKind, Facing};
use reda_viewer::{list_circuits, Axis, Session};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// Bytes per cell in `Session::geometry`'s packed output -- the layout that
/// method documents (`[x, y, z, kind, facing, face, delay]`, coordinates as
/// little-endian `u16`). Restated here rather than exported, same as
/// `and4_truth_table.rs` restates `slice`'s layout: the point is to check the
/// documented contract, not the implementation against itself.
const GEOMETRY_BYTES_PER_CELL: usize = 10;

/// One cell's signal strength out of a `slice` result, using the row-major
/// layout `Session::slice` documents for `Axis::Z`.
fn strength_at(slice_bytes: &[u8], size_y: i32, coord: (i32, i32, i32)) -> u8 {
    let (x, y, _z) = coord;
    slice_bytes[2 * ((x * size_y + y) as usize) + 1]
}

/// Set every lever to the bits of `value` (MSB first over `inputs`), settle,
/// and read each output lamp back through `slice`.
fn evaluate(session: &mut Session, inputs: &[&str], value: u32, outputs: &[(i32, i32, i32)]) -> Vec<bool> {
    for (index, name) in inputs.iter().enumerate() {
        let bit = (value >> (inputs.len() - 1 - index)) & 1 == 1;
        session.set_lever(name, bit).expect("every input name comes from the netlist itself");
    }
    session.run_until_stable().expect("a synthesised circuit must settle");

    let size = session.size();
    outputs
        .iter()
        .map(|&position| {
            let bytes = session.slice(Axis::Z, position.2).expect("an output lamp is inside the world");
            strength_at(&bytes, size[1], position) > 0
        })
        .collect()
}

/// Every output lamp's coordinate, learned by compiling the same baked
/// netlist directly -- the same "two front doors onto one compiled circuit"
/// cross-check `and4_truth_table.rs` makes.
fn output_positions(circuit_name: &str) -> Vec<(i32, i32, i32)> {
    let circuit = verilog::find(circuit_name).expect("catalog entry must exist");
    let (netlist, labels) = circuit.baked_netlist();
    let netlist = lower_optimised(&netlist).expect("a baked netlist lowers");
    let compiled = compile(&netlist).expect("a baked netlist compiles");
    labels
        .iter()
        .map(|(_port, signal)| *compiled.output_positions.get(signal).expect("compile places every output"))
        .collect()
}

fn json_coordinate(value: &Value, field: &str, port: &str) -> [i32; 3] {
    let values = value
        .as_array()
        .unwrap_or_else(|| panic!("`{port}`'s `{field}` must be a coordinate array, got {value}"));
    assert_eq!(
        values.len(),
        3,
        "`{port}`'s `{field}` must have three coordinates"
    );
    let mut coordinate = [0; 3];
    for (index, value) in values.iter().enumerate() {
        let value = value
            .as_i64()
            .unwrap_or_else(|| panic!("`{port}`'s `{field}` coordinate #{index} is not an integer"));
        coordinate[index] = i32::try_from(value).unwrap_or_else(|_| {
            panic!("`{port}`'s `{field}` coordinate #{index} is outside the i32 world range")
        });
    }
    coordinate
}

fn json_facing(value: &Value, field: &str, port: &str) -> Facing {
    match value.as_str() {
        Some("north") => Facing::North,
        Some("south") => Facing::South,
        Some("east") => Facing::East,
        Some("west") => Facing::West,
        other => panic!("`{port}`'s `{field}` must be a horizontal direction, got {other:?}"),
    }
}

/// The authored pins file is the source input; the pinout is generated output.
/// Compare both files port-by-port so a stale sidecar cannot pass merely by
/// remaining self-consistent with the litematic that was generated beside it.
#[test]
fn checked_in_grown_decoder_pinout_matches_its_literal_pins() {
    let baked = Path::new(env!("CARGO_MANIFEST_DIR")).join("baked");
    let pins: Value = serde_json::from_str(
        &std::fs::read_to_string(baked.join("verilog_seven_segment.grown.pins.json"))
            .expect("the literal grown decoder pins are checked in"),
    )
    .expect("the literal grown decoder pins are JSON");
    let pinout: Value = serde_json::from_str(
        &std::fs::read_to_string(baked.join("verilog_seven_segment.grown.pinout.json"))
            .expect("the generated grown decoder pinout is checked in"),
    )
    .expect("the generated grown decoder pinout is JSON");

    let expected_inputs = [
        ("d3", [76, 1, 120], "north", [76, 1, 119]),
        ("d2", [88, 1, 120], "north", [88, 1, 119]),
        ("d1", [100, 1, 120], "north", [100, 1, 119]),
        ("d0", [112, 1, 120], "north", [112, 1, 119]),
    ];
    let expected_outputs = [
        ("a", [76, 1, 24], "north", [76, 1, 25]),
        ("b", [84, 1, 32], "east", [83, 1, 32]),
        ("c", [84, 1, 48], "east", [83, 1, 48]),
        ("d", [76, 1, 56], "south", [76, 1, 55]),
        ("e", [68, 1, 48], "west", [69, 1, 48]),
        ("f", [68, 1, 32], "west", [69, 1, 32]),
        ("g", [76, 1, 40], "west", [77, 1, 40]),
    ];

    for (role_name, role, expected) in [
        ("inputs", PortRole::Input, expected_inputs.as_slice()),
        ("outputs", PortRole::Output, expected_outputs.as_slice()),
    ] {
        let authored = pins[role_name]
            .as_object()
            .unwrap_or_else(|| panic!("literal pins `{role_name}` must be an object"));
        let generated = pinout[role_name]
            .as_object()
            .unwrap_or_else(|| panic!("generated pinout `{role_name}` must be an object"));
        let expected_keys: BTreeSet<&str> = expected.iter().map(|(name, ..)| *name).collect();
        assert_eq!(
            authored.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            expected_keys,
            "literal `{role_name}` keys changed"
        );
        assert_eq!(
            generated
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            expected_keys,
            "generated `{role_name}` keys changed"
        );

        for &(name, literal_at, literal_toward, literal_handover) in expected {
            let authored_port = &authored[name];
            let generated_port = &generated[name];
            let at = json_coordinate(&authored_port["at"], "at", name);
            let toward = authored_port["toward"]
                .as_str()
                .unwrap_or_else(|| panic!("literal `{name}` toward must be a string"));

            assert_eq!(at, literal_at, "literal `{name}` coordinate changed");
            assert_eq!(toward, literal_toward, "literal `{name}` direction changed");
            assert_eq!(
                json_coordinate(&generated_port["at"], "at", name),
                at,
                "generated `{name}` coordinate drifted from its source pin"
            );
            assert_eq!(
                generated_port["toward"].as_str(),
                Some(toward),
                "generated `{name}` direction drifted from its source pin"
            );

            let handover = PortPin {
                at: Anchor {
                    x: at[0],
                    y: at[1],
                    z: at[2],
                },
                toward: json_facing(&authored_port["toward"], "toward", name),
            }
            .handover(role);
            let derived_handover = [handover.x, handover.y, handover.z];
            assert_eq!(
                derived_handover, literal_handover,
                "literal `{name}` no longer has its reviewed role-specific handover"
            );
            assert_eq!(
                json_coordinate(&generated_port["handover"], "handover", name),
                derived_handover,
                "generated `{name}` reports a handover inconsistent with PortPin"
            );
        }
    }
}

/// The generated files are shipping artifacts, not test fixtures generated on
/// demand. This reads those exact checked-in bytes and catches either half of
/// the artifact drifting: the sidecar must retain the public glyph labels and
/// literal geometry, while the litematic must contain the matching terminal
/// handovers and leave every caller-owned `at` cell empty.
#[test]
fn checked_in_grown_decoder_is_the_pinned_glyph() {
    let baked = Path::new(env!("CARGO_MANIFEST_DIR")).join("baked");
    let pinout_text = std::fs::read_to_string(baked.join("verilog_seven_segment.grown.pinout.json"))
        .expect("the grown decoder pinout is checked in");
    let pinout: Value =
        serde_json::from_str(&pinout_text).expect("the grown decoder pinout is JSON");
    let inputs = pinout["inputs"]
        .as_object()
        .expect("pinout inputs are an object");
    let outputs = pinout["outputs"]
        .as_object()
        .expect("pinout outputs are an object");

    let input_keys: BTreeSet<&str> = inputs.keys().map(String::as_str).collect();
    assert_eq!(input_keys, BTreeSet::from(["d0", "d1", "d2", "d3"]));
    let output_keys: BTreeSet<&str> = outputs.keys().map(String::as_str).collect();
    assert_eq!(
        output_keys,
        BTreeSet::from(["a", "b", "c", "d", "e", "f", "g"])
    );
    assert_eq!(
        inputs.len() + outputs.len(),
        11,
        "the decoder ships eleven pinned ports"
    );

    let expected_inputs = [
        ("d3", [76, 1, 120], "north", [76, 1, 119]),
        ("d2", [88, 1, 120], "north", [88, 1, 119]),
        ("d1", [100, 1, 120], "north", [100, 1, 119]),
        ("d0", [112, 1, 120], "north", [112, 1, 119]),
    ];
    let expected_outputs = [
        ("a", [76, 1, 24], "north", [76, 1, 25]),
        ("b", [84, 1, 32], "east", [83, 1, 32]),
        ("c", [84, 1, 48], "east", [83, 1, 48]),
        ("d", [76, 1, 56], "south", [76, 1, 55]),
        ("e", [68, 1, 48], "west", [69, 1, 48]),
        ("f", [68, 1, 32], "west", [69, 1, 32]),
        ("g", [76, 1, 40], "west", [77, 1, 40]),
    ];

    let world = litematic::load(&baked.join("verilog_seven_segment.grown.litematic"))
        .expect("the checked-in grown decoder litematic loads");
    for (is_input, expected) in [
        (true, expected_inputs.as_slice()),
        (false, expected_outputs.as_slice()),
    ] {
        let ports = if is_input { inputs } else { outputs };
        for &(name, at, toward, handover) in expected {
            let entry = ports[name]
                .as_object()
                .unwrap_or_else(|| {
                    panic!(
                        "`{name}` must be a structured pinned entry, got {}",
                        ports[name]
                    )
                });
            assert_eq!(
                entry.len(),
                3,
                "`{name}` must report only at, toward and handover"
            );
            assert_eq!(json_coordinate(&entry["at"], "at", name), at, "`{name}` moved");
            assert_eq!(
                entry["toward"].as_str(),
                Some(toward),
                "`{name}` changed direction"
            );
            assert_eq!(
                json_coordinate(&entry["handover"], "handover", name),
                handover,
                "`{name}` reported the wrong handover"
            );

            let recorded = Position::new(at[0], at[1], at[2]);
            assert_eq!(
                world.get(recorded.x, recorded.y, recorded.z).kind,
                BlockKind::Air,
                "`{name}`'s caller cell must ship empty"
            );
            let found = if is_input {
                input_terminal_reader(&world, recorded)
            } else {
                output_terminal_handover(&world, recorded)
            };
            assert_eq!(
                found,
                Some(Position::new(handover[0], handover[1], handover[2])),
                "`{name}`'s reported handover must be the shipped terminal"
            );
        }
    }
}

/// The hand-written size ladder first, then the Verilog catalog verbatim --
/// `verilog:` prefix included, because `verilog:seven_segment` and
/// `seven_segment` compute the same function out of entirely different gates
/// and a viewer showing one of them has to say which.
///
/// # And nothing else: the `planned:` block is gone
///
/// This test used to require those four entries. It now forbids them, and the
/// reason is that `compile` became a hybrid -- it tries relaxation placement
/// and falls back to the emitter -- which made every one of them either a
/// duplicate or a trap. `planned:and4` and `planned:full_adder` became
/// byte-identical to the plain names; `planned:segment_a` and
/// `planned:seven_segment` freeze the page for tens of seconds on a
/// synchronous wasm call and then fail, because the planner places those two
/// and cannot route them.
///
/// Asserted as an absence rather than deleted, so that putting them back is a
/// decision somebody makes on purpose. `PLANNED_PREFIX` and its branch in
/// `Session::new` are still there for the day the two paths differ again on a
/// circuit that routes.
#[test]
fn list_circuits_reports_both_catalogs_and_nothing_else() {
    let names = list_circuits();
    let verilog_names: Vec<String> = verilog::CIRCUITS.iter().map(|c| c.name.to_string()).collect();

    assert!(
        !names.iter().any(|name| name.starts_with("planned:")),
        "the planned entries are duplicates or stalls now; got {names:?}"
    );
    assert_eq!(
        names[names.len() - verilog_names.len()..],
        verilog_names[..],
        "the Verilog catalog must appear last, in its own order; got {names:?}"
    );
    assert!(names.iter().any(|n| n == "seven_segment"), "the hand-written decoder must still be listed");
    assert!(names.iter().any(|n| n == "verilog:seven_segment"), "the synthesised decoder must be listed");
}

#[test]
fn the_verilog_and4_session_matches_its_truth_table_through_the_wasm_api() {
    let outputs = output_positions("verilog:and4");
    let mut session = Session::new("verilog:and4").expect("the baked and4 netlist builds a session");

    for value in 0..16u32 {
        let expected = value == 0b1111;
        let observed = evaluate(&mut session, &["a", "b", "c", "d"], value, &outputs);
        assert_eq!(
            observed,
            vec![expected],
            "verilog:and4 with inputs {value:04b} must output {expected}"
        );
    }
}

#[test]
fn the_verilog_seven_segment_session_matches_its_truth_table_through_the_wasm_api() {
    let outputs = output_positions("verilog:seven_segment");
    assert_eq!(outputs.len(), 7, "a seven-segment decoder has seven outputs");
    let mut session =
        Session::new("verilog:seven_segment").expect("the baked seven_segment netlist builds a session");

    for value in 0..16u32 {
        let expected: Vec<bool> = if (value as usize) < TRUTH_TABLE.len() {
            TRUTH_TABLE[value as usize].iter().map(|&bit| bit == 1).collect()
        } else {
            vec![false; 7]
        };
        let observed = evaluate(&mut session, &["d3", "d2", "d1", "d0"], value, &outputs);
        assert_eq!(observed, expected, "verilog:seven_segment on digit {value}");
    }
}

/// The synthesised decoder the viewer loads is the same circuit the rest of
/// this project quotes, at both of its levels: 31 gate-level cells as Yosys
/// left them, 47 torches and merges once `compile::lowering` has assigned
/// global polarities, 10088 blocks once the compiler has. `geometry()`'s length is that block
/// count as the viewer itself sees it: one entry per non-air cell.
#[test]
fn the_verilog_seven_segment_is_the_size_the_ladder_says_it_is() {
    let (netlist, _) = verilog::find("verilog:seven_segment").expect("catalog entry").baked_netlist();
    assert_eq!(netlist.gates.len(), 31, "gate-level cell count has moved");
    assert_eq!(
        netlist.gates.iter().filter(|gate| gate.kind.is_realisable()).count(),
        9,
        "only 9 of the decoder's 31 cells are things redstone builds directly"
    );

    let lowered = lower_optimised(&netlist).expect("the decoder lowers");
    assert_eq!(lowered.gates.len(), 47, "lowered gate count has moved");

    let session = Session::new("verilog:seven_segment").expect("session builds");
    let cells = session.geometry().len() / GEOMETRY_BYTES_PER_CELL;
    assert_eq!(cells, 10088, "the synthesised decoder's block count has moved");
    assert_eq!(session.geometry().len() % GEOMETRY_BYTES_PER_CELL, 0);
    assert_eq!(session.strengths().len(), cells, "one strength byte per geometry entry");
}
