//! The circuit the browser draws is the circuit the compiler placed.
//!
//! `viewer/` builds the same crate for wasm, so once `compile()` uses the
//! relaxation placer the layout on screen is the layout this code chose -- and
//! only if the two toolchains compute the same numbers. Whether they do is a
//! fact about two backends, not something a comment can settle, so it is
//! stated here as a test that runs under both.
//!
//! # What actually differs between the two backends
//!
//! Almost nothing, and the interesting part is *which* almost. Addition,
//! multiplication, division and `sqrt` are exactly specified by IEEE-754 and
//! are correctly rounded on both targets; the solver's only `sqrt` is the
//! Cholesky pivot, it contracts no FMA (there is no `mul_add` in the crate, no
//! `.cargo/config.toml`, no `[profile]` and no `target-cpu`), and the
//! relaxation reaches no `HashMap`, no sort and no source of entropy -- the
//! perturbation is integer splitmix64 and ships with seed 0, which returns
//! before touching a body.
//!
//! The two operations that are genuinely *lowered* differently are the two
//! such a list usually leaves out. `f64::round` (`relax/snap.rs`) is ties away
//! from zero, while wasm's `f64.nearest` opcode is ties to even, so the wasm
//! build cannot use the opcode and goes through a libm routine instead. And
//! `f64::max`/`min` (`relax/mod.rs`, `relax/project.rs`) is `llvm.maxnum`,
//! which differs from wasm's `f64.max` on NaN and on signed zero. Both are
//! exactly specified where it counts and neither NaN nor `-0.0` is reachable
//! on the converged path -- but "expected to agree" is the argument, and these
//! two tests are the measurement.
//!
//! # Two fingerprints, because they prove different things
//!
//! [`and4_places_identically_wherever_it_is_built`] compares the layout after
//! `snap` has rounded it. That is the property that matters to a reader: it is
//! the circuit the viewer draws and the emitter builds. It is also *coarse* --
//! `planner::measure_snapped_fingerprint_slack` measures the tightest rounding
//! margin on and4 at 0.0268 cells, thirteen orders of magnitude above `f64`
//! epsilon, so a divergence in the last bits passes it silently and surfaces
//! later, on some other circuit, as a whole cell with no way to date it.
//!
//! [`and4_solves_to_the_same_bits_wherever_it_is_built`] compares the solve
//! before rounding, as raw `f64` bit patterns. That is the sharp one: nothing
//! below it is a difference at all. It also runs before `route_every_net`, so
//! a routing failure under wasm shows up as its own panic instead of wearing
//! this test's name.
//!
//! If either fails, **do not adjust the fixture.** Record which body moved and
//! by how much; the remedy is to make the solver exact rather than to make the
//! test agree with itself. `Body::position` in `i64` at 1/1024 of a cell would
//! do it -- the arithmetic is addition, multiplication and comparison, and the
//! one `sqrt` is a Cholesky pivot that can take an integer square root -- and
//! that is a separate task, which these tests are what would justify.

use reda::circuits::and4::build_and4_netlist;
use reda::compile::planner::{
    continuous_placement_fingerprint, placement_fingerprint, plan_from_netlist, PortPlacements,
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

/// Compare, and on a mismatch print the computed side with `{}` first.
///
/// `assert_eq!` formats with `{:?}`, so a multi-line fingerprint arrives as one
/// backslash-escaped line. Pasting *that* into the fixture writes the two
/// characters `\` and `n`, after which the test fails forever with a message
/// indistinguishable from a real toolchain disagreement -- the exact confusion
/// these tests exist to prevent, arriving one step later than expected. So the
/// unescaped copy is printed first, and the fixture is reproducible by
/// copy-paste or by redirection.
///
/// **Native only.** Measured 2026-08-15: under `wasm-pack test --node` this
/// `eprintln!` does not reach the output at all, while the `assert_eq!` panic
/// does, escapes and all. That costs nothing -- the fixture is produced on the
/// native side, and a wasm divergence still arrives as a readable `left`/`right`
/// pair that needs unescaping -- but do not expect this line there.
fn agrees(what: &str, computed: &str, expected: &str) {
    if computed.trim() == expected.trim() {
        return;
    }
    eprintln!("--- {what}: this toolchain computed ---\n{computed}\n--- end ---");
    assert_eq!(
        computed.trim(),
        expected.trim(),
        "this toolchain placed and4 somewhere else ({what})"
    );
}

/// One function, two harnesses. `wasm-bindgen-test-runner` collects only
/// `#[wasm_bindgen_test]`, and `cargo test` on the host collects only
/// `#[test]` -- so a single attribute means one of the two runs nothing and
/// says nothing, which is the outcome this whole file exists to rule out.
///
/// Do **not** add `wasm_bindgen_test_configure!(run_in_browser)` here. Under
/// `wasm-pack test --node` that turns the file into a silent no-op that prints
/// a skip line and exits 0 -- measured on 2026-08-15 with a deliberately
/// failing assertion in place, which it reported as success.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn and4_places_identically_wherever_it_is_built() {
    let (netlist, _) = build_and4_netlist();
    let candidate = plan_from_netlist(&netlist, &PortPlacements::default()).expect("places");
    let fingerprint = placement_fingerprint(&candidate);

    agrees(
        "snapped anchors and facings",
        &fingerprint,
        include_str!("fixtures/and4_placement.txt"),
    );
}

/// The same placement one step earlier, bit for bit.
///
/// `include_str!` rather than a runtime `read_to_string`, in both tests: this
/// runs under `wasm-pack test --node`, where a relative file read does not
/// resolve to the same path. The fixture is a build input, which is why it has
/// to exist -- empty is fine -- before anything can compile.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn and4_solves_to_the_same_bits_wherever_it_is_built() {
    let (netlist, _) = build_and4_netlist();
    let fingerprint = continuous_placement_fingerprint(&netlist, &PortPlacements::default())
        .expect("relaxes");

    agrees(
        "continuous solve, f64 bits",
        &fingerprint,
        include_str!("fixtures/and4_placement_bits.txt"),
    );
}
