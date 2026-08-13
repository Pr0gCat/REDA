//! Continuous placement: springs pull, the spacing rule pushes back, and what
//! comes out is rounded onto the lattice.
//!
//! See `docs/superpowers/specs/2026-08-13-spring-placement.md`.

mod build;
mod linear;
mod project;

// Re-exported rather than kept private: `relax` below reaches some of these,
// and nothing outside this directory names the module at all until `snap` and
// the planner do, in Tasks 9 and 10. A `pub` item in a private module that
// nobody reaches is `dead_code` -- an error under `check.sh`'s
// `cargo clippy --all-targets -- -D warnings`.
pub use build::{
    attach_offset, build, cells, pin_hops, Attach, Body, BodyGraph, BodyKind, Cell, Pull, Weld,
    SIGNAL_STIFFNESS,
};
pub use linear::{Factorisation, NotPositiveDefinite};
pub use project::{
    placed_cells, project, required_separations, reservation, worst_violation, Axes, PlacedCell,
    Violation, CONDUCTOR_CLEARANCE, PROJECTION_ROUNDS, ROUTE_PITCH, SETTLED, SNAP_MARGIN,
};

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
/// let run before the anchor clamped them:
///
/// | | and4 area | full_adder area |
/// |---|---|---|
/// | `k = 1`, `g = 2` | **1,035** in 7 steps | **3,465** in 9 steps |
/// | `k = 4`, `g = 2` | 2,773 in 2 | 6,950 in 6 |
/// | `k = 1024`, `g = 2` | 4,095 in 2 | 10,143 in 2 |
/// | `k = 1`, `g = 64` | 1,924 in 2 | 5,760 in 3 |
///
/// At `k = 1024` the returned area is the starting layout's exactly, for both
/// circuits: the anchor pins the solve to `x_legal` on the first step and the
/// loop terminates on what it was handed. So the temptation this comment
/// exists to refuse is the obvious one -- a circuit is slow, raise the anchor,
/// it converges in two steps and has placed nothing.
///
/// `k = 1, g = 2` is the best-quality corner of that sweep and already
/// converges in single-digit steps. Raising `RelaxEffort::iterations` instead
/// costs nothing and changes nothing: 256, 1024, 4096 and 16384 all converge
/// at the same step with an identical trace.
pub const ANCHOR_GROWTH: f64 = 2.0;

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
    /// Whether the last step moved every body less than [`CONVERGED`].
    ///
    /// `snap` refuses an unconverged placement: rounding is exact only if the
    /// projection converged, and one that did not has no margin to spend.
    pub converged: bool,
    pub iterations: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelaxError {
    /// The budget ran out with a violation still standing.
    DidNotConverge { iterations: usize, worst: Violation },
    /// No progress, and a violation still standing. A different error because
    /// the remedy differs: constraints that contradict, not a budget that ran
    /// out.
    Deadlocked { worst: Violation },
    /// The factorisation found no positive pivot.
    ///
    /// Not the unpinned-component case, which cannot arise: the anchor on the
    /// diagonal makes `A + anchor * I` strictly diagonally dominant, so it is
    /// positive definite whether or not anything is pinned. What is left is a
    /// stiffness that is not positive, or a pull whose two ends are the same
    /// body -- either a bug in how the graph was built rather than a property
    /// of the circuit.
    Unsolvable { component_row: usize },
    /// The netlist and its primitive graph do not agree well enough to build
    /// bodies from -- a gate with no primitive, a declared input with no
    /// lever.
    CannotBuild { reason: String },
}

impl std::fmt::Display for RelaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // `relax` raises this after a projection that returned `Ok`, so
            // there is usually no violating pair to name and `worst` is the
            // placeholder. Rendering it anyway prints "bodies 0 and 0 are
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
    // pinned. That matters because `PortPlacements` defaults to empty and
    // `compile()` passes the default: without an anchor a component free to
    // translate makes the system singular, and the factorisation refuses it --
    // correctly, and uselessly.
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
/// it, which is what makes the two bounds meet.
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

/// Solve, turn, project, pull the anchor tighter. Repeat until the solved and
/// the projected configuration stop disagreeing -- which is not the same as
/// nothing moving, and the loop below says why.
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

    // The upper bound: a configuration that is legal, and the thing the anchor
    // pulls toward. It starts as the starting layout, which is legal.
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
        // every unpinned body onto its neighbours' plane and the storeys
        // `Shape::Tall` laid out collapse.
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
        legal = bodies.bodies.iter().map(|body| body.position).collect();

        // Convergence is the two bounds meeting: what the springs want and
        // what is legal have stopped disagreeing. Not "nothing moved", which
        // is a different and weaker claim -- a system oscillating between two
        // configurations moves a lot every step and is going nowhere.
        //
        // And no body turned this step, which is the second condition and is
        // not a formality. Drop it and and4 returns at step *one*: the springs
        // and the lattice agree there already, on a configuration whose bodies
        // are still turning, and what comes back has a horizontal bounding box
        // of 54 by 39 against the 45 by 25 the seven steps produce. Measured on
        // 2026-08-13, by deleting `!turned` and relaxing both reference
        // circuits from their starting layouts; full_adder takes nine steps
        // either way.
        //
        // It cannot spin forever: a facing is an argmin over four with a
        // lowest-index tie-break, evaluated on positions that are themselves
        // converging, so once the positions settle the argmin settles with
        // them. In those same two runs the case this condition exists for -- a
        // gap already under [`CONVERGED`] with a body still turning -- arises
        // once for and4 and never for full_adder.
        let gap = solved
            .iter()
            .zip(&legal)
            .map(|(wanted, allowed)| {
                (0..3)
                    .map(|axis| (wanted[axis] - allowed[axis]).abs())
                    .fold(0.0, f64::max)
            })
            .fold(0.0, f64::max);

        if gap < CONVERGED && !turned {
            return Ok(ContinuousPlacement {
                graph: bodies,
                converged: true,
                iterations: iteration,
            });
        }

        anchor *= ANCHOR_GROWTH;
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

    /// Nothing has to be pinned. `compile()` passes
    /// `PortPlacements::default()`, so this is the case that actually ships.
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
}
