//! Pins the one invariant `geometry()`'s doc comment states and no other test
//! checks: entry `i` of `geometry()` and byte `i` of `strengths()` describe
//! the very same non-air cell, in the documented ascending-YZX order.
//!
//! This is the bug the plan singled out as the one that hides: if the two
//! arrays ever drifted out of order relative to each other, a 3D view built
//! from them would still render a plausible-looking circuit -- every colour
//! would just be attached to the wrong block, and nothing on screen would
//! say so. So this test decodes `geometry()`'s packed bytes back into
//! `(x, y, z, kind)` and checks each one, plus `strengths()`'s value at the
//! same index, against an independently built `Simulator`/`World` -- not
//! against `reda_viewer`'s own (private) `signal_strength`/`block_kind_id`
//! helpers, so this is a real cross-check, not the implementation checking
//! itself. It also checks completeness (every non-air cell appears exactly
//! once) and strict ordering (ascending flat index), not just per-entry
//! correctness.
//!
//! Deliberately native (`cargo test`, no wasm/browser): `geometry()` and
//! `strengths()` return plain `Vec<u8>`, precisely so this test -- and any
//! future JS-side host -- never needs a `JsValue` to check them. See
//! `viewer/src/lib.rs`'s module doc comment for why `JsValue` cannot be
//! constructed on the host target at all.

use reda::circuits::and4::{build_and4_netlist, INPUT_NAMES};
use reda::compile::compile;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda_viewer::Session;

const MAX_TICKS: u64 = 2000;

/// Must match `viewer/src/lib.rs`'s private `GEOMETRY_BYTES_PER_CELL` --
/// re-declared here rather than exported, so this test decodes the *public*
/// byte contract `geometry()` documents, not an internal constant.
const GEOMETRY_BYTES_PER_CELL: usize = 9;

/// The `[kind_id, strength]` pair `geometry()`/`strengths()` should report
/// for `state`, taken straight from `slice()`'s (and `geometry()`'s) own
/// documented contract: `kind as u8`, dust's own analog `power`, and
/// everything else boolean off `power`/`lit`. This mirrors
/// `and4_truth_table.rs`'s `expected_bytes`, reimplemented independently here
/// so a bug shared between `geometry()`/`strengths()` and this helper
/// wouldn't hide behind both agreeing with each other.
fn expected(state: &BlockState) -> (u8, u8) {
    let kind_id = state.kind as u8;
    let strength = if state.kind == BlockKind::RedstoneWire {
        state.power
    } else if state.power > 0 || state.lit {
        15
    } else {
        0
    };
    (kind_id, strength)
}

/// The byte `geometry()` should report for `state.facing`, reimplemented
/// independently of `viewer/src/lib.rs`'s private `facing_id` -- North
/// through Down as 0..6, 255 for `None`. This is the check the plan calls out
/// by name: "a facing that silently defaulted would point every repeater the
/// same way and look entirely plausible", so this helper must compute its
/// expectation from `BlockState::facing` directly, never from whatever
/// `geometry()` itself happened to emit.
fn expected_facing_id(state: &BlockState) -> u8 {
    match state.facing {
        Some(Facing::North) => 0,
        Some(Facing::South) => 1,
        Some(Facing::East) => 2,
        Some(Facing::West) => 3,
        Some(Facing::Up) => 4,
        Some(Facing::Down) => 5,
        None => 255,
    }
}

/// The byte `geometry()` should report for `state.face` (the lever/button
/// attachment surface), reimplemented independently of `viewer/src/lib.rs`'s
/// private `face_id` for the same reason as [`expected_facing_id`].
fn expected_face_id(state: &BlockState) -> u8 {
    match state.face {
        Some(Face::Floor) => 0,
        Some(Face::Wall) => 1,
        Some(Face::Ceiling) => 2,
        None => 255,
    }
}

