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
//! tests are the measurement.
//!
//! # Two fingerprints, because they prove different things
//!
//! The `*_places_identically_*` tests compare the layout after `snap` has
//! rounded it. That is the property that matters to a reader: it is the circuit
//! the viewer draws and the emitter builds. It is also *coarse* --
//! `planner::measure_snapped_fingerprint_slack` measures the tightest rounding
//! margin on and4 at 0.0268 cells, thirteen orders of magnitude above `f64`
//! epsilon, so a divergence in the last bits passes it silently and surfaces
//! later, on some other circuit, as a whole cell with no way to date it.
//!
//! The `*_solves_to_the_same_bits_*` tests compare the solve before rounding,
//! as raw `f64` bit patterns. Those are the sharp ones: nothing below them is a
//! difference at all. They also run before `route_every_net`, which is what
//! lets the sharp comparison cover circuits that cannot be routed at all --
//! see below.
//!
//! # Why four circuits get the bit test and only two get the snapped one
//!
//! The bit test is `continuous_placement_fingerprint`, which stops at `relax`;
//! the snapped test goes through `plan_from_netlist`, which ends in
//! `route_every_net`. All four reference circuits *place*. Only two *route*.
//! Measured 2026-08-15 by [`which_reference_circuits_place_and_which_also_route`],
//! `cargo test --release`, this tree:
//!
//! | circuit | bodies | `continuous_placement_fingerprint` | `plan_from_netlist` |
//! |---|---|---|---|
//! | and4 | 11 | Ok | Ok |
//! | full_adder | 25 | Ok | Ok |
//! | segment_a | 50 | Ok | **Err**: `no safe local route from (99, 1, 97) to (122, 1, 89)` |
//! | seven_segment | 88 | Ok | **Err**: `no safe local route from (83, 1, 106) to (83, 1, 96)` |
//!
//! Body counts and the two addresses are exact and reproduce run to run; the
//! harness also prints wall-clock times, which do not (segment_a's failure was
//! 32.3s and 43.1s in two runs on the same machine) and so are not pinned here.
//! The shape of them is the part worth knowing: placing all four costs under
//! two seconds, and the two failures cost about half a minute *each*, which is
//! why nothing in this file routes segment_a or above.
//!
//! So segment_a and seven_segment carry a bit test and no snapped test, and the
//! reason is a routing defect that predates this file --
//! `planner::how_far_the_planners_own_placement_carries` is the `#[ignore]`d
//! marker that owns it. A snapped test for either would fail on
//! `.expect("places")` before it ever compared a fingerprint, which is a test
//! reporting somebody else's bug under this file's name. When that route is
//! fixed, the two missing fixtures are the first thing to add here.
//!
//! Covering them at all is the point of the split: the snapped margin shrinks
//! **17x** from and4's 0.0268 cells to segment_a's 0.0016, so the larger
//! circuits are where a divergence would show first, and they are exactly the
//! ones the routable path cannot reach.
//!
//! # Regenerating every fixture in one run
//!
//! On a mismatch each test prints its computed fingerprint unescaped between
//! two marker lines (see [`agrees`]). Truncate the fixtures, run once
//! natively, and split the output back out:
//!
//! ```bash
//! cd viewer
//! for f in tests/fixtures/*_placement*.txt; do : > "$f"; done
//! cargo test --release --test placement_agrees_with_native 2>&1 |
//!   awk '/^--- BEGIN /{f="tests/fixtures/"$3".txt"; next} /^--- END ---$/{f=""; next} f{print > f}'
//! cargo test --release --test placement_agrees_with_native   # now green
//! ```
//!
//! The fixtures are LF; `core.autocrlf` is `false` and there is no
//! `.gitattributes`, so they round-trip through git unchanged.
//!
//! If any of these fails, **do not adjust the fixture.** Record which body
//! moved and by how much; the remedy is to make the solver exact rather than to
//! make the test agree with itself. `Body::position` in `i64` at 1/1024 of a
//! cell would do it -- the arithmetic is addition, multiplication and
//! comparison, and the one `sqrt` is a Cholesky pivot that can take an integer
//! square root -- and that is a separate task, which these tests are what would
//! justify.

use reda::circuits::and4::build_and4_netlist;
use reda::circuits::full_adder::build_full_adder_netlist;
use reda::circuits::seven_segment::{build_seven_segment_netlist, build_single_segment_netlist};
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
/// unescaped copy is printed first, wrapped in markers that name the fixture it
/// belongs to, and the whole set is reproducible by one redirection.
///
/// **Native only.** Measured 2026-08-15: under `wasm-pack test --node` this
/// `eprintln!` does not reach the output at all, while the `assert_eq!` panic
/// does, escapes and all. That costs nothing -- the fixtures are produced on the
/// native side, and a wasm divergence still arrives as a readable `left`/`right`
/// pair that needs unescaping -- but do not expect this line there.
fn agrees(fixture: &str, circuit: &str, what: &str, computed: &str, expected: &str) {
    if computed.trim() == expected.trim() {
        return;
    }
    eprintln!("--- BEGIN {fixture} ---\n{computed}\n--- END ---");
    assert_eq!(
        computed.trim(),
        expected.trim(),
        "this toolchain placed {circuit} somewhere else ({what})"
    );
}

