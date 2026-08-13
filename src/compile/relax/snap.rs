//! Rounding a relaxed placement onto the lattice, and refusing to when that
//! would be a lie.
//!
//! There is no facing to quantise here. The solver chose one of four at every
//! step, so what is left is rounding positions -- and one cell of margin
//! covers that, because rounding moves a body by at most half a cell, so two
//! bodies approach by at most one.

use crate::compile::geometry::CellFacing;
use crate::compile::planner::Anchor;
use crate::compile::relax::project::{required_separations, worst_violation};
use crate::compile::relax::{ContinuousPlacement, RelaxError, SNAP_MARGIN};

/// Where one of `PlanCandidate`'s nodes goes, and which way it is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnappedNode {
    /// Index into `PlanCandidate`'s nodes: gates first, then primary inputs.
    ///
    /// Not a `NodeId`. A bare merge's junction has no node -- `expand`
    /// produces no primitive for one -- so there would be nothing to name it
    /// by, and this is the order `emit_primitives` reads anyway.
    pub node: usize,
    pub anchor: Anchor,
    pub facing: CellFacing,
}

/// Round a converged placement onto the lattice.
pub fn snap(placement: &ContinuousPlacement) -> Result<Vec<SnappedNode>, RelaxError> {
    if !placement.converged {
        let required = required_separations(&placement.graph);
        let worst = worst_violation(&placement.graph, &required).unwrap_or(
            crate::compile::relax::Violation { left: 0, right: 0, shortfall: 0.0 },
        );
        return Err(RelaxError::DidNotConverge {
            iterations: placement.iterations,
            worst,
        });
    }

    let mut rounded = placement.graph.clone();
    for body in &mut rounded.bodies {
        for axis in 0..3 {
            body.position[axis] = body.position[axis].round();
        }
    }

    // The margin's claim, checked rather than trusted. The invariants exist to
    // catch real errors, not to catch the legaliser's leftovers.
    //
    // Without the margin, deliberately. `required_separations` is what the
    // *projection* enforces -- clearance, reservation, and one cell of margin
    // -- and springs pull while separation pushes, so wherever separation is
    // what stopped the springs a converged placement sits exactly at it, with
    // the margin already committed. The margin is what rounding is allowed to
    // consume; what has to survive rounding is the physical requirement
    // without it.
    //
    // Asking for it on both sides refuses placements the relaxation is
    // entitled to produce. Measured on 2026-08-14 by deleting the subtraction
    // below: `full_adder`, relaxed from `plan_from_netlist`'s anchors with
    // nothing pinned under `Axes::IN_PLANE` and converged in 9 steps, is
    // refused with bodies 20 and 21 exactly 0.500 short -- one rounding's worth
    // of a margin charged twice. `separation_survives_rounding_from_a_half_cell_boundary`
    // is that case in miniature and is the one test of the five that fails.
    // `and4` and this module's two-gate chain say nothing either way: neither
    // has a pair within its requirement even before rounding, which is why the
    // test that pins this down is hand-built.
    let required: Vec<f64> = required_separations(&rounded)
        .into_iter()
        .map(|separation| separation - SNAP_MARGIN)
        .collect();
    if let Some(worst) = worst_violation(&rounded, &required) {
        return Err(RelaxError::SurvivedSnap { worst });
    }

    Ok(rounded
        .anchor_body
        .iter()
        .enumerate()
        .map(|(node, &body)| SnappedNode {
            node,
            anchor: Anchor {
                x: rounded.bodies[body].position[0] as i32,
                y: rounded.bodies[body].position[1] as i32,
                z: rounded.bodies[body].position[2] as i32,
            },
            facing: rounded.bodies[body].facing,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::planner::PortPlacements;
    use crate::compile::primitive_graph::expand;
    use crate::compile::relax::{relax, Axes, RelaxEffort};
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["b"])],
        }
    }

    /// One answer per node `PlanCandidate` expects, in the order it expects
    /// them: gates, then primary inputs.
    #[test]
    fn snap_answers_once_per_candidate_node_in_candidate_order() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();
        let mut placements = PortPlacements::default();
        placements.pin("a", start[2]);

        let placement = relax(&netlist, &graph, &start, &placements, Axes::IN_PLANE, RelaxEffort::default())
            .expect("relaxes");
        let snapped = snap(&placement).expect("a converged placement rounds");

        assert_eq!(snapped.len(), 3);
        for (index, node) in snapped.iter().enumerate() {
            assert_eq!(node.node, index, "answers are not in candidate order");
        }
    }

    /// A pinned port comes back exactly where it was pinned. Not near it.
    #[test]
    fn a_pinned_port_snaps_to_where_it_was_pinned() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..3)
            .map(|index| Anchor { x: index * 20, y: 1, z: index * 16 })
            .collect();
        let pinned_at = Anchor { x: 37, y: 1, z: 41 };
        let mut placements = PortPlacements::default();
        placements.pin("a", pinned_at);

        let placement = relax(&netlist, &graph, &start, &placements, Axes::IN_PLANE, RelaxEffort::default())
            .expect("relaxes");
        let snapped = snap(&placement).expect("rounds");

        assert_eq!(snapped[2].anchor, pinned_at, "input `a` was pinned");
    }

    /// An unconverged placement is refused rather than rounded, and the error
    /// names the worst violation left standing. Both halves, because the spec
    /// asks for both: a refusal that reports the placeholder pair would satisfy
    /// a variant check and tell whoever reads it nothing.
    ///
    /// The margin it would spend is not there. Three nodes on one anchor
    /// overlap outright, so there is a real pair to name.
    #[test]
    fn an_unconverged_placement_is_refused_rather_than_rounded() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let placement = ContinuousPlacement {
            graph: crate::compile::relax::build::build(
                &netlist,
                &graph,
                &start,
                &PortPlacements::default(),
            )
            .expect("builds"),
            converged: false,
            iterations: 1,
        };

        let error = snap(&placement).expect_err("an unconverged placement has no margin");
        // By reference, so the `else` arm can still print the error it refused
        // to match.
        let RelaxError::DidNotConverge { worst, .. } = &error else {
            panic!("refused, but not as unconverged: {error}")
        };
        assert_ne!(worst.left, worst.right, "the error has to name a pair");
        assert!(worst.shortfall > 0.0, "and say how far short it fell");
    }

    /// Rounding moves a body by at most half a cell, so two approach by at
    /// most one -- which is what `SNAP_MARGIN` claims, and this is the case
    /// that tests it: a placement separated by *exactly* what the projection
    /// guarantees, with every body on a half-cell boundary in all three axes
    /// at once, which is where rounding moves one furthest.
    ///
    /// Exactly, not generously. A gap wider than the projection can produce
    /// tests nothing -- the margin would never be spent. What has to hold is
    /// that the worst case the projection *can* hand over still survives
    /// rounding, and the worst case is the equilibrium: springs pull and
    /// separation pushes, so a converged placement sits at the requirement.
    ///
    /// The requirement is between cells, and a NOR's input socket reaches one
    /// cell back toward the neighbour on that side, so the centre gap that
    /// puts the closest foreign cells exactly at the requirement is one more
    /// than the requirement itself.
    #[test]
    fn separation_survives_rounding_from_a_half_cell_boundary() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = crate::compile::relax::project::required_separations(&built);
        let gap = required[0].max(required[1]) + 1.0;
        built.bodies[0].position = [0.5, 1.5, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.5, 0.5];
        built.bodies[2].position = [0.5 + 2.0 * gap, 1.5, 0.5];

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        snap(&placement).expect("what the projection guarantees has to survive rounding");
    }

    /// And a placement one cell tighter than the projection can produce does
    /// not survive, which is what makes the test above a claim rather than a
    /// coincidence of a generous gap.
    #[test]
    fn a_placement_tighter_than_the_projection_allows_is_caught_after_rounding() {
        let netlist = chain();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 3];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = crate::compile::relax::project::required_separations(&built);
        // Two cells tighter: one gives back the margin rounding is allowed to
        // spend, and the second is a real violation.
        let gap = required[0].max(required[1]) + 1.0 - 2.0;
        built.bodies[0].position = [0.5, 1.5, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.5, 0.5];
        built.bodies[2].position = [0.5 + 2.0 * gap, 1.5, 0.5];

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        let error = snap(&placement).expect_err("this one is genuinely too tight");
        assert!(matches!(error, RelaxError::SurvivedSnap { .. }), "got {error}");
    }
}
