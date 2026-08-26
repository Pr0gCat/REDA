//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod build;
mod linear;
mod project;
mod snap;

// Re-exported rather than kept private: `relax` and `snap` below reach some of
// these, and the four submodules are named by nobody outside this directory at
// all. Since Task 10 there is one outside caller, `planner`; Task 11's tests
// added `project`, `project_for_test`, `CONDUCTOR_CLEARANCE`, `GROUND`,
// `SETTLED` and `VERTICAL_CLEARANCE` to what it reaches, so the list is no
// longer worth enumerating here -- `grep -n "relax::" src/compile/planner.rs`
// is. Everything else is reached from inside. A `pub` item in a private module
// that nobody reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use build::{
    attach_offset, build, cells, pin_hops, Attach, Body, BodyGraph, BodyKind, Cell, Pull, Weld,
    SIGNAL_REST_LENGTH, SIGNAL_STIFFNESS,
};
pub use linear::{Factorisation, NotPositiveDefinite};
pub use project::{
    placed_cells, project, required_separations, reservation, worst_violation, Axes, PlacedCell,
    Violation, CONDUCTOR_CLEARANCE, GROUND, PROJECTION_ROUNDS, ROUTE_PITCH, SETTLED, SNAP_MARGIN,
    VERTICAL_CLEARANCE,
};
pub use snap::{snap, SnappedNode};

/// [`incident_energy`], for a measurement that lives in `planner`.
///
/// `choose_facings` picks a facing by a strict `<` over four of these, which
/// makes it the one place in the solver where a last-bit difference becomes a
/// different answer without any rounding boundary to cross.
/// `planner::measure_snapped_fingerprint_slack` reports how close the winner
/// and the runner-up come, and it cannot do that through `choose_facings`,
/// which returns only whether something turned.
///
/// `#[cfg(test)]` and no wider than the crate: this is a probe, not a second
/// way to ask the solver a question.
#[cfg(test)]
pub(crate) fn incident_energy_for_test(graph: &BodyGraph, body: usize) -> f64 {
    incident_energy(graph, body)
}

/// One fixture, for a test that lives in `planner`.
///
/// `project.rs` has this graph already, as its own `graph_of(vec![body(..),
/// body(..)], Vec::new())`, but a test module is not reachable from another
/// crate module -- and what `planner`'s test needs is to state a body graph in
/// two coordinates rather than to build six `Body` fields by hand. `#[cfg(test)]`
/// because a fixture is not something the shipping crate should offer.
#[cfg(test)]
pub(crate) mod project_for_test {
    use super::{Body, BodyGraph, BodyKind};
    use crate::compile::geometry::CellFacing;
    use crate::compile::topology::Primitive;

    /// Two unwelded, unpinned bodies, each on its own net.
    ///
    /// Its own net, because two cells carrying the same signal are exempt from
    /// separation -- the route between them is what makes them one thing -- and
    /// a fixture that shared a name would be testing the exemption instead.
    pub fn two_free_bodies(a: [f64; 3], b: [f64; 3]) -> BodyGraph {
        let body = |position: [f64; 3]| Body {
            what: BodyKind::Primitive { node: 0, kind: Primitive::Torch },
            position,
            inputs: vec![format!("net{}{}{}", position[0], position[1], position[2])],
            output: None,
            facing: CellFacing::NORTH,
            pinned: false,
        };
        BodyGraph {
            bodies: vec![body(a), body(b)],
            pulls: Vec::new(),
            welds: Vec::new(),
            nodes: vec![vec![0], vec![1]],
            anchor_body: vec![0, 1],
        }
    }
}

use crate::compile::planner::{Anchor, PortPlacements};
use crate::compile::primitive_graph::PrimitiveGraph;
use crate::compile::Netlist;

/// How far a body may still be moving and the relaxation still be finished.
///
/// A tenth of a cell, because the rounding margin is a whole one: a system
/// still twitching below that cannot change what `snap` produces, and running
/// past it buys nothing measurable.
pub const CONVERGED: f64 = 0.1;

/// How hard a body is pulled toward where it was last legally placed.
///
/// This is the `c` of the `Ax = f + c` the founding spec cites, and dropping
/// it is why an earlier draft of this design did not converge. An exact solve
/// has zero-rest-length springs collapse every free body onto its neighbours,
/// so the projection unpicks the same knot every step and the two take turns
/// undoing each other. Measured on 2026-08-13: `and4` deadlocked with two
/// bodies 0.030 too close and `full_adder` with two 1.372, from the starting
/// layout and from a naive grid alike -- while the projection *alone*
/// converged from both.
///
/// One, because that is one more spring of the same `k = 1` every signal
/// spring has: the weakest anchor that is not no anchor.
pub const ANCHOR_STIFFNESS: f64 = 1.0;

/// What the anchor is multiplied by after each step.
///
/// Doubling, so the anchor overwhelms a bounded degree in a number of steps
/// logarithmic in it.
///
/// **Raising either anchor number converges sooner and places worse.** An
/// earlier draft of this comment claimed the opposite -- that the schedule
/// decides only how many steps are spent, not what is found -- and a parameter
/// sweep on 2026-08-13 says otherwise. The projection is not onto a convex
/// set, so what the loop finds is a local optimum of how far the springs were
/// let run before the anchor clamped them.
///
/// **Area here is one stated metric**, not a word: round every cell centre of
/// [`placed_cells`] to the nearest lattice column, count the columns spanned on
/// X and on Z inclusive, and multiply. Both reference circuits are started from
/// `planner::starting_layout(netlist, &PortPlacements::default())` -- rows and
/// barycentres -- with `Axes::IN_PLANE`, nothing
/// pinned, and `RelaxEffort::default()`. Re-measured on 2026-08-14, after
/// `settled` joined the convergence test -- an earlier draft of this table
/// named a metric it did not state and could not be reproduced from the tree.
///
/// It says `starting_layout` and not `plan_from_netlist` for a reason that
/// only became true in Task 10: `plan_from_netlist` now *is* a relaxation, so
/// starting from its anchors would relax an already-converged placement and
/// every row of this table would collapse toward the same small box, `k = 1024`
/// included. The numbers are unchanged by the switch, because until Task 10
/// `plan_from_netlist` returned exactly this layout.
///
/// | | and4 area | full_adder area |
/// |---|---|---|
/// | `k = 1`, `g = 2` | **1,125** (45x25) in 8 steps | **3,638** (34x107) in 9 steps |
/// | `k = 4`, `g = 2` | 2,610 (58x45) in 7 | 7,140 (51x140) in 8 |
/// | `k = 1024`, `g = 2` | 4,221 (63x67) in 2 | 10,595 (65x163) in 2 |
/// | `k = 1`, `g = 64` | 2,028 (52x39) in 3 | 5,980 (46x130) in 3 |
///
/// At `k = 1024` what comes back is the layout that went in. The starting
/// layout measures 63x67 for and4 and 64x163 for full_adder in that same
/// metric, so and4's 63x67 is its input to the column and full_adder's 65x163
/// is one column wider on X -- the facing sweep still runs. The anchor pins the
/// solve to `x_legal` on the first step and the loop terminates on what it was
/// handed. So the temptation this comment exists to refuse is the obvious one:
/// a circuit is slow, raise the anchor, it converges in two steps and has
/// placed nothing.
///
/// `k = 1, g = 2` is the best-quality corner of that sweep and already
/// converges in single-digit steps. Raising `RelaxEffort::iterations` instead
/// costs nothing and changes nothing: at every corner above, budgets of 256,
/// 1024, 4096 and 16384 converge at the same step with the same box. That is
/// safe at any budget only because [`ANCHOR_CEILING`] stops the doubling --
/// without it a budget past 1024 is where the anchor overflows to `+inf`.
pub const ANCHOR_GROWTH: f64 = 2.0;

/// Where the doubling stops.
///
/// A cap rather than a documented bound, because the failure it prevents is
/// silent. `anchor` is an `f64` doubling from [`ANCHOR_STIFFNESS`], so it is
/// `+inf` from step 1025 -- and `+inf` is absorbed rather than refused at every
/// stage after that. [`Factorisation::of`] rejects only a pivot `<= 0.0`, and
/// `inf` is not; every pivot becomes `sqrt(inf) = inf`; the back-substitution
/// divides `inf` by `inf` and every free body's position becomes `NaN`. `NaN`
/// then passes each of `relax`'s three exit conditions rather than tripping
/// them: `choose_facings` never beats its `f64::INFINITY` seed because
/// `NaN < INFINITY` is false, so nothing turns; `project` finds no violation
/// because every comparison against `NaN` is false; and `fold(0.0, f64::max)`
/// ignores `NaN` by contract, so `gap` and `settled` both fold to 0.0. The call
/// would return `Ok` with `converged: true` and a placement of `NaN`s.
///
/// Reachable only through [`RelaxEffort::iterations`], which is a `pub` field
/// this file's own comments tell the reader to raise. The default of 256 does
/// not get near it.
///
/// Nothing measured here is lost by stopping. The anchor reaches `2^60` at step
/// 61, and both reference circuits converge in single digits: the budget sweep
/// in [`ANCHOR_GROWTH`] was re-run at 256, 1024, 4096 and 16384 with the cap in
/// place and every corner gave the same step and the same box. Past that point
/// the cap is not a compromise either -- once the anchor is `2^53` times a
/// body's incident stiffness the solve's answer differs from `legal` by less
/// than an ULP, so doubling further changes nothing but the exponent. A power
/// of two, so the multiply and the comparison stay exact.
pub const ANCHOR_CEILING: f64 = (1u64 << 60) as f64;

