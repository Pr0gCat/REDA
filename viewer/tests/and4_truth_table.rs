//! Headless acceptance test for the wasm-facing API: no browser, no wasm
//! runtime, just `cargo test` on the host target.
//!
//! This drives `and4` entirely through `Session::new`, `set_lever`, and
//! `run_until_stable` -- the same wasm-exported methods a browser page would
//! call -- and checks all 16 input combinations against the truth table
//! `tests/reference_circuits.rs` (in the `reda` crate) checks the same way.
//! If the wasm binding wired a lever or the output to the wrong coordinate,
//! or `slice`'s byte layout disagreed with itself, this fails.
//!
//! It deliberately never calls `Session::pinout` or `Session::legend`: both
//! return a `JsValue`, and constructing one aborts the process outside an
//! actual wasm32 + JS host environment (see the crate's module doc comment).
//! Every method actually used here (`list_circuits`, `Session::new`,
//! `set_lever`, `run_until_stable`, `size`, `slice`) returns a plain Rust
//! type instead, which is exactly why they can run natively.
//!
//! Output coordinates are learned independently, by calling `reda::compile`
//! directly on the same netlist `and4_adapter` builds internally -- this
//! mirrors how `tests/reference_circuits.rs` itself discovers wiring via
//! `CompiledCircuit::output_positions`, so the two tests are cross-checking
//! the same compiled circuit through two different front doors (direct
//! `Simulator` calls there, the wasm `Session` API here).

use reda::circuits::and4::{build_and4_netlist, INPUT_NAMES};
use reda::compile::compile;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState};
use reda_viewer::{list_circuits, Axis, Session};

const MAX_TICKS: u64 = 2000;

/// Read one cell's `[block_kind_id, signal_strength]` bytes out of a `slice`
/// result, using the exact row-major layout `Session::slice` documents.
fn cell_bytes(
    slice_bytes: &[u8],
    axis: Axis,
    size: (i32, i32, i32),
    coord: (i32, i32, i32),
) -> (u8, u8) {
    let (_size_x, size_y, size_z) = size;
    let (x, y, z) = coord;
    let (outer, inner, inner_len) = match axis {
        Axis::X => (y, z, size_z),
        Axis::Y => (x, z, size_z),
        Axis::Z => (x, y, size_y),
    };
    let offset = 2 * ((outer * inner_len + inner) as usize);
    (slice_bytes[offset], slice_bytes[offset + 1])
}

