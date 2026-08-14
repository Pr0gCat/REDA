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
    SIGNAL_STIFFNESS,
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

/// The right-hand side for one axis, given the current facings.
///
/// A pull wants `(x_i + off_i) - (x_j + off_j) == 0`, so the port offsets and
/// every pinned neighbour's position land here. `anchor * legal` is the `c`
/// term: every free body is also pulled toward where the projection last put
/// it, which is what makes the two bounds meet. On the first step there is no
/// "last put it" yet and `legal` is the layout `relax` was handed, legal or
/// not; from the first projection on it is the projection's own output.
fn right_hand_side(
    graph: &BodyGraph,
    free: &[Option<usize>],
    order: usize,
    axis: usize,
    anchor: f64,
    legal: &[[f64; 3]],
) -> Vec<f64> {
    let mut rhs = vec![0.0; order];
    for (index, slot) in free.iter().enumerate() {
        if let Some(slot) = slot {
            rhs[*slot] += anchor * legal[index][axis];
        }
    }
    for pull in &graph.pulls {
        let (left, right) = (pull.from.0, pull.to.0);
        let left_offset = attach_offset(pull.from.1, &graph.bodies[left])[axis];
        let right_offset = attach_offset(pull.to.1, &graph.bodies[right])[axis];
        let want = right_offset - left_offset;

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
fn incident_energy(graph: &BodyGraph, body: usize) -> f64 {
    let mut energy = 0.0;
    for pull in &graph.pulls {
        if pull.from.0 != body && pull.to.0 != body {
            continue;
        }
        let from = &graph.bodies[pull.from.0];
        let to = &graph.bodies[pull.to.0];
        let from_at = attach_offset(pull.from.1, from);
        let to_at = attach_offset(pull.to.1, to);
        let mut squared = 0.0;
        for axis in 0..3 {
            let delta = (from.position[axis] + from_at[axis]) - (to.position[axis] + to_at[axis]);
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

    let mut free = vec![None; bodies.bodies.len()];
    let mut order = 0;
    for (index, body) in bodies.bodies.iter().enumerate() {
        if !body.pinned {
            free[index] = Some(order);
            order += 1;
        }
    }

    let required = project::required_separations(&bodies);

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

        // The lower bound: where the springs want the bodies, given how hard
        // they are currently held to the last legal configuration.
        //
        // Only the axes this stage may move on. An earlier draft solved all
        // three and restricted only the projection, which does not hold a body
        // on its storey: springs have zero rest length, so the Y solve pulls
        // every unpinned body onto its neighbours' plane and the storeys a
        // pinned anchor put there collapse.
        for axis in axes.iter() {
            let mut rhs = right_hand_side(&bodies, &free, order, axis, anchor, &legal);
            factorisation.solve(&mut rhs);
            for (index, slot) in free.iter().enumerate() {
                if let Some(slot) = slot {
                    bodies.bodies[index].position[axis] = rhs[*slot];
                }
            }
        }

        let solved: Vec<[f64; 3]> = bodies.bodies.iter().map(|body| body.position).collect();

        let turned = choose_facings(&mut bodies);

        if let Err(worst) = project::project(&mut bodies, &required, axes) {
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

    let worst = project::worst_violation(&bodies, &required).unwrap_or(Violation {
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
    use crate::compile::primitive_graph::expand;
    use crate::compile::topology::Library;
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
}