/// How hard to try, and from where.
///
/// The seed has one job: retrying a stuck configuration from a slightly
/// different one, reproducibly. It is *not* what breaks the planar symmetry --
/// upward separation does that, in Stage 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelaxEffort {
    pub iterations: usize,
    pub seed: u64,
}

impl Default for RelaxEffort {
    fn default() -> Self {
        RelaxEffort { iterations: 256, seed: 0 }
    }
}

/// A relaxed placement, in continuous space, before anything is rounded.
#[derive(Debug, Clone)]
pub struct ContinuousPlacement {
    pub graph: BodyGraph,
    /// Whether the last step met all three of [`relax`]'s exit conditions: the
    /// solve and the projection agreed to within [`CONVERGED`], the step moved
    /// the legal configuration less than [`CONVERGED`], and no body turned.
    ///
    /// [`relax`] returns this `true` or does not return at all -- a run that
    /// does not converge is [`RelaxError::DidNotConverge`], never an `Ok`
    /// carrying `false`. The field is here for the consumer, not the producer:
    /// [`snap`] refuses an unconverged placement, because rounding is exact
    /// only if the projection converged and one that did not has no margin to
    /// spend. [`snap`] is the one reader in the tree, and since nothing
    /// produces a `false` the placement that exercises that refusal is
    /// hand-built -- `an_unconverged_placement_is_refused_rather_than_rounded`,
    /// in `snap.rs`.
    pub converged: bool,
    pub iterations: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelaxError {
    /// The relaxation never finished.
    ///
    /// Not "no progress, and a violation still standing", which is what the
    /// sibling below is for. This one has **two producers**, and `worst` means
    /// a different thing in each -- so a caller may not assume either.
    ///
    /// From [`relax`], `worst` is the `{0, 0, 0.0}` placeholder: the budget ran
    /// out with every pair legal and the two bounds still apart. `relax`
    /// returns [`RelaxError::Deadlocked`] the moment `project` errs, so it can
    /// only fall through to its own budget check after a projection that
    /// returned `Ok` -- and under `Axes::IN_PLANE` an `Ok` means every pair's
    /// shortfall is at or under [`SETTLED`], which is the same threshold
    /// [`worst_violation`] tests. There is no pair left to name.
    ///
    /// From [`snap`], it usually names a real one. [`snap`] refuses any
    /// placement whose `converged` is false and measures it as it was handed
    /// it -- and a placement that never converged is exactly where a violation
    /// can still be standing.
    /// `an_unconverged_placement_is_refused_rather_than_rounded` builds one and
    /// asserts the pair is named and the shortfall positive.
    ///
    /// The `Display` arm below therefore branches on `worst.shortfall` rather
    /// than on which producer raised it: prose for the placeholder, a measured
    /// pair otherwise.
    DidNotConverge { iterations: usize, worst: Violation },
    /// No progress, and a violation still standing. A different error because
    /// the remedy differs: constraints that contradict, not a budget that ran
    /// out.
    Deadlocked { worst: Violation },
    /// The factorisation found no positive pivot.
    ///
    /// Not the unpinned-component case, which cannot arise: the anchor on the
    /// diagonal makes `A + anchor * I` strictly diagonally dominant, so it is
    /// positive definite whether or not anything is pinned. Nor a pull whose
    /// two ends are the same body: `laplacian`'s free-free arm then has
    /// `i == j`, so its four writes all land on the one diagonal cell as
    /// `+k, +k, -k, -k` and cancel, and the right-hand side cancels the same
    /// way. A self-pull is dropped silently rather than refused -- which is
    /// worth knowing, because `signal_pulls` emits one for a gate that lists
    /// its own output as an input.
    ///
    /// What is left is a negative stiffness, deep enough to overcome the
    /// anchor: a bug in how the graph was built rather than a property of the
    /// circuit. Nothing in the tree reaches it. [`build`] is the only producer
    /// of a [`Pull`] and it always writes [`SIGNAL_STIFFNESS`], which is `1.0`;
    /// only a hand-built [`BodyGraph`] could get here.
    Unsolvable { component_row: usize },
    /// The netlist and its primitive graph do not agree well enough to build
    /// bodies from -- a gate with no primitive, a declared input with no
    /// lever.
    CannotBuild { reason: String },
    /// A violation survived rounding, so the margin argument is wrong.
    ///
    /// Reported here rather than left for an invariant, which would find it
    /// downstream and attribute it to routing.
    SurvivedSnap { worst: Violation },
}

impl std::fmt::Display for RelaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Which producer raised it decides whether there is a pair worth
            // printing -- `relax` reaches its budget check only after a
            // projection that returned `Ok` and so carries the placeholder,
            // while `snap` measures an unconverged placement and usually finds
            // a real pair. The shortfall is what tells the two apart here.
            // Rendering the placeholder anyway prints "bodies 0 and 0 are
            // 0.000 too close", which reads as a measurement of a real pair.
            RelaxError::DidNotConverge { iterations, worst } if worst.shortfall == 0.0 => write!(
                f,
                "relaxation did not converge in {iterations} iterations; every pair is legal, the springs and the lattice just never agreed"
            ),
            RelaxError::DidNotConverge { iterations, worst } => write!(
                f,
                "relaxation did not converge in {iterations} iterations; bodies {} and {} are {:.3} too close",
                worst.left, worst.right, worst.shortfall
            ),
            RelaxError::Deadlocked { worst } => write!(
                f,
                "projection deadlocked: bodies {} and {} cannot be {:.3} further apart and stay welded",
                worst.left, worst.right, worst.shortfall
            ),
            RelaxError::Unsolvable { component_row } => write!(
                f,
                "the spring system has no positive pivot at body {component_row}, which means the graph was built wrong"
            ),
            RelaxError::CannotBuild { reason } => {
                write!(f, "cannot build bodies for this netlist: {reason}")
            }
            RelaxError::SurvivedSnap { worst } => write!(
                f,
                "rounding left bodies {} and {} {:.3} too close: the snap margin is wrong",
                worst.left, worst.right, worst.shortfall
            ),
        }
    }
}

impl std::error::Error for RelaxError {}

/// The weighted Laplacian, with pinned bodies struck out and the step's anchor
/// on the diagonal.
///
/// Struck out rather than weighted heavily: a pinned body takes no force, so it
/// is not an unknown, and its position moves to the right-hand side. What makes
/// the result positive definite is the anchor rather than the striking-out,
/// which is the whole reason an unpinned netlist can be placed at all -- see
/// [`ANCHOR_STIFFNESS`]. A [`Factorisation`] that refuses one of these is
/// therefore reporting a graph built wrong, not a circuit free to translate,
/// and [`RelaxError::Unsolvable`] says so.
fn laplacian(graph: &BodyGraph, free: &[Option<usize>], order: usize, anchor: f64) -> Vec<f64> {
    let mut matrix = vec![0.0; order * order];
    // The anchor sits on the diagonal, which makes the matrix strictly
    // diagonally dominant and so positive definite whether or not anything is
    // pinned. That matters because `PortPlacements` defaults to empty and the
    // netlist-only path takes the default: today the viewer, through
    // `compile_planned(&netlist, &PortPlacements::default())` at
    // `viewer/src/lib.rs:862`, and `plan_from_netlist`'s own tests. (`compile()`
    // is not that caller yet -- it seeds the planner from the legacy emitter and
    // never builds a `PortPlacements` at all. Task 13 is where it switches.)
    // Without an anchor a component free to translate makes the system
    // singular, and the factorisation refuses it -- correctly, and uselessly.
    for slot in 0..order {
        matrix[slot * order + slot] += anchor;
    }
    for pull in &graph.pulls {
        let (left, right) = (pull.from.0, pull.to.0);
        match (free[left], free[right]) {
            (Some(i), Some(j)) => {
                matrix[i * order + i] += pull.stiffness;
                matrix[j * order + j] += pull.stiffness;
                matrix[i * order + j] -= pull.stiffness;
                matrix[j * order + i] -= pull.stiffness;
            }
            (Some(i), None) | (None, Some(i)) => {
                matrix[i * order + i] += pull.stiffness;
            }
            (None, None) => {}
        }
    }
    matrix
}