/// The snapped layout: what the viewer draws and the emitter builds.
///
/// Goes through `plan_from_netlist`, so it also asserts the circuit **routes**
/// on this toolchain -- a wasm routing failure arrives here as a panic in
/// `.expect("places")`. That is deliberate and it is the only wasm evidence in
/// the tree that `route_every_net`'s `HashMap`s do not stop the build; the
/// routes themselves are still not compared.
fn snapped_agrees(fixture: &str, circuit: &str, netlist: &reda::compile::Netlist, expected: &str) {
    let candidate = plan_from_netlist(netlist, &PortPlacements::default()).expect("places");
    agrees(
        fixture,
        circuit,
        "snapped anchors and facings",
        &placement_fingerprint(&candidate),
        expected,
    );
}

/// The same placement one step earlier, bit for bit.
///
/// `include_str!` rather than a runtime `read_to_string`, in every test here:
/// this runs under `wasm-pack test --node`, where a relative file read does not
/// resolve to the same path. The fixture is a build input, which is why it has
/// to exist -- empty is fine -- before anything can compile.
fn bits_agree(fixture: &str, circuit: &str, netlist: &reda::compile::Netlist, expected: &str) {
    let fingerprint =
        continuous_placement_fingerprint(netlist, &PortPlacements::default()).expect("relaxes");
    agrees(
        fixture,
        circuit,
        "continuous solve, f64 bits",
        &fingerprint,
        expected,
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
///
/// One test function per fixture rather than a loop, for the same reason:
/// `wasm-bindgen-test-runner` counts functions, and `check.sh` reads that count.
/// A loop over six circuits is one test, and six of them silently becoming five
/// is a change the count cannot see.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn and4_places_identically_wherever_it_is_built() {
    snapped_agrees(
        "and4_placement",
        "and4",
        &build_and4_netlist().0,
        include_str!("fixtures/and4_placement.txt"),
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn and4_solves_to_the_same_bits_wherever_it_is_built() {
    bits_agree(
        "and4_placement_bits",
        "and4",
        &build_and4_netlist().0,
        include_str!("fixtures/and4_placement_bits.txt"),
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn full_adder_places_identically_wherever_it_is_built() {
    snapped_agrees(
        "full_adder_placement",
        "full_adder",
        &build_full_adder_netlist().0,
        include_str!("fixtures/full_adder_placement.txt"),
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn full_adder_solves_to_the_same_bits_wherever_it_is_built() {
    bits_agree(
        "full_adder_placement_bits",
        "full_adder",
        &build_full_adder_netlist().0,
        include_str!("fixtures/full_adder_placement_bits.txt"),
    );
}

/// 50 bodies, and the tightest snapped rounding margin of the four at 0.0016
/// cells -- 17x tighter than and4's. No snapped companion: this circuit does
/// not route from this planner's placement, see the module doc's table.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn segment_a_solves_to_the_same_bits_wherever_it_is_built() {
    bits_agree(
        "segment_a_placement_bits",
        "segment_a",
        &build_single_segment_netlist(0).0,
        include_str!("fixtures/segment_a_placement_bits.txt"),
    );
}

/// 88 bodies, the largest solve in the tree: 264 `f64`s and 11 steps of
/// projection to get there. No snapped companion, same reason as segment_a.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn seven_segment_solves_to_the_same_bits_wherever_it_is_built() {
    bits_agree(
        "seven_segment_placement_bits",
        "seven_segment",
        &build_seven_segment_netlist().0,
        include_str!("fixtures/seven_segment_placement_bits.txt"),
    );
}

/// Which reference circuits place, and which of those also route: the method
/// behind the module doc's table, in the tree rather than in a scratch file.
///
/// ```bash
/// cd viewer && cargo test --release --test placement_agrees_with_native \
///   which_reference_circuits_place_and_which_also_route -- --ignored --nocapture
/// ```
///
/// Asserts nothing and swallows every failure, like the three harnesses in
/// `planner`'s test module, so one circuit that cannot route does not hide the
/// rest. That is the whole reason it exists next to
/// `planner::how_far_the_planners_own_placement_carries`, which asserts the
/// same thing and *panics* on segment_a -- so it has never once reported what
/// seven_segment does, and the tree said so.
///
/// **This is the file's own boundary condition.** Two circuits here carry a
/// bit fixture and no snapped one, and this harness is what decides that.
/// When the segment_a route is fixed, this goes green in the fourth column and
/// the two missing fixtures become addable in the same run.
///
/// Native only. It spends about a minute inside `route_every_net` failing
/// twice over, and nothing it measures is a fact about wasm.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "measurement harness: asserts nothing, prints the module doc's table, and takes about a minute"]
fn which_reference_circuits_place_and_which_also_route() {
    for (name, netlist) in [
        ("and4", build_and4_netlist().0),
        ("full_adder", build_full_adder_netlist().0),
        ("segment_a", build_single_segment_netlist(0).0),
        ("seven_segment", build_seven_segment_netlist().0),
    ] {
        let started = std::time::Instant::now();
        let placed = match continuous_placement_fingerprint(&netlist, &PortPlacements::default()) {
            // The `steps` line is not a body, hence the `- 1`.
            Ok(text) => format!(
                "Ok, {} bodies, {:.1}s",
                text.lines().count() - 1,
                started.elapsed().as_secs_f64()
            ),
            Err(error) => format!("Err after {:.1}s: {error}", started.elapsed().as_secs_f64()),
        };

        let started = std::time::Instant::now();
        let routed = match plan_from_netlist(&netlist, &PortPlacements::default()) {
            Ok(_) => format!("Ok, {:.1}s", started.elapsed().as_secs_f64()),
            Err(error) => format!("Err after {:.1}s: {error}", started.elapsed().as_secs_f64()),
        };

        eprintln!("{name}: continuous {placed} | plan_from_netlist {routed}");
    }
}