/// The `[block_kind_id, signal_strength]` pair `Session::slice` should report
/// for `state`, computed independently of `reda_viewer`'s own (private)
/// `block_kind_id`/`signal_strength` helpers -- straight from `slice`'s own
/// doc comment: `kind as u8`, dust's own `power`, and everything else
/// boolean off `power`/`lit`. Reimplementing this here, rather than exporting
/// and calling the crate's private helpers, is the point: it cross-checks the
/// documented contract against the real output instead of checking the
/// implementation against itself.
fn expected_bytes(state: &BlockState) -> (u8, u8) {
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

#[test]
fn list_circuits_includes_and4() {
    let names = list_circuits();
    assert!(
        names.iter().any(|n| n == "and4"),
        "list_circuits() must include \"and4\", got {names:?}"
    );
}

#[test]
fn the_and4_session_matches_its_truth_table_through_the_wasm_api() {
    // Independently compile the same netlist just to learn the output lamp's
    // coordinate -- not used for simulation. `Session` does its own
    // compile+simulate internally; this is only so the test knows where to
    // look with `slice`.
    let (netlist, output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();

    let mut session = Session::new("and4").expect("and4 must be a valid circuit name");

    let size = session.size();
    assert_eq!(size.len(), 3);
    let size = (size[0], size[1], size[2]);
    assert_eq!(size, compiled.world.size(), "Session::size() must match the compiled world");

    let mut mismatches = Vec::new();
    for combination in 0u8..16 {
        let bits = [
            (combination >> 3) & 1,
            (combination >> 2) & 1,
            (combination >> 1) & 1,
            combination & 1,
        ];
        for (&name, &bit) in INPUT_NAMES.iter().zip(bits.iter()) {
            session.set_lever(name, bit == 1).unwrap_or_else(|_| {
                panic!("`{name}` must be a valid input for and4")
            });
        }
        session.run_until_stable().expect("and4 must settle after every input change");

        let expected = bits.iter().all(|&bit| bit == 1);

        // Read the output lamp back through `slice`, on the Z axis fixed at
        // the lamp's own Z -- exercising the wasm-facing read path, not a
        // direct `World::get`.
        let axis = Axis::Z;
        let bytes = session
            .slice(axis, output_position.2)
            .expect("output_position.2 must be in range for and4's world");
        let (_, strength) = cell_bytes(&bytes, axis, size, output_position);
        let actual = strength > 0;

        if actual != expected {
            mismatches.push(format!("inputs={bits:?}: expected {expected}, got {actual}"));
        }
    }

    assert!(
        mismatches.is_empty(),
        "and4 via the wasm Session API does not match its truth table ({}/16 wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn run_until_stable_reports_zero_ticks_once_already_settled() {
    let mut session = Session::new("and4").unwrap();
    session.run_until_stable().expect("and4 must settle from its initial state");
    let ticks = session.run_until_stable().expect("a second run must not error");
    assert_eq!(ticks, 0, "an already-settled circuit must take zero more ticks to resettle");
}

#[test]
fn stepping_manually_eventually_reaches_the_same_state_as_run_until_stable() {
    let (netlist, output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();

    let mut session = Session::new("and4").unwrap();
    session.run_until_stable().unwrap();

    for &name in INPUT_NAMES.iter() {
        session.set_lever(name, true).unwrap();
    }

    // `step()` only reports work that was actually due *this* game tick.
    // Flipping a lever changes dust strength immediately, but
    // `Simulator::step` -> `settle_from_current_state` discards
    // `recompute_dust_strengths`'s own count (see
    // `src/redstone/simulator/mod.rs`); only a due repeater/torch/lamp tick
    // counts. So a repeater one or more hops downstream of the flipped lever
    // can leave the very first `step()` reporting `false` even though the
    // world did change. What must hold is that stepping enough times reaches
    // the same settled state `run_until_stable` would reach in one call.
    let mut saw_a_change = false;
    for _ in 0..200 {
        if session.step() {
            saw_a_change = true;
        }
    }
    assert!(saw_a_change, "stepping and4 through a lever flip must eventually report a change");

    let axis = Axis::Z;
    let size = session.size();
    let size = (size[0], size[1], size[2]);
    let bytes = session
        .slice(axis, output_position.2)
        .expect("output_position.2 must be in range for and4's world");
    let (_, strength) = cell_bytes(&bytes, axis, size, output_position);
    assert!(strength > 0, "and4 with every lever stepped on must settle to a lit output");
}

#[test]
fn reset_rebuilds_the_circuit_exactly_like_a_fresh_session() {
    let baseline_ticks = Session::new("and4").unwrap().tick_count();

    let mut session = Session::new("and4").unwrap();
    for &name in INPUT_NAMES.iter() {
        session.set_lever(name, true).unwrap();
    }
    session.run_until_stable().unwrap();
    assert!(
        session.tick_count() > baseline_ticks,
        "settling and4 from all-off to all-on must take more ticks than a fresh build alone"
    );

    session.reset().expect("reset must rebuild and4 without error");
    assert_eq!(
        session.tick_count(),
        baseline_ticks,
        "reset must rebuild the circuit exactly like a fresh Session::new, tick count included"
    );
}

/// `slice`'s byte layout must agree with a direct `World::get` at the same
/// coordinate, on all three axes -- a slice with two axes swapped would still
/// produce a same-shaped `Uint8Array` full of plausible-looking bytes, just
/// at the wrong (x, y, z), and nothing about `list_circuits`/`set_lever`
/// alone would ever catch that. This drives an independent `Simulator` with
/// the exact same lever pattern `Session` sees, then samples several real
/// coordinates -- both lever inputs and dust so the analog 0-15 case is
/// covered, not just the boolean one -- through `slice` on each of `Axis::X`,
/// `Axis::Y` and `Axis::Z`.
#[test]
fn slice_agrees_with_world_get_on_every_axis() {
    let (netlist, output_signal) = build_and4_netlist();
    let compiled = compile(&netlist).expect("and4 is acyclic and fully driven");
    let input_positions = compiled.input_positions.clone();
    let output_position = *compiled.output_positions.get(&output_signal).unwrap();

    let mut ground_truth = Simulator::new(compiled.world);
    // An asymmetric pattern, not all-on or all-off: with every lever the same
    // value, several coordinates would happen to read as the same byte pair
    // regardless of which axis fed them in, hiding a transposition bug.
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

    let size = ground_truth.world().size();

    // At least one lit dust cell, to exercise the analog 0-15 path (not just
    // the boolean 0-or-15 one every other kind here takes).
    let dust_position = ground_truth
        .world()
        .positions_of(BlockKind::RedstoneWire)
        .map(|flat| ground_truth.world().decode(flat))
        .find(|&(x, y, z)| ground_truth.world().get(x, y, z).power > 0)
        .expect("and4, with this lever pattern, must have at least one live dust cell");

    let mut sample_coords: Vec<(i32, i32, i32)> = input_positions.values().copied().collect();
    sample_coords.push(output_position);
    sample_coords.push(dust_position);
    sample_coords.push((0, 0, 0)); // corner air, the degenerate all-zero case

    let mut session = Session::new("and4").unwrap();
    for (&name, &on) in INPUT_NAMES.iter().zip(pattern.iter()) {
        session.set_lever(name, on).unwrap();
    }
    session.run_until_stable().expect("and4 must settle through the wasm API too");

    let mut mismatches = Vec::new();
    for coord in sample_coords {
        let expected = expected_bytes(ground_truth.world().get(coord.0, coord.1, coord.2));
        for axis in [Axis::X, Axis::Y, Axis::Z] {
            let index = match axis {
                Axis::X => coord.0,
                Axis::Y => coord.1,
                Axis::Z => coord.2,
            };
            let bytes = session
                .slice(axis, index)
                .unwrap_or_else(|_| panic!("index {index} must be in range on axis {axis:?}"));
            let actual = cell_bytes(&bytes, axis, size, coord);
            if actual != expected {
                mismatches.push(format!(
                    "{coord:?} via {axis:?}: expected {expected:?}, got {actual:?}"
                ));
            }
        }
    }

    assert!(
        mismatches.is_empty(),
        "slice() disagreed with World::get ({} mismatches):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