/// The vector from a pull's `from` attachment to its `to` attachment, as the
/// bodies currently stand.
///
/// This is the quantity every spring in this file is a function of, and it is
/// the *attachment* separation rather than the body separation: a socket and a
/// pin sit off their bodies' own positions by [`attach_offset`], which is how a
/// facing enters the energy at all.
fn separation(graph: &BodyGraph, pull: &Pull) -> [f64; 3] {
    let from = &graph.bodies[pull.from.0];
    let to = &graph.bodies[pull.to.0];
    let from_at = attach_offset(pull.from.1, from);
    let to_at = attach_offset(pull.to.1, to);
    std::array::from_fn(|axis| {
        (from.position[axis] + from_at[axis]) - (to.position[axis] + to_at[axis])
    })
}

/// What fraction of a separation the spring still wants gone.
///
/// `0.0` at or inside [`Pull::rest`] -- the spring is slack and exerts nothing
/// -- rising toward `1.0` as the separation grows past it. The norm is
/// **Manhattan**, because a route is rectilinear and the Manhattan distance
/// between two attachments is the least number of cells any path joining them
/// can spend; [`SIGNAL_REST_LENGTH`] is calibrated in that metric and says why.
///
/// Two things follow, and both matter to the loop below.
///
/// **It is continuous at the boundary.** At `norm == rest` this is exactly
/// zero and it approaches zero from above, so a pair crossing the free radius
/// does not step from "no force" to "some force" -- the force it carries fades
/// in. That is what a rest length has to do to be safe here: a discontinuous
/// one is a pair that can flip between pulled and free forever, which is the
/// oscillation this design was warned about. What is discontinuous is the
/// second derivative, and nothing in this file differentiates twice.
///
/// **At `rest == 0.0` it is `1.0` for every separation that is not exactly
/// zero, and `0.0` for one that is** -- and a separation of exactly zero
/// contributes nothing either way, because it is multiplied by itself below.
/// So a graph of zero-rest springs assembles the identical matrix and the
/// identical right-hand side it did before rest lengths existed. That is not a
/// coincidence to be grateful for; it is what makes
/// `a_zero_rest_length_is_the_solver_that_shipped_before_it` able to fail.
fn tension(separation: [f64; 3], rest: f64) -> f64 {
    let norm = separation[0].abs() + separation[1].abs() + separation[2].abs();
    if norm <= rest {
        0.0
    } else {
        (norm - rest) / norm
    }
}

/// The separation each pull is asking for on this step, linearised.
///
/// **This is where the non-linearity goes, and it is the whole design.** A rest
/// length makes the spring energy `k * max(0, |d| - rest)^2`, which is not
/// quadratic in position, so it cannot be assembled into a matrix and handed to
/// a Cholesky. What is quadratic is "hold the separation at a *fixed vector*
/// `t`" -- and the loop below already re-solves from scratch every step, so `t`
/// can be recomputed each time from the configuration the step starts in and
/// the solve stays linear. `t` is the current separation with the free part
/// left in and the rest taken out: `d * (1 - tension(d))`, which is `d` itself
/// while the pair is inside the free radius and shrinks toward the rest length
/// as it leaves.
///
/// Its fixed point is the fixed point of the true energy. A slack pull asks for
/// the separation it already has, so it contributes no force; a taut one asks
/// for `rest` along the direction it currently points, so it stops pulling
/// exactly when the pair is `rest` apart. Neither is an approximation of the
/// answer -- only of the path taken to it.
///
/// **Frozen once per step, not recomputed per axis.** `settle` solves the three
/// axes in sequence and writes each solution back into the bodies as it goes,
/// so a target computed inside that loop would see X already moved while
/// solving Z, and the three axes would stop being independent solves of one
/// system. Every caller must compute this before the axis loop and hand the
/// same targets to all three.
fn pull_targets(graph: &BodyGraph) -> Vec<[f64; 3]> {
    graph
        .pulls
        .iter()
        .map(|pull| {
            let apart = separation(graph, pull);
            let slack = 1.0 - tension(apart, pull.rest);
            [apart[0] * slack, apart[1] * slack, apart[2] * slack]
        })
        .collect()
}

/// The right-hand side for one axis, given the current facings.
///
/// A pull wants `(x_i + off_i) - (x_j + off_j) == t`, so the port offsets, the
/// step's target separation and every pinned neighbour's position land here.
/// `anchor * legal` is the `c` term: every free body is also pulled toward
/// where the projection last put it, which is what makes the two bounds meet.
/// On the first step there is no "last put it" yet and `legal` is the layout
/// `relax` was handed, legal or not; from the first projection on it is the
/// projection's own output.
///
/// **The matrix is not a party to any of this.** [`pull_targets`] changes what
/// separation a spring asks for and never how stiffly it asks, so every
/// coefficient of [`laplacian`] is what it was -- which is what keeps the
/// system positive definite for exactly the reason its own doc gives, and what
/// lets one factorisation still serve all three axes.
fn right_hand_side(
    graph: &BodyGraph,
    free: &[Option<usize>],
    order: usize,
    axis: usize,
    anchor: f64,
    legal: &[[f64; 3]],
    targets: &[[f64; 3]],
) -> Vec<f64> {
    // A `zip` shorter than the pulls would drop the tail of them from the
    // right-hand side while [`laplacian`] kept every one of them on the
    // diagonal -- a system asking the last few springs for a separation of
    // nothing, which is not a failure any assertion downstream would catch.
    assert_eq!(
        targets.len(),
        graph.pulls.len(),
        "the target table is indexed by pull"
    );
    let mut rhs = vec![0.0; order];
    for (index, slot) in free.iter().enumerate() {
        if let Some(slot) = slot {
            rhs[*slot] += anchor * legal[index][axis];
        }
    }
    for (pull, target) in graph.pulls.iter().zip(targets) {
        let (left, right) = (pull.from.0, pull.to.0);
        let left_offset = attach_offset(pull.from.1, &graph.bodies[left])[axis];
        let right_offset = attach_offset(pull.to.1, &graph.bodies[right])[axis];
        let want = right_offset - left_offset + target[axis];

        if let Some(i) = free[left] {
            rhs[i] += pull.stiffness * want;
            if free[right].is_none() {
                rhs[i] += pull.stiffness * graph.bodies[right].position[axis];
            }
        }
        if let Some(j) = free[right] {
            rhs[j] -= pull.stiffness * want;
            if free[left].is_none() {
                rhs[j] += pull.stiffness * graph.bodies[left].position[axis];
            }
        }
    }
    rhs
}

/// Each body's best facing for the current positions, found by trying all
/// four.
///
/// Not a rotation integrated over time: an enumeration, because there are
/// four. Ties go to the lowest index so the same input always turns the same
/// way.
///
/// Returns whether anything turned, because a step that changed no facing and
/// moved nothing is a converged step.
fn choose_facings(graph: &mut BodyGraph) -> bool {
    let mut turned = false;
    for body in 0..graph.bodies.len() {
        // Every body, pinned ones included. `PortPlacements` fixes where a
        // port sits, not which way its cell is built -- and a pinned output
        // whose route has to leave the wrong face is exactly the case a
        // facing exists to fix.
        // Recorded before the trials, because the loop leaves the *last*
        // facing tried in the body -- comparing against that would report a
        // turn on almost every body on almost every step, and the relaxation
        // would never satisfy its convergence test.
        let was = graph.bodies[body].facing;
        let mut best = (was, f64::INFINITY);
        for index in 0..4u8 {
            let facing = crate::compile::geometry::CellFacing::from_index(index)
                .expect("0..4 is horizontal");
            graph.bodies[body].facing = facing;
            let energy = incident_energy(graph, body);
            if energy < best.1 {
                best = (facing, energy);
            }
        }
        graph.bodies[body].facing = best.0;
        if best.0 != was {
            turned = true;
        }
    }
    turned
}

/// The spring energy of every pull touching `body`, with everything else held.
///
/// **The free part of a separation carries no energy**, which is what makes a
/// facing sweep agree with the solve it sits between rather than optimising a
/// second, older objective. Only [`tension`] of each separation is squared: a
/// pull whose two ends are already inside its rest length contributes exactly
/// `0.0` whichever way its body faces, so a body all of whose pulls are slack
/// has four equal energies and `choose_facings` leaves it where its
/// lowest-index tie-break puts it. That is a real consequence and not a
/// rounding one -- the four energies are the same `0.0`, bit for bit, on every
/// toolchain, which is a good deal *safer* than the near-ties
/// `measure_snapped_fingerprint_slack` exists to watch.
///
/// At `rest == 0.0` the tension is `1.0` and this is the plain sum of squared
/// separations it has always been.
fn incident_energy(graph: &BodyGraph, body: usize) -> f64 {
    let mut energy = 0.0;
    for pull in &graph.pulls {
        if pull.from.0 != body && pull.to.0 != body {
            continue;
        }
        let apart = separation(graph, pull);
        let taut = tension(apart, pull.rest);
        let mut squared = 0.0;
        for gap in &apart {
            let delta = gap * taut;
            squared += delta * delta;
        }
        energy += pull.stiffness * squared;
    }
    energy
}

