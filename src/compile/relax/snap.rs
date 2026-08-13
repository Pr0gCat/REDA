//! Rounding a relaxed placement onto the lattice, and refusing to when that
//! would be a lie.
//!
//! There is no facing to quantise here. The solver chose one of four at every
//! step, so what is left is rounding positions -- and one cell of margin
//! covers that, because rounding moves a body by at most half a cell, so two
//! bodies approach by at most one.
//!
//! **Horizontally.** [`required_separations`] adds [`SNAP_MARGIN`] to the
//! horizontal requirement and to nothing else; the vertical gate is a bare
//! `dy < CONDUCTOR_CLEARANCE` in `project::unseparated`, with no margin on it.
//! Stage 1 pays nothing for that, because `Axes::IN_PLANE` never writes a
//! fractional Y: every `dy` is an integer difference of starting storeys and
//! rounding is the identity on it. Under `Axes::ALL` it becomes live, and this
//! module is one of the two places it surfaces. The mechanism is narrower than
//! it looks, and worth stating precisely so Task 11 does not chase the wrong
//! one: a pair exempted at `dy = 2.0` can only round to `dy = 1.0` by losing
//! exactly one whole cell, which needs one body at `+0.5` and the other at
//! `-0.5` -- `f64::round` ties away from zero, so straddling `Y = 0` is the
//! only way two half-integers move apart rather than in unison. A sweep over
//! `[-3, 3]` in steps of 0.025 with cell offsets `-4..=4` found 72 such pairs,
//! and every one of them straddles zero, which no starting storey in this tree
//! produces. So the exposure is real but is the same coincidence the weld note
//! below calls "code for a coincidence". Task 11 is where the vertical
//! requirement grows its own margin, and the reason to give it one is that the
//! asymmetry exists at all -- not that a circuit has hit it.

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

    // Every body rounds on its own, and `f64::round` ties away from zero: a
    // body at `-0.5` lands on -1 while one at `0.5` lands on 1. So a welded
    // pair whose unit offset spans zero comes out two cells apart rather than
    // one -- rounding is not offset-preserving across the origin.
    //
    // Documented rather than handled, for two reasons that both have to hold.
    // It is reachable only at an exact `-0.5`: `satisfy` writes a welded
    // body's position as its anchor's plus an integer offset, so the two
    // always share a fractional part, and only the one fraction that is a tie
    // on both sides of zero splits them. And it is inconsequential if reached,
    // because nothing downstream reads the held body's position. The check
    // below exempts welded pairs (`project::exempt`), the collapse further
    // down answers with `anchor_body` -- the junction, never the repeater --
    // and `world_partition::resolve_node_position` re-derives the repeater's
    // cell from the junction's position and facing. Handling it would be code
    // for a coincidence, guarding an answer that is thrown away.
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
    // of a margin charged twice.
    //
    // `and4` and this module's two-gate chain say nothing either way, and it
    // is not because they are loose. A converged placement never has a pair
    // *inside* its requirement -- that is what `project` returning `Ok` means,
    // `full_adder` included -- so having none is no distinction at all. What
    // separates them is how much room is left over, and what rounding then
    // does to the pair that has least. Measured the same day, by inflating
    // every requirement by `delta` and asking `worst_violation`, in steps of
    // 0.001, for the smallest `delta` at which a pair appears: before
    // rounding, `and4`'s tightest pair and `full_adder`'s both sit within
    // 0.001 of their requirement, at the equilibrium this design is built
    // around. After rounding `and4`'s tightest pair lands exactly *on* its
    // requirement -- a rounded separation is an integer and a requirement is a
    // quarter-cell multiple, so a `delta` under 0.001 means zero -- and the
    // double charge survives that by nothing whatever, while `full_adder`'s 20
    // and 21 land 0.500 inside it. The chain this module still relaxes -- the
    // one in `a_pinned_port_snaps_to_where_it_was_pinned`, pinned at
    // (37, 1, 41) -- is the genuinely slack one: 1.850 before rounding and
    // 2.501 after, over 7 steps.
    //
    // That slack is a property of the pin rather than of the chain. The same
    // ladder unpinned fails the double charge by 0.500, at bodies 0 and 2: a
    // pinned body cannot be moved by the springs, so the pair it belongs to
    // settles wherever the pin left it rather than at the requirement.
    //
    // Two of this module's five tests fail under the double charge, and both
    // are tests built to spend the margin. The hand-built pair in
    // `a_pair_at_its_requirement_survives_rounding_toward_each_other` comes
    // out 0.750 short -- the largest fraction a requirement can carry, and
    // that test's own doc derives why. The isolated merge that
    // `snap_answers_once_per_candidate_node_in_candidate_order` relaxes comes
    // out 0.500 short between gate `na` and the junction, its tightest pair
    // having sat within 0.001 of its requirement before rounding. So the
    // relaxed half of this argument is held by a real converged circuit in
    // this module, and not only by `full_adder`, which nothing in the tree
    // runs end to end yet.
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
    use crate::compile::relax::{relax, Axes, RelaxEffort, SETTLED};
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["c".into()],
            gates: vec![Gate::nor("b", &["a"]), Gate::nor("c", &["b"])],
        }
    }

    /// A merge one of whose branches is *isolated*: `nb` feeds both the merge
    /// and `spy`, so that branch is shared rather than bare and `expand` gives
    /// it a repeater of its own. That repeater is a body, welded into the
    /// junction's socket, and it belongs to the merge's node rather than to a
    /// node of its own -- the only shape in the tree where a node owns more
    /// than one body.
    ///
    /// The same netlist as `build.rs`'s
    /// `an_isolated_branch_welds_its_repeater_into_the_junctions_socket`,
    /// where the weld this turns on is asserted.
    fn isolated_merge() -> Netlist {
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into(), "spy".into()],
            gates: vec![
                Gate::nor("na", &["a"]),
                Gate::nor("nb", &["b"]),
                Gate::merge("m", &["na", "nb"]),
                Gate::nor("out", &["m"]),
                Gate::nor("spy", &["nb"]),
            ],
        }
    }

    /// A two-input NOR that something reads: `m` has a routed degree of three,
    /// so its requirement is `CONDUCTOR_CLEARANCE + ROUTE_PITCH * 3 / 8 +
    /// SNAP_MARGIN` = 3.75 -- the largest quarter-cell fraction a requirement
    /// can carry, which is what makes it the worst case for rounding.
    fn wide() -> Netlist {
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["out".into()],
            gates: vec![Gate::nor("m", &["a", "b"]), Gate::nor("out", &["m"])],
        }
    }

    /// One answer per node `PlanCandidate` expects, in the order it expects
    /// them: gates, then primary inputs. Per *node*, which is the collapse,
    /// and the collapse is everything `snap` does beyond rounding.
    ///
    /// The fixture is the isolated merge for exactly that reason. On `chain()`
    /// and on `wide()` -- between them, what every other test in this module
    /// uses -- `bodies` and `anchor_body` are the same list in the same order, so a `snap` that
    /// mapped over `rounded.bodies` instead would pass every one of them. Here
    /// it returns eight answers for seven nodes, with the merge's welded
    /// repeater taking a node slot and every gate after it shifted by one.
    ///
    /// It is also, unplanned, the one relaxed circuit in this module that
    /// spends the rounding margin: its tightest pair -- gate `na` and the
    /// junction -- sits within 0.001 of its requirement before rounding and
    /// 0.500 inside it after, so the `expect` below is what fails first if the
    /// margin above is ever charged twice.
    #[test]
    fn snap_answers_once_per_candidate_node_in_candidate_order() {
        let netlist = isolated_merge();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start: Vec<Anchor> = (0..netlist.gates.len() + netlist.inputs.len())
            .map(|index| Anchor { x: index as i32 * 20, y: 1, z: index as i32 * 16 })
            .collect();

        let placement = relax(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
            Axes::IN_PLANE,
            RelaxEffort::default(),
        )
        .expect("relaxes");

        // Asserted rather than assumed: this fixture says nothing at all
        // unless the two counts differ, and a `chain()` here would be a test
        // that passes against the defect it names.
        assert_eq!(
            placement.graph.bodies.len(),
            8,
            "the isolated branch's welded repeater is what makes bodies outnumber nodes"
        );
        assert_eq!(placement.graph.anchor_body.len(), 7, "five gates and two inputs");

        let snapped = snap(&placement).expect("a converged placement rounds");

        assert_eq!(snapped.len(), 7, "one answer per candidate node, not per body");
        for (index, node) in snapped.iter().enumerate() {
            assert_eq!(node.node, index, "answers are not in candidate order");
            let anchor = &placement.graph.bodies[placement.graph.anchor_body[index]];
            assert_eq!(
                node.anchor,
                Anchor {
                    x: anchor.position[0].round() as i32,
                    y: anchor.position[1].round() as i32,
                    z: anchor.position[2].round() as i32,
                },
                "node {index} did not come back at its own anchor body"
            );
        }

        // And the body the collapse dropped is somewhere else, so "one per
        // node" is a choice rather than an accident of the two coinciding.
        let merge = 2;
        let junction = placement.graph.anchor_body[merge];
        assert_eq!(
            placement.graph.nodes[merge].len(),
            2,
            "the merge's node owns its junction and its branch repeater"
        );
        let repeater = *placement.graph.nodes[merge]
            .iter()
            .find(|&&body| body != junction)
            .expect("the other one");
        assert_ne!(
            placement.graph.bodies[repeater].position.map(f64::round),
            placement.graph.bodies[junction].position.map(f64::round),
            "the repeater sits in the junction's socket, a cell off it"
        );
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

    /// The worst case rounding can hand `snap`, built rather than described: a
    /// pair sitting exactly at its requirement, positioned so that rounding
    /// moves one body up and the other *down* and the two approach by 0.75 of
    /// a cell.
    ///
    /// Both halves are load-bearing.
    ///
    /// **Exactly at the requirement**, because a wider gap never spends the
    /// margin and so tests nothing. Springs pull and separation pushes, so
    /// wherever separation is what stopped the springs a converged placement
    /// sits at the requirement, and that is the tightest thing `project` can
    /// hand over. The two `worst_violation` assertions below are what make
    /// that a measurement rather than a comment: legal against the
    /// requirement, violating against a hair more.
    ///
    /// **Toward each other**, because `f64::round` ties away from zero. Two
    /// positive coordinates on half-cell boundaries therefore both round *up*,
    /// in unison, and the separation between them does not change at all --
    /// this fixture's Z is that case, both bodies at `z = 0.5` and both
    /// landing on 1, and it costs nothing. An earlier version of this test
    /// called "every body on a half-cell boundary in all three axes at once"
    /// the place "where rounding moves one furthest"; it is the place where
    /// rounding moves them the same way.
    ///
    /// **0.75 is the most that can be lost, and the arithmetic says why.**
    /// Rounding moves each body by at most half a cell, so a separation closes
    /// by at most one, and that bound alone is all [`SNAP_MARGIN`] needs to be
    /// sound. But rounding also leaves every coordinate an integer, so the
    /// separation afterwards is an integer -- while `required_separations` is
    /// `CONDUCTOR_CLEARANCE + ROUTE_PITCH * d / 8 + SNAP_MARGIN` = `3 + d/4`
    /// for a routed degree `d`. Whenever that fraction is not zero, the only
    /// integer in `[required - 1, required]` is `required - frac(required)`,
    /// so the loss is either nothing or exactly the fraction: at most 3/4, at
    /// `d = 3`. This fixture is that corner -- `m` reads both inputs and is
    /// read by `out` -- and nothing built on `required_separations` can lose
    /// more without `d` being a multiple of four, where the fraction is zero
    /// and the two reachable losses are nothing and a whole cell.
    ///
    /// The requirement is between *cells*, not centres: `m`'s socket for `b`
    /// reaches one cell east and `out`'s socket for `m` one cell west, so the
    /// centre gap that puts the closest foreign cells exactly at the
    /// requirement is two more than the requirement itself.
    #[test]
    fn a_pair_at_its_requirement_survives_rounding_toward_each_other() {
        let netlist = wide();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 4];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = required_separations(&built);
        assert_eq!(required[0], 3.75, "`m` is the degree-three body this fixture is for");
        let gap = required[0].max(required[1]) + 2.0;
        built.bodies[0].position = [0.5, 1.0, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.0, 0.5];
        // The two levers say nothing here and are parked out of reach, on
        // integers, where rounding is the identity.
        built.bodies[2].position = [0.0, 1.0, 60.0];
        built.bodies[3].position = [0.0, 1.0, 120.0];

        assert!(
            worst_violation(&built, &required).is_none(),
            "the fixture has to be legal before rounding, or it is not a placement `project` could hand over"
        );
        let a_hair: Vec<f64> = required.iter().map(|need| need + 2.0 * SETTLED).collect();
        let pinched = worst_violation(&built, &a_hair)
            .expect("and at the requirement with nothing to spare, or it tests nothing");
        assert_eq!((pinched.left, pinched.right), (0, 1), "the wrong pair is the tight one");

        let west = built.bodies[0].position[0];
        let east = built.bodies[1].position[0];
        assert!(west.round() > west, "the west body has to round up");
        assert!(east.round() < east, "and the east body down, or they do not approach at all");
        assert_eq!(
            (east - west) - (east.round() - west.round()),
            0.75,
            "the pair has to close by the whole fraction its requirement carries"
        );

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        snap(&placement).expect("what the projection guarantees has to survive rounding");
    }

    /// And a placement tighter than the projection can produce does not
    /// survive, which is what makes the test above a claim rather than a
    /// coincidence of a generous gap.
    #[test]
    fn a_placement_tighter_than_the_projection_allows_is_caught_after_rounding() {
        let netlist = wide();
        let graph = expand(&netlist, &Library::default_library()).expect("expands");
        let start = vec![Anchor { x: 0, y: 1, z: 0 }; 4];
        let mut built = crate::compile::relax::build::build(
            &netlist,
            &graph,
            &start,
            &PortPlacements::default(),
        )
        .expect("builds");

        let required = required_separations(&built);
        // Two cells tighter than the test above: the first is the margin
        // rounding is allowed to spend, and the second is a real violation.
        //
        // Not that one cell tighter would pass `snap`. The test above is built
        // to lose 0.75 to rounding, so at one cell tighter the shipped check
        // already reports 0.75 short. Two cells is what makes this a violation
        // whichever way rounding moves the pair.
        let gap = required[0].max(required[1]) + 2.0 - 2.0;
        built.bodies[0].position = [0.5, 1.0, 0.5];
        built.bodies[1].position = [0.5 + gap, 1.0, 0.5];
        built.bodies[2].position = [0.0, 1.0, 60.0];
        built.bodies[3].position = [0.0, 1.0, 120.0];

        let placement = ContinuousPlacement { graph: built, converged: true, iterations: 1 };
        let error = snap(&placement).expect_err("this one is genuinely too tight");
        assert!(matches!(error, RelaxError::SurvivedSnap { .. }), "got {error}");
    }
}