#[test]
fn geometry_and_strengths_agree_with_world_get_in_order() {
    let (netlist, _output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
    let input_positions = compiled.input_positions.clone();

    let mut ground_truth = Simulator::new(compiled.world);
    // An asymmetric pattern, not all-on/all-off: with every lever the same
    // value, several coordinates would happen to read as the same byte pair
    // regardless of a transposition bug, hiding exactly the kind of ordering
    // mistake this test exists to catch.
    let pattern = [true, false, true, true];
    for (&name, &on) in INPUT_NAMES.iter().zip(pattern.iter()) {
        let position = input_positions[name];
        let mut state = ground_truth.world().get(position.0, position.1, position.2).clone();
        state.lit = on;
        ground_truth.world_mut().set(position.0, position.1, position.2, state);
    }
    ground_truth
        .run_until_stable(MAX_TICKS)
        .expect("and4 must settle for this ground-truth simulator");

    let mut session = Session::new("and4").expect("and4 must be a valid circuit name");
    for (&name, &on) in INPUT_NAMES.iter().zip(pattern.iter()) {
        session.set_lever(name, on).unwrap_or_else(|_| panic!("`{name}` must be a valid input"));
    }
    session.run_until_stable().expect("and4 must settle through the wasm API too");

    let geometry = session.geometry();
    let strengths = session.strengths();

    assert_eq!(
        geometry.len() % GEOMETRY_BYTES_PER_CELL,
        0,
        "geometry() length must be a whole number of {GEOMETRY_BYTES_PER_CELL}-byte cells, got {} bytes",
        geometry.len()
    );
    let cell_count = geometry.len() / GEOMETRY_BYTES_PER_CELL;
    assert_eq!(
        cell_count,
        strengths.len(),
        "geometry() and strengths() must report the same number of cells"
    );
    assert!(cell_count > 0, "and4 must have at least one non-air block");

    let (size_x, size_y, size_z) = ground_truth.world().size();

    // Completeness: every non-air cell in the ground-truth world must appear
    // in geometry() exactly once -- a per-entry check alone couldn't catch a
    // silently dropped cell.
    let mut expected_count = 0usize;
    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                if ground_truth.world().get(x, y, z).kind != BlockKind::Air {
                    expected_count += 1;
                }
            }
        }
    }
    assert_eq!(
        cell_count, expected_count,
        "geometry() must include every non-air cell in the world exactly once"
    );

    let mut mismatches = Vec::new();
    let mut last_flat: i64 = -1;
    // Distinct facing bytes seen among repeaters -- and4's router turns
    // corners, so its repeaters genuinely point in more than one direction.
    // A `geometry()` bug that always emitted the same facing (e.g. a
    // forgotten field, defaulting to 0/North) would pass every per-entry
    // check below only if it happened to match reality everywhere; this set
    // is the direct check the plan asks for: catch a facing that "look[s]
    // entirely plausible" by construction, not by coincidence.
    let mut repeater_facings_seen = std::collections::BTreeSet::new();
    let entries = geometry.chunks_exact(GEOMETRY_BYTES_PER_CELL).zip(strengths.iter());
    for (i, (cell, &strength)) in entries.enumerate() {
        let x = u16::from_le_bytes([cell[0], cell[1]]) as i32;
        let y = u16::from_le_bytes([cell[2], cell[3]]) as i32;
        let z = u16::from_le_bytes([cell[4], cell[5]]) as i32;
        let kind_id = cell[6];
        let facing_id = cell[7];
        let face_id = cell[8];

        // Strict ascending-YZX order, matching World's own internal layout
        // (see World's module doc comment and `World::decode`).
        let flat = (y as i64) * (size_x as i64) * (size_z as i64)
            + (z as i64) * (size_x as i64)
            + (x as i64);
        assert!(
            flat > last_flat,
            "geometry() entry {i} at ({x}, {y}, {z}) (flat {flat}) is not in strictly \
             ascending YZX order (previous flat index was {last_flat})"
        );
        last_flat = flat;

        let state = ground_truth.world().get(x, y, z);
        assert_ne!(
            state.kind,
            BlockKind::Air,
            "geometry() entry {i} at ({x}, {y}, {z}) must not be air"
        );
        let (expected_kind, expected_strength) = expected(state);
        let expected_facing = expected_facing_id(state);
        let expected_face = expected_face_id(state);
        if kind_id != expected_kind || strength != expected_strength {
            mismatches.push(format!(
                "entry {i} at ({x}, {y}, {z}): geometry/strengths say kind {kind_id}, strength \
                 {strength}; World::get says kind {expected_kind}, strength {expected_strength}"
            ));
        }
        if facing_id != expected_facing {
            mismatches.push(format!(
                "entry {i} at ({x}, {y}, {z}): geometry says facing byte {facing_id}; \
                 World::get's actual facing ({:?}) maps to {expected_facing}",
                state.facing
            ));
        }
        if face_id != expected_face {
            mismatches.push(format!(
                "entry {i} at ({x}, {y}, {z}): geometry says face byte {face_id}; \
                 World::get's actual face ({:?}) maps to {expected_face}",
                state.face
            ));
        }
        if kind_id == BlockKind::Repeater as u8 {
            repeater_facings_seen.insert(facing_id);
        }
    }

    assert!(
        repeater_facings_seen.len() > 1,
        "and4's repeaters must point in more than one direction (router turns corners) -- only \
         saw facing byte(s) {repeater_facings_seen:?}. A single value here is exactly what a \
         defaulted `facing` field would produce, and would look plausible on screen while being \
         wrong for most repeaters."
    );

    assert!(
        mismatches.is_empty(),
        "geometry()/strengths() disagreed with World::get ({} mismatches):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