/// Solve, turn, project, pull the anchor tighter. Repeat until three things
/// are true at once: the solve and the projection agree, the legal
/// configuration has stopped moving, and no body turned.
///
/// The middle one is not decoration. An earlier draft exited on the first
/// alone, and it does not measure settling: the projection moves nothing
/// whenever no pair is violating, so a slack constraint set makes that gap
/// exactly zero however far the layout still is from where the springs want
/// it. Measured on 2026-08-13 -- `and4`'s first step already had a gap of
/// 0.0000, and seven further steps took its extent from 54x39 to 45x25. The
/// loop below says which condition does what.
pub fn relax(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    start: &[Anchor],
    pinned: &PortPlacements,
    axes: Axes,
    effort: RelaxEffort,
) -> Result<ContinuousPlacement, RelaxError> {
    let mut bodies = build::build(netlist, graph, start, pinned)
        .map_err(|reason| RelaxError::CannotBuild { reason })?;
    perturb(&mut bodies, effort.seed);
    let required = project::required_separations(&bodies);
    settle(bodies, &required, axes, effort)
}

/// [`relax`], with every signal spring's rest length overridden.
///
/// The measurement seam [`SIGNAL_REST_LENGTH`] was chosen through, and the only
/// way to ask this solver what a *different* free radius would have produced
/// without recompiling the constant. Four probes go through it, and between
/// them they are why that constant is zero:
/// `planner::sweep_the_signal_rest_length`,
/// `planner::is_a_rest_lengths_delay_a_property_or_a_coincidence`,
/// `planner::where_the_extra_repeater_comes_from` and
/// `planner::does_more_room_let_the_negotiated_router_thread_segment_a`.
///
/// `#[cfg(test)]`, for the same reason [`relax_with_required`] below is: which
/// rest length ships is this design's decision and not a caller's. Passing
/// [`SIGNAL_REST_LENGTH`] here is [`relax`] exactly, and
/// `the_rest_length_seam_at_the_shipped_value_is_the_shipping_placer` holds it
/// to that on both reference circuits.
#[cfg(test)]
pub(crate) fn relax_with_rest(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    start: &[Anchor],
    pinned: &PortPlacements,
    axes: Axes,
    effort: RelaxEffort,
    rest: f64,
) -> Result<ContinuousPlacement, RelaxError> {
    let mut bodies = build::build(netlist, graph, start, pinned)
        .map_err(|reason| RelaxError::CannotBuild { reason })?;
    for pull in &mut bodies.pulls {
        pull.rest = rest;
    }
    perturb(&mut bodies, effort.seed);
    let required = project::required_separations(&bodies);
    settle(bodies, &required, axes, effort)
}

/// [`relax`], with the separation table handed in rather than derived.
///
/// This exists for one probe and is not a second way to place a circuit. The
/// §5.7 experiment of `docs/superpowers/specs/2026-08-15-routing-at-scale.md`
/// asks whether *congestion-driven* placement routes `segment_a`: trial-route,
/// measure where the router fought, inflate the bodies in the hot regions, and
/// relax again. Inflating an entry of the per-body requirement is the whole of
/// the intervention -- [`required_separations`] already returns a `Vec<f64>` and
/// [`project`] already takes one as a parameter -- so the only thing missing was
/// a way in.
///
/// `#[cfg(test)]`, so nothing on the shipping path can reach it: [`relax`] above
/// is the same function with `required_separations(&bodies)` in place of the
/// argument, and it is the only caller `plan_from_netlist` has.
///
/// The probe is `planner::measure_whether_congestion_driven_placement_routes_
/// segment_a`.
#[cfg(test)]
pub(crate) fn relax_with_required(
    netlist: &Netlist,
    graph: &PrimitiveGraph,
    start: &[Anchor],
    pinned: &PortPlacements,
    axes: Axes,
    effort: RelaxEffort,
    required: &[f64],
) -> Result<ContinuousPlacement, RelaxError> {
    let mut bodies = build::build(netlist, graph, start, pinned)
        .map_err(|reason| RelaxError::CannotBuild { reason })?;
    perturb(&mut bodies, effort.seed);
    assert_eq!(
        required.len(),
        bodies.bodies.len(),
        "the requirement table is indexed by body"
    );
    settle(bodies, required, axes, effort)
}

/// Solve, turn, project, tighten -- the loop [`relax`]'s own doc describes.
///
/// Split from [`relax`] so the separation table can be a parameter rather than
/// a local. Nothing else moved: the body of this function is `relax`'s from the
/// `free` sweep down, and the four bit-for-bit placement fixtures in
/// `viewer/tests/placement_agrees_with_native.rs` are what holds it to that.
fn settle(
    mut bodies: BodyGraph,
    required: &[f64],
    axes: Axes,
    effort: RelaxEffort,
) -> Result<ContinuousPlacement, RelaxError> {
    let mut free = vec![None; bodies.bodies.len()];
    let mut order = 0;
    for (index, body) in bodies.bodies.iter().enumerate() {
        if !body.pinned {
            free[index] = Some(order);
            order += 1;
        }
    }

    // The upper bound: the thing the anchor pulls toward, and from the end of
    // the first step a configuration that is legal.
    //
    // Not on entry. It starts as the layout `relax` was handed, and nothing
    // checks that: `build` seeds each body from `start` or from its pin without
    // consulting `required_separations`, and `perturb` then moves bodies by up
    // to a quarter cell. The counterexample is in this file --
    // `running_out_of_iterations_is_an_error_that_says_how_many_it_had` hands
    // `relax` three coincident anchors against a requirement of at least three
    // cells. Nothing rests on it: the seed enters only as the soft
    // `anchor * legal` term of step one's right-hand side, and `project` below
    // replaces it before anything is measured against it. What leaves the loop
    // is always projected.
    let mut legal: Vec<[f64; 3]> = bodies.bodies.iter().map(|body| body.position).collect();
    let mut anchor = ANCHOR_STIFFNESS;

    for iteration in 1..=effort.iterations {
        // Refactorised every step, because the anchor is on the diagonal and
        // the anchor grows. One factorisation serves all three axes -- the
        // anchor is the same for each -- so this is one `O(n^3/3)` per step
        // against three `O(n^2)` solves, which at a couple of hundred bodies
        // is not the cost worth optimising.
        let factorisation = Factorisation::of(&laplacian(&bodies, &free, order, anchor), order)
            .map_err(|error| RelaxError::Unsolvable { component_row: error.row })?;

        // What each spring asks for this step, linearised about the
        // configuration the step starts in -- which is the last projection's
        // output, and on step one the layout `relax` was handed.
        //
        // **Before the axis loop, and this is load-bearing.** The loop writes
        // each axis's solution straight back into the bodies, so a target
        // computed inside it would read an X that this same step had already
        // moved while assembling Z. The three solves would then be three
        // different systems rather than three axes of one, and the answer would
        // depend on the order `axes.iter()` happens to yield. [`pull_targets`]
        // says the same thing from the other end.
        let targets = pull_targets(&bodies);

        // The lower bound: where the springs want the bodies, given how hard
        // they are currently held to the last legal configuration.
        //
        // Only the axes this stage may move on. An earlier draft solved all
        // three and restricted only the projection, which does not hold a body
        // on its storey: a signal spring's rest length is a Manhattan radius
        // and Y counts toward it, so the Y solve pulls a body that is too far
        // from its neighbours down onto their plane and the storeys a pinned
        // anchor put there collapse. That was true when the springs had no rest
        // length at all -- then *every* pull did it -- and a rest length
        // narrows the case without closing it.
        for axis in axes.iter() {
            let mut rhs = right_hand_side(&bodies, &free, order, axis, anchor, &legal, &targets);
            factorisation.solve(&mut rhs);
            for (index, slot) in free.iter().enumerate() {
                if let Some(slot) = slot {
                    bodies.bodies[index].position[axis] = rhs[*slot];
                }
            }
        }

        let solved: Vec<[f64; 3]> = bodies.bodies.iter().map(|body| body.position).collect();

        let turned = choose_facings(&mut bodies);

        if let Err(worst) = project::project(&mut bodies, required, axes) {
            return Err(RelaxError::Deadlocked { worst });
        }
        let previous = std::mem::replace(
            &mut legal,
            bodies.bodies.iter().map(|body| body.position).collect(),
        );

        // Three conditions, and each one rules out a way of stopping early
        // that the other two allow.
        //
        // `gap` is the two bounds meeting: what the springs want and what is
        // legal have stopped disagreeing. It is emphatically *not* a settling
        // test and must not be read as one. `solved` and `legal` are the same
        // configuration either side of one `project` call, and `project` returns
        // having moved nothing whenever no pair's shortfall exceeds [`SETTLED`]
        // -- so an inactive constraint set makes `gap` exactly 0.0 however far
        // the solve just carried the bodies. What it measures is whether the
        // projection was idle.
        //
        // and4 is the demonstration, measured on 2026-08-14 from
        // `planner::starting_layout`'s anchors: its step-1 `gap` is 0.0000
        // while that same step moves a body 20.80 cells, and the seven further
        // steps take its bounding box from 54 by 39 to 45 by 25.
        //
        // `settled` is the settling test: how far this step moved the legal
        // configuration itself, `max |legal_new - legal_prev|`. It is the
        // quantity [`ContinuousPlacement::converged`] describes, and it is what
        // makes a return mean the relaxation finished rather than that the
        // constraints happened to be slack. Without it, the exit test on a graph
        // nothing crowds reduces to `!turned` alone -- a facing condition, and
        // `build` seeds every body `CellFacing::NORTH`, so it goes false as soon
        // as one sweep has re-oriented everything. That is step *two* on a chain
        // laid out north-to-south, and it stops with the two gates 30.09 cells
        // apart against the 24.78 the nine steps produce.
        // `a_slack_constraint_set_is_not_a_settled_one` is that graph, and both
        // of those numbers are measured off it.
        //
        // `!turned` is neither of those -- it is what makes `gap` a comparison
        // of like with like. `solved` is captured before `choose_facings`, so it
        // is the spring optimum for the *old* facings, while `legal` comes back
        // from a projection run against the *new* ones. A facing is an input to
        // both sides: `attach_offset` reads `body.facing` for a socket and for a
        // pin alike, so the right-hand side the solve minimised is not the one
        // that holds after a turn, and `project` separates cells the turn has
        // already moved. The subtraction is still well-defined arithmetic; what
        // is not well-defined across a turn is reading a small `gap` as "the
        // springs and the lattice agree", because the next step's solve targets
        // different offsets and lands somewhere else.
        //
        // It is kept for that reason and not for a step count: with `settled` in
        // the test it no longer changes either reference circuit's answer.
        // Measured 2026-08-14 by deleting it -- and4 still stops at step 8 with
        // a 45 by 25 box and full_adder at step 9 with 34 by 107, both
        // identical to the three-condition run.
        //
        // It cannot spin forever: a facing is an argmin over four with a
        // lowest-index tie-break, evaluated on positions that are themselves
        // converging, so once the positions settle the argmin settles with
        // them. And the anchor grows -- as far as [`ANCHOR_CEILING`], which
        // that constant's own doc argues is far past where every measured run
        // has already exited -- so the solve is pulled arbitrarily close to
        // `legal`, which is already legal, driving `gap` and `settled`
        // together to zero.
        let gap = solved
            .iter()
            .zip(&legal)
            .map(|(wanted, allowed)| {
                (0..3)
                    .map(|axis| (wanted[axis] - allowed[axis]).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max);

        let settled = previous
            .iter()
            .zip(&legal)
            .map(|(before, after)| {
                (0..3)
                    .map(|axis| (after[axis] - before[axis]).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max);

        if gap < CONVERGED && settled < CONVERGED && !turned {
            return Ok(ContinuousPlacement {
                graph: bodies,
                converged: true,
                iterations: iteration,
            });
        }

        anchor = (anchor * ANCHOR_GROWTH).min(ANCHOR_CEILING);
    }

    let worst = project::worst_violation(&bodies, required).unwrap_or(Violation {
        left: 0,
        right: 0,
        shortfall: 0.0,
    });
    Err(RelaxError::DidNotConverge {
        iterations: effort.iterations,
        worst,
    })
}

/// Nudge the start, reproducibly.
///
/// A stuck configuration can be retried from a slightly different one. Seed
/// zero is no perturbation at all, which is what every measurement in this
/// design is taken with.
fn perturb(graph: &mut BodyGraph, seed: u64) {
    if seed == 0 {
        return;
    }
    let mut state = seed;
    for body in &mut graph.bodies {
        if body.pinned {
            continue;
        }
        for axis in [0usize, 2] {
            // splitmix64, so a seed of one bit still moves every body.
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            // A quarter cell either way: enough to break a tie, not enough to
            // move a body past a neighbour.
            body.position[axis] += (z >> 11) as f64 / (1u64 << 53) as f64 * 0.5 - 0.25;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::geometry::CellFacing;
    use crate::compile::primitive_graph::expand;
    use crate::compile::topology::{Library, Primitive};
    use crate::compile::{Gate, Netlist};

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["b"])],
        }
    }

    /// Rows and barycentres, which is where every measurement in this file
    /// starts.
    ///
    /// **Not** `plan_from_netlist`'s anchors. Since Task 10 those *are* a
    /// relaxation's output, so relaxing from them measures how far a converged
    /// placement moves when relaxed again -- which is not what any doc comment
    /// here claims and not what the `k = 1024` corner of [`ANCHOR_GROWTH`]'s
    /// sweep would be caught by.
    fn starting_layout(netlist: &Netlist, pinned: &PortPlacements) -> Result<Vec<Anchor>, String> {
        crate::compile::planner::starting_layout(netlist, pinned)
            .map_err(|error| error.to_string())
    }

    fn relaxed(netlist: &Netlist, effort: RelaxEffort) -> ContinuousPlacement {
        let graph = expand(netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..netlist.gates.len() + netlist.inputs.len())
            .map(|index| Anchor { x: index as i32 * 20, y: 1, z: index as i32 * 16 })
            .collect();
        let mut placements = PortPlacements::default();
        placements.pin("a", start[netlist.gates.len()]);
        relax(netlist, &graph, &start, &placements, Axes::IN_PLANE, effort)
            .expect("a two-gate chain relaxes")
    }

    /// Same graph, same effort, identical placement -- bit for bit, not
    /// nearly. Every measurement taken downstream is noise otherwise.
    #[test]
    fn the_same_input_relaxes_to_the_same_bits() {
        let netlist = chain();
        let effort = RelaxEffort { iterations: 64, seed: 0x26_02 };
        let first = relaxed(&netlist, effort);
        let second = relaxed(&netlist, effort);

        for (index, (left, right)) in first.graph.bodies.iter().zip(&second.graph.bodies).enumerate()
        {
            assert_eq!(
                left.position.map(f64::to_bits),
                right.position.map(f64::to_bits),
                "body {index} landed somewhere else the second time"
            );
            assert_eq!(left.facing, right.facing, "body {index} turned differently");
        }
    }

    /// Torque produces orientation. A repeater whose only consumer sits east
    /// ends up driving its front eastwards -- stated as geometry rather than
    /// as a compass bearing, since "faces east" means different things for a
    /// wall torch and a repeater.
    ///
    /// Hand-built rather than taken from a circuit, because this is the claim
    /// the whole facing mechanism rests on and it has to be checkable by
    /// reading it.
    #[test]
    fn a_body_turns_to_face_what_pulls_it() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        // `b`'s only consumer, `c`, sits far to the east of it.
        let start = vec![
            Anchor { x: 0, y: 1, z: 0 },   // gate b
            Anchor { x: 60, y: 1, z: 0 },  // gate c
            Anchor { x: -20, y: 1, z: 0 }, // input a
        ];
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);
        placements.pin("c", start[1]);

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &placements,
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("relaxes");

        let b = placement.graph.anchor_body[0];
        assert_eq!(
            placement.graph.bodies[b].facing.direction(),
            crate::redstone::world::block::Facing::East,
            "b's output has to leave towards the only thing reading it"
        );
    }

    /// Nothing has to be pinned, and on the netlist-only path nothing is: the
    /// viewer calls `compile_planned(&netlist, &PortPlacements::default())`
    /// (`viewer/src/lib.rs:862`) and `plan_from_netlist`'s own tests do the
    /// same. `compile()` is not that caller today -- it seeds the planner from
    /// the legacy emitter -- and Task 13 is where it becomes one.
    ///
    /// Without the anchor this is the system the solver refuses before it
    /// starts: every component is free to translate, the Laplacian is singular,
    /// and `Factorisation::of` returns `NotPositiveDefinite` on the first flat
    /// pivot. With `ANCHOR_STIFFNESS` on the diagonal it is strictly diagonally
    /// dominant, so it factorises whatever the graph looks like -- which is why
    /// there is no longer any mechanism that goes looking for a component to
    /// hold still.
    #[test]
    fn a_netlist_with_nothing_pinned_still_relaxes() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("the anchor is what makes an unpinned system solvable");
        assert!(placement.converged, "it stopped without the two bounds meeting");
    }

    /// A relaxation that ran out of iterations says so rather than handing
    /// back something that looks placed, and says how many it was given.
    ///
    /// `worst` is wildcarded here on purpose, unlike in `snap`'s counterpart --
    /// which is also why this test is not named for the pair. `relax` reaches
    /// that line only after a projection that returned `Ok`, which means it
    /// left no violation, so `worst_violation` is `None` and the field is the
    /// placeholder. The pair worth naming is the one `snap` finds on an
    /// unconverged placement, and that is where the spec asks for it.
    #[test]
    fn running_out_of_iterations_is_an_error_that_says_how_many_it_had() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        // Every body on one anchor, so one step cannot possibly finish: the
        // solve collapses them, the projection pulls them apart by at least
        // two cells, and the gap between those two answers is what convergence
        // is measured on.
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let error = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort { iterations: 1, seed: 0 },
        )
        .expect_err("one iteration from a knot cannot converge");
        assert!(matches!(error, RelaxError::DidNotConverge { iterations: 1, .. }));
    }

    /// A slack constraint set is not a settled relaxation, and `gap` alone
    /// cannot tell them apart.
    ///
    /// This chain is laid out north-to-south with sixty cells between
    /// neighbours, so no pair is ever inside its separation requirement:
    /// `project` returns having moved nothing on every round, `legal` is
    /// bit-identical to `solved`, and `gap` is 0.0000 at *every* step. So the
    /// old two-condition test reduced here to `!turned` alone -- and `build`
    /// seeds every body [`crate::compile::geometry::CellFacing::NORTH`], which
    /// this layout makes the argmin from the first sweep on. It returned at
    /// step 2, two solves in, reporting `converged: true`.
    ///
    /// `settled` is what makes the loop keep going, and the numbers say by how
    /// much. Measured 2026-08-14: without it, 2 steps and 30.09 cells between
    /// `b` and `c`; with it, 9 steps and 24.78. Both bounds below are loose
    /// enough to leave that whole gap and still fail on the wrong side of it.
    #[test]
    fn a_slack_constraint_set_is_not_a_settled_one() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![
            Anchor { x: 0, y: 1, z: -60 },  // gate b
            Anchor { x: 0, y: 1, z: -120 }, // gate c
            Anchor { x: 0, y: 1, z: 0 },    // input a
        ];
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &placements,
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("a chain nothing crowds relaxes");

        assert!(
            placement.iterations >= 5,
            "it stopped after {} step(s): nothing was measuring whether it had settled",
            placement.iterations
        );
        let b = placement.graph.bodies[placement.graph.anchor_body[0]].position[2];
        let c = placement.graph.bodies[placement.graph.anchor_body[1]].position[2];
        assert!(
            (b - c).abs() < 27.0,
            "b and c are {:.3} apart; the springs had not finished pulling them together",
            (b - c).abs()
        );
    }

    /// A guardrail on the anchor schedule, not a golden number.
    ///
    /// [`ANCHOR_GROWTH`]'s doc argues that raising either anchor number
    /// converges sooner and places worse, and until this test that argument had
    /// nothing holding it: `ANCHOR_STIFFNESS = 1024.0` or `ANCHOR_GROWTH = 64.0`
    /// -- the corners the doc calls "converges in two steps and has placed
    /// nothing" -- passed every other assertion in the module.
    ///
    /// The ceiling is deliberately loose. and4 measures 1,125 at the shipped
    /// constants (2026-08-14, metric as [`ANCHOR_GROWTH`] states it), 2,028 at
    /// `g = 64`, 4,221 at `k = 1024` -- and 4,221 is also the starting layout's
    /// own box, so the `k = 1024` corner really has placed nothing. Anything
    /// under 1,500 is the shipped corner with room to move; an ordinary
    /// refactor that shifts a body or two will not trip it, and neither wrong
    /// anchor can get under it. The step floor catches `k = 1024` (2 steps)
    /// but *not* `g = 64`, which lands on exactly 3 -- the area assertion is
    /// what catches that corner.
    #[test]
    fn the_anchor_schedule_still_places_and4_small() {
        let (netlist, _) = crate::circuits::and4::build_and4_netlist();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = starting_layout(&netlist, &PortPlacements::default()).expect("lays out");

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("and4 relaxes");

        let cells = placed_cells(&placement.graph);
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for body in &cells {
            for cell in body {
                for axis in [0usize, 2] {
                    lo[axis] = lo[axis].min(cell.at[axis]);
                    hi[axis] = hi[axis].max(cell.at[axis]);
                }
            }
        }
        let width = (hi[0].round() - lo[0].round()) as i64 + 1;
        let depth = (hi[2].round() - lo[2].round()) as i64 + 1;
        assert!(
            width * depth < 1_500,
            "and4 came back {width} by {depth} = {}; the anchor schedule has stopped placing",
            width * depth
        );
        assert!(
            placement.iterations >= 3,
            "and4 converged in {} step(s), which is what an anchor too strong to move anything \
             looks like",
            placement.iterations
        );
    }

    /// [`Axes`] governs the solve as well as the projection.
    ///
    /// Gate `b` starts a storey above everything that pulls on it, and in-plane
    /// relaxation has to leave it there. Replace the solve's `axes.iter()` with
    /// `0..3` and this is the only test in the module that notices: every other
    /// fixture starts every body at `y = 1`, so the Y right-hand side is
    /// `anchor * 1.0` for every free body and the Y solve returns exactly the
    /// storey it was given. Here the zero-rest-length Y springs pull `b` down
    /// onto its neighbours' plane instead -- and `gap` cannot see it, because
    /// the in-plane projection never writes Y, so `solved` and `legal` agree on
    /// that axis bit for bit however wrong it is.
    ///
    /// Exact equality rather than a tolerance: under [`Axes::IN_PLANE`] nothing
    /// writes `position[1]` at all -- `separate` moves only the axis
    /// `cheapest_axis` chose, and `chain()` has no welds, so `satisfy` never
    /// runs -- so the value has to be the starting one bit for bit.
    #[test]
    fn a_body_stays_on_the_storey_it_started_on() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![
            Anchor { x: 0, y: 6, z: 0 },   // gate b, one storey up
            Anchor { x: 40, y: 1, z: 0 },  // gate c
            Anchor { x: -20, y: 1, z: 0 }, // input a
        ];

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("relaxes");

        for (node, anchor) in start.iter().enumerate() {
            let body = placement.graph.anchor_body[node];
            assert_eq!(
                placement.graph.bodies[body].position[1],
                f64::from(anchor.y),
                "node {node} left the storey its starting layout gave it"
            );
        }
    }

    /// The seed's one job, asserted. Seed zero changes nothing; a non-zero seed
    /// moves every unpinned body in plane and no pinned body at all, by less
    /// than the quarter cell that would carry one past a neighbour; and two
    /// seeds give two starts. Without this the whole of `perturb` could be
    /// emptied to its signature and the suite would not notice -- the only
    /// non-zero seed anywhere compares two runs at the *same* seed.
    #[test]
    fn a_seed_nudges_the_unpinned_in_plane_and_nothing_else() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);
        let built = || build::build(&netlist, &graph, &start, &placements).expect("builds");

        let mut untouched = built();
        perturb(&mut untouched, 0);
        for (index, (before, after)) in built().bodies.iter().zip(&untouched.bodies).enumerate() {
            assert_eq!(
                before.position.map(f64::to_bits),
                after.position.map(f64::to_bits),
                "seed zero moved body {index}"
            );
        }

        let mut nudged = built();
        perturb(&mut nudged, 0x26_02);
        for (index, (before, after)) in built().bodies.iter().zip(&nudged.bodies).enumerate() {
            assert_eq!(
                before.position[1], after.position[1],
                "body {index} left its storey"
            );
            if before.pinned {
                assert_eq!(
                    before.position.map(f64::to_bits),
                    after.position.map(f64::to_bits),
                    "pinned body {index} left its pin"
                );
                continue;
            }
            for axis in [0usize, 2] {
                let moved = (after.position[axis] - before.position[axis]).abs();
                assert!(moved > 0.0, "body {index} did not move on axis {axis}");
                assert!(moved < 0.25, "body {index} moved {moved} on axis {axis}");
            }
        }

        let mut other = built();
        perturb(&mut other, 0x26_03);
        assert_ne!(
            nudged.bodies[0].position.map(f64::to_bits),
            other.bodies[0].position.map(f64::to_bits),
            "two seeds have to give two starts"
        );
    }

    /// A rest length is a distance the spring stops pulling at, and this is
    /// what says so.
    ///
    /// [`pull_targets`] carries the whole of the non-linearity, so it is
    /// asserted directly rather than read off a settled placement -- a lone
    /// pull against the anchor converges to the anchor's residual and not to
    /// its rest length, which is a fact about [`ANCHOR_GROWTH`] and would make
    /// a fixture that asserted "8 apart" a test of the wrong thing. Four
    /// separations, each deciding something a wrong implementation gets wrong:
    ///
    /// - **diagonal, well outside the rest length.** 40 along X and 40 along
    ///   Z. The target has to have Manhattan norm exactly the rest length; a
    ///   Euclidean implementation gives it Manhattan norm `rest * sqrt(2)`, so
    ///   this is the case that decides the metric [`SIGNAL_REST_LENGTH`] is
    ///   calibrated in.
    /// - **inside the rest length.** The target has to be the separation
    ///   itself, which is the "no pull inside it" half of the design: the
    ///   right-hand side then asks for the separation the pair already has and
    ///   the spring contributes no force. Taking `max` where [`pull_targets`]
    ///   takes `min` -- the reading of "rest length" that also *pushes* -- is
    ///   what this catches.
    /// - **exactly at the rest length.** Both branches have to give the same
    ///   answer, which is the continuity the loop's convergence rests on.
    /// - **coincident.** No direction to point a target along; it has to be the
    ///   zero vector rather than a `NaN` out of a division by a zero norm.
    ///   `NaN` would not be caught downstream -- [`ANCHOR_CEILING`]'s doc walks
    ///   through how every one of `relax`'s exit conditions passes a `NaN`
    ///   rather than tripping on it.
    #[test]
    fn a_pull_stops_pulling_at_its_rest_length() {
        let graph = |at: [f64; 3], rest: f64| BodyGraph {
            bodies: vec![
                Body {
                    what: BodyKind::Primitive { node: 0, kind: Primitive::Torch },
                    position: [0.0, 0.0, 0.0],
                    inputs: Vec::new(),
                    output: Some("n".into()),
                    facing: CellFacing::NORTH,
                    pinned: true,
                },
                Body {
                    what: BodyKind::Primitive { node: 1, kind: Primitive::Torch },
                    position: at,
                    inputs: vec!["n".into()],
                    output: None,
                    facing: CellFacing::NORTH,
                    pinned: false,
                },
            ],
            // Attached at the two bodies' own cells, so the separation is the
            // positions and the arithmetic below is readable. `Attach::Pin`
            // would add `pin_hops` to it and test the same thing less clearly.
            pulls: vec![Pull {
                from: (0, Attach::Socket(0)),
                to: (1, Attach::Socket(0)),
                stiffness: SIGNAL_STIFFNESS,
                rest,
            }],
            welds: Vec::new(),
            nodes: vec![vec![0], vec![1]],
            anchor_body: vec![0, 1],
        };
        let manhattan = |vector: [f64; 3]| vector[0].abs() + vector[1].abs() + vector[2].abs();

        // Diagonal and far outside: the target points the same way and is
        // exactly `rest` long, by the Manhattan norm.
        for rest in [4.0f64, 8.0, 12.0] {
            let bodies = graph([-40.0, 0.0, -40.0], rest);
            let target = pull_targets(&bodies)[0];
            assert!(
                (manhattan(target) - rest).abs() < 1e-9,
                "rest {rest} on a diagonal pair asked for a target {:.6} long by Manhattan; a \
                 Euclidean rest length would make it {:.6}",
                manhattan(target),
                rest * 2.0f64.sqrt()
            );
            assert!(
                (target[0] - target[2]).abs() < 1e-9 && target[0] > 0.0,
                "the target has to point along the separation it shortened, not somewhere else: \
                 {target:?}"
            );
        }

        // Inside: the target is the separation itself, so the pull asks for
        // what it already has and exerts nothing.
        let bodies = graph([-3.0, 0.0, -4.0], 12.0);
        let apart = separation(&bodies, &bodies.pulls[0]);
        assert_eq!(
            pull_targets(&bodies)[0].map(f64::to_bits),
            apart.map(f64::to_bits),
            "a pull inside its rest length has to ask for the separation it already has"
        );
        // And it carries no energy either, which is the same claim made to
        // `choose_facings` rather than to the solve. A facing sweep run against
        // the old zero-rest energy would turn bodies to shorten a distance the
        // objective says is free -- two halves of one loop optimising two
        // different things.
        assert_eq!(
            incident_energy(&bodies, 1),
            0.0,
            "a body whose every pull is slack has to weigh all four facings the same"
        );
        assert!(
            incident_energy(&graph([-40.0, 0.0, -40.0], 12.0), 1) > 0.0,
            "a body whose pull is taut still has an energy to minimise"
        );

        // Exactly at it: the two branches have to agree, or the force steps
        // rather than fading in and a pair on the boundary can oscillate.
        let bodies = graph([-8.0, 0.0, -4.0], 12.0);
        let apart = separation(&bodies, &bodies.pulls[0]);
        assert_eq!(
            pull_targets(&bodies)[0].map(f64::to_bits),
            apart.map(f64::to_bits),
            "at exactly the rest length the pull is still slack"
        );

        // Coincident: no direction, and no division by a zero norm.
        let bodies = graph([0.0, 0.0, 0.0], 12.0);
        assert_eq!(
            pull_targets(&bodies)[0],
            [0.0, 0.0, 0.0],
            "a coincident pair has no direction to shorten along"
        );

        // And the whole of it reaches the solver: the same fixture settled at
        // three rest lengths comes back monotonically wider. The step is not
        // the rest length -- the anchor doubles faster than one pull can pull,
        // so what a lone pull converges to is the anchor's residual either way
        // -- but it moves, and it moves the right way.
        let settled_at = |rest: f64| -> f64 {
            let bodies = graph([40.0, 1.0, 40.0], rest);
            let required = vec![0.0; bodies.bodies.len()];
            let placement = settle(bodies, &required, Axes::IN_PLANE, RelaxEffort::default())
                .expect("one pull and one anchor always solve");
            manhattan(separation(&placement.graph, &placement.graph.pulls[0]))
        };
        let (tight, four, twelve) = (settled_at(0.0), settled_at(4.0), settled_at(12.0));
        assert!(
            twelve > four && four > tight,
            "settled separations {tight:.3} / {four:.3} / {twelve:.3} at rest 0 / 4 / 12 are not \
             increasing, so the target never reached the right-hand side"
        );
    }

    /// A rest length of zero is the solver that shipped before rest lengths,
    /// and this is what makes that claim falsifiable.
    ///
    /// It matters because [`SIGNAL_REST_LENGTH`] is zero: every pinned number
    /// on this branch rests on the rest-length machinery being inert at that
    /// value, and "inert" is a claim about `tension` returning `1.0` for every
    /// separation that is not exactly zero and about [`pull_targets`] returning
    /// the zero vector, not something the reader should take on trust.
    ///
    /// Two assertions, and they catch different mistakes.
    ///
    /// The first is on the constant itself, and it is what goes red the moment
    /// somebody gives `signal_pulls` a nonzero rest length: the pinned counts
    /// have to move with it, so this test is the wrong thing to silence and
    /// says so in its own message. Measured by injection -- setting
    /// [`SIGNAL_REST_LENGTH`] to 12.0 turns this one red along with two more in
    /// `relax::` and eighteen across the whole lib suite, and takes and4 from
    /// 232 blocks to 370.
    ///
    /// The second compares `relax` against `relax_with_rest(0.0)` bit for bit
    /// on `and4` and the two-gate chain. That is not a tautology while the
    /// constant is zero and the seam takes its own path: the seam overwrites
    /// every pull's rest *after* `build` and *before* `perturb`, and an
    /// argument that the overwrite cannot matter at zero is exactly the kind
    /// this file has been wrong about before.
    #[test]
    fn a_zero_rest_length_is_the_solver_that_shipped_before_it() {
        assert_eq!(
            SIGNAL_REST_LENGTH, 0.0,
            "this test is the reason the pinned counts did not move; if the shipped rest length \
             is no longer zero, they have to move and this test is the wrong one to silence"
        );

        for netlist in [chain(), crate::circuits::and4::build_and4_netlist().0] {
            let graph = expand(&netlist, &Library::default_library()).expect("expands");
            let start = starting_layout(&netlist, &PortPlacements::default()).expect("lays out");
            let shipped = relax(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                Axes::IN_PLANE,
                RelaxEffort::default(),
            )
            .expect("relaxes");
            let through_the_seam = relax_with_rest(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                Axes::IN_PLANE,
                RelaxEffort::default(),
                0.0,
            )
            .expect("relaxes");

            assert_eq!(shipped.iterations, through_the_seam.iterations);
            for (index, (left, right)) in shipped
                .graph
                .bodies
                .iter()
                .zip(&through_the_seam.graph.bodies)
                .enumerate()
            {
                assert_eq!(
                    left.position.map(f64::to_bits),
                    right.position.map(f64::to_bits),
                    "body {index} landed somewhere else through the seam"
                );
                assert_eq!(left.facing, right.facing, "body {index} turned a different way");
            }
        }
    }

    /// [`relax_with_rest`] at [`SIGNAL_REST_LENGTH`] is [`relax`].
    ///
    /// The seam is what four measurement harnesses call, and a seam that has
    /// drifted from the function it stands in for makes every number they print
    /// a number about nothing. Bit for bit on two circuits, and it is a
    /// different claim from the test above: that one fixes the rest length at
    /// zero, this one follows whatever [`SIGNAL_REST_LENGTH`] becomes.
    #[test]
    fn the_rest_length_seam_at_the_shipped_value_is_the_shipping_placer() {
        for netlist in [chain(), crate::circuits::and4::build_and4_netlist().0] {
            let graph = expand(&netlist, &Library::default_library()).expect("expands");
            let start = starting_layout(&netlist, &PortPlacements::default()).expect("lays out");
            let direct = relax(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                Axes::IN_PLANE,
                RelaxEffort::default(),
            )
            .expect("relaxes");
            let seam = relax_with_rest(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                Axes::IN_PLANE,
                RelaxEffort::default(),
                SIGNAL_REST_LENGTH,
            )
            .expect("relaxes");
            for (index, (left, right)) in
                direct.graph.bodies.iter().zip(&seam.graph.bodies).enumerate()
            {
                assert_eq!(
                    left.position.map(f64::to_bits),
                    right.position.map(f64::to_bits),
                    "body {index} landed somewhere else through the seam"
                );
            }
        }
    }

    /// The rest length is in the right-hand side and nowhere near the matrix.
    ///
    /// [`laplacian`]'s own doc argues the system is positive definite from the
    /// anchor on its diagonal, and [`RelaxError::Unsolvable`] argues from
    /// `SIGNAL_STIFFNESS` being the only stiffness anything writes. Both
    /// arguments survive a rest length only because a rest length changes what
    /// a spring asks for and never how stiffly it asks. That is easy to say and
    /// easy to break -- scaling the stiffness by `tension` is the obvious
    /// implementation and it is the wrong one -- so it is asserted: the same
    /// graph at four rest lengths assembles four bit-identical matrices.
    #[test]
    fn the_matrix_does_not_depend_on_the_rest_length() {
        let netlist = crate::circuits::and4::build_and4_netlist().0;
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = starting_layout(&netlist, &PortPlacements::default()).expect("lays out");
        let mut bodies =
            build(&netlist, &graph, &start, &PortPlacements::default()).expect("builds");
        let free: Vec<Option<usize>> = (0..bodies.bodies.len()).map(Some).collect();
        let order = bodies.bodies.len();

        let reference = laplacian(&bodies, &free, order, ANCHOR_STIFFNESS);
        for rest in [1.0f64, 8.0, 12.0, 1_000.0] {
            for pull in &mut bodies.pulls {
                pull.rest = rest;
            }
            let matrix = laplacian(&bodies, &free, order, ANCHOR_STIFFNESS);
            assert_eq!(
                matrix.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                reference.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
                "rest {rest} changed the matrix, so the positive-definiteness argument no longer \
                 holds and one factorisation no longer serves three axes"
            );
        }
    }

    /// A rest length converges, and a pair sitting on the free radius does not
    /// flip between pulled and free forever.
    ///
    /// This is the failure the design was warned about and the reason
    /// [`tension`] fades a pull in across its boundary rather than switching it
    /// on. `and4` and `full_adder` are relaxed at every radius the sweep used,
    /// and each has to converge -- `relax` returns `Err` if it does not -- well
    /// inside the 256-step budget. The bound is 32, which is over twice the
    /// worst step count any circuit reached in
    /// `planner::sweep_the_signal_rest_length` (12, `seven_segment` at rest
    /// 15) and far enough under the budget that an oscillation would have to
    /// be a slow one to hide.
    ///
    /// Raising the bound is the wrong repair if this goes red. What the sweep
    /// measured is that step counts *fall* as the rest length rises -- `and4`
    /// 8, 8, 8, 8, 8, 8, 7, 7 across radii 0 to 15 -- because a slack spring
    /// has less to argue with the projection about. A run that suddenly needs
    /// dozens of steps is the oscillation, not a budget that was too small.
    #[test]
    fn a_rest_length_converges_at_every_radius_the_sweep_used() {
        for (name, netlist) in [
            ("and4", crate::circuits::and4::build_and4_netlist().0),
            ("full_adder", crate::circuits::full_adder::build_full_adder_netlist().0),
        ] {
            let graph = expand(&netlist, &Library::default_library()).expect("expands");
            let start = starting_layout(&netlist, &PortPlacements::default()).expect("lays out");
            for rest in [0.0f64, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 15.0] {
                let placement = relax_with_rest(
                    &netlist,
                    &graph,
                    &start,
                    &PortPlacements::default(),
                    Axes::IN_PLANE,
                    RelaxEffort::default(),
                    rest,
                )
                .unwrap_or_else(|error| panic!("{name} at rest {rest} did not place: {error}"));
                assert!(
                    placement.iterations <= 32,
                    "{name} at rest {rest} took {} steps; the sweep's worst is 12, so this is an \
                     oscillation and not a budget",
                    placement.iterations
                );
            }
        }
    }

    /// The free radius really is free per edge, which is the premise the whole
    /// direction rests on and the one part of it the measurement confirmed.
    ///
    /// Two bodies on one net, relaxed at rest 0 and at rest 12, then routed by
    /// nothing -- the claim here is narrower than a route and does not need
    /// one. What it asserts is that the pull is what moved them: at rest 12 the
    /// separation is at least eight cells wider than at rest 0. If the springs
    /// were still pulling to coincidence past their rest length the two numbers
    /// would be equal, which is what this went red with before
    /// [`right_hand_side`] learned about [`pull_targets`].
    #[test]
    fn the_rest_length_is_what_widens_a_layout() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = starting_layout(&netlist, &PortPlacements::default()).expect("lays out");
        let width = |rest: f64| -> f64 {
            let placement = relax_with_rest(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
                Axes::IN_PLANE,
                RelaxEffort::default(),
                rest,
            )
            .expect("the chain places at every rest length");
            placement
                .graph
                .pulls
                .iter()
                .map(|pull| {
                    let apart = separation(&placement.graph, pull);
                    apart[0].abs() + apart[1].abs() + apart[2].abs()
                })
                .fold(0.0, f64::max)
        };
        let tight = width(0.0);
        let loose = width(12.0);
        assert!(
            loose - tight >= 8.0,
            "rest 12 left the widest pull {loose:.3} apart against {tight:.3} at rest 0; a rest \
             length that does not widen anything is not a rest length"
        );
    }

    /// The three axes are three solves of one system, and a rest length must
    /// not make them a chain.
    ///
    /// `settle` writes each axis's solution straight back into the bodies
    /// before solving the next, which is harmless while every spring's target
    /// is the constant zero and is not harmless once a target is a function of
    /// the current separation: computing it inside the axis loop would let the
    /// Z solve read an X this same step had already moved. `pull_targets` is
    /// therefore called once, above the loop, and this is what would notice if
    /// somebody moved it back down.
    ///
    /// **A symmetry, because a coupled solve cannot keep one.** Two repeaters
    /// attached at `PortKind::RepeaterRear`, which `physical.rs` declares at
    /// `ORIGIN` for all four facings -- so the attachment offsets are exactly
    /// zero and the fixture is exactly symmetric under exchanging X and Z. One
    /// is pinned at the origin, the other starts 40 along each axis with a rest
    /// length of 12. Whatever the springs and the anchor do to it, they have to
    /// do the same thing on both axes, to the bit. Solve X first and feed it
    /// into Z and they do not.
    ///
    /// It is a `rest = 12` fixture even though the shipped rest length is zero:
    /// at zero every target is the zero vector and the ordering cannot matter,
    /// so a test written at the shipped value would be vacuous -- which is what
    /// the first version of it was, and the injection that moved
    /// `pull_targets` into the loop passed the whole of `relax::` green.
    #[test]
    fn the_axes_are_solved_from_one_configuration_not_in_sequence() {
        use crate::compile::physical::PortKind;

        let repeater = |position: [f64; 3], net: &str, pinned: bool| Body {
            what: BodyKind::Primitive { node: 0, kind: Primitive::Repeater },
            position,
            inputs: if pinned { Vec::new() } else { vec![net.to_string()] },
            output: if pinned { Some(net.to_string()) } else { None },
            facing: CellFacing::NORTH,
            pinned,
        };
        let bodies = BodyGraph {
            bodies: vec![
                repeater([0.0, 0.0, 0.0], "n", true),
                repeater([40.0, 0.0, 40.0], "n", false),
            ],
            pulls: vec![Pull {
                from: (0, Attach::Port(PortKind::RepeaterRear)),
                to: (1, Attach::Port(PortKind::RepeaterRear)),
                stiffness: SIGNAL_STIFFNESS,
                rest: 12.0,
            }],
            welds: Vec::new(),
            nodes: vec![vec![0], vec![1]],
            anchor_body: vec![0, 1],
        };
        assert_eq!(
            attach_offset(Attach::Port(PortKind::RepeaterRear), &bodies.bodies[0]),
            [0.0, 0.0, 0.0],
            "the fixture's symmetry rests on this port sitting at the body's own cell"
        );

        let required = vec![0.0; bodies.bodies.len()];
        let placement = settle(bodies, &required, Axes::IN_PLANE, RelaxEffort::default())
            .expect("one pull and one anchor always solve");
        let apart = separation(&placement.graph, &placement.graph.pulls[0]);
        assert_eq!(
            apart[0].to_bits(),
            apart[2].to_bits(),
            "a symmetric fixture came back {apart:?}; the axes were solved in sequence, so the \
             answer depends on the order `Axes::iter` yields"
        );
    }
}
