//! Parent Route Repack, pruning only: replace strength-redundant repeaters
//! on one parent-owned route with dust.
//!
//! The router refreshes a trunk retroactively when a later branch runs out
//! of strength (`routing.rs`, "refreshes the trunk first"), but never
//! revisits the refreshes earlier branches placed for themselves. Those are
//! the repeaters this pass removes. It runs inside `union_candidate` on the
//! renumbered parent clone BEFORE any child is stamped, so every cell it can
//! see is parent-owned by construction and no provenance is recorded.
//!
//! The source's real strength is never assumed: a repeater goes only when
//! each branch through it still has an earlier route-owned repeater, and the
//! walk restarts at full strength immediately after that repeater.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use crate::compile::fragment_synth::candidate::RealisedRouteTree;
use crate::compile::fragment_synth::identity::RouteId;
use crate::compile::fragment_synth::timing_graph::{RealisedTimingGraph, TimingArcKind};
use crate::compile::geometry::Anchor;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::BlockKind;

/// One Parent Route Repack choice: prune this parent route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(crate) struct ParentRouteChoice {
    pub route: RouteId,
}

/// Greedily turn every strength-redundant, non-terminal, route-owned
/// repeater of `tree` into dust, downstream first (greatest path index over
/// the branches through it, then anchor). Each attempt is proven against the
/// cells as they stand after earlier retained deletions, and a failed
/// attempt restores the exact prior state. Returns whether anything changed.
///
/// Delays are not refreshed here: the union's
/// `normalise_routes_and_connections` refreshes every tree it touches.
pub(crate) fn prune_route(tree: &mut RealisedRouteTree) -> bool {
    let terminals: BTreeSet<Anchor> = tree.branches.iter().map(|b| b.terminal.at).collect();
    let mut depth = BTreeMap::<Anchor, usize>::new();
    for branch in &tree.branches {
        for (index, at) in branch.path.iter().enumerate() {
            let deepest = depth.entry(*at).or_insert(0);
            *deepest = (*deepest).max(index);
        }
    }
    let mut candidates: Vec<(Reverse<usize>, Anchor)> = tree
        .cells
        .iter()
        .filter(|cell| cell.state.kind == BlockKind::Repeater && !terminals.contains(&cell.at))
        .filter_map(|cell| depth.get(&cell.at).map(|&d| (Reverse(d), cell.at)))
        .collect();
    candidates.sort();

    let mut changed = false;
    for (_, at) in candidates {
        let index = tree
            .cells
            .iter()
            .position(|cell| cell.at == at)
            .expect("candidates were collected from these cells");
        let retained = std::mem::replace(&mut tree.cells[index].state, crate::compile::dust());
        if branches_carry_through(tree, at) {
            changed = true;
        } else {
            tree.cells[index].state = retained;
        }
    }
    changed
}

/// Every branch through `at` still reaches its terminal: it has an earlier
/// route-owned repeater still standing, and from full strength right after
/// that repeater the current suffix (dust costs one, a repeater restores
/// full, anything else is not a conductor) never reaches zero.
///
/// The terminal cell may belong to whatever the route delivers into rather
/// than to `tree.cells` (a block lever, a gate landing); its recorded
/// `terminal.state` then stands in for it. Only the terminal gets that
/// fallback: the upstream refresh must be a route-owned cell.
fn branches_carry_through(tree: &RealisedRouteTree, at: Anchor) -> bool {
    let kinds: BTreeMap<Anchor, BlockKind> =
        tree.cells.iter().map(|cell| (cell.at, cell.state.kind)).collect();
    let mut affected = tree.branches.iter().filter(|branch| branch.path.contains(&at)).peekable();
    if affected.peek().is_none() {
        return false;
    }
    affected.all(|branch| {
        let Some(position) = branch.path.iter().position(|cell| *cell == at) else {
            return false;
        };
        let Some(refresh) = branch.path[..position]
            .iter()
            .rposition(|cell| kinds.get(cell) == Some(&BlockKind::Repeater))
        else {
            return false;
        };
        if branch.path.last() != Some(&branch.terminal.at) {
            return false;
        }
        let mut strength = MAX_SIGNAL_STRENGTH;
        for cell in &branch.path[refresh + 1..] {
            let kind = kinds.get(cell).copied().or_else(|| {
                (*cell == branch.terminal.at).then_some(branch.terminal.state.kind)
            });
            match kind {
                Some(BlockKind::Repeater) => strength = MAX_SIGNAL_STRENGTH,
                Some(BlockKind::RedstoneWire) => {
                    strength -= 1;
                    if strength == 0 {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    })
}

/// The prune stage's descriptors, in stream order: every parent route whose
/// final tree has a `TimingArcKind::Route` arc, once each, by minimum
/// analysed slack of that final tree then original parent route id.
///
/// `parent_routes` maps each pre-union parent route id to the id of the tree
/// that carries it in the certified candidate (`union_candidate`'s second
/// result): a route absorbed by a block's output tree is timed under the
/// block's id, but the descriptor names the parent's own id, which is what
/// the union prunes. Routes whose final tree the graph does not time are not
/// offered.
pub(crate) fn prune_descriptors(
    timing: &RealisedTimingGraph,
    parent_routes: &BTreeMap<RouteId, RouteId>,
) -> Vec<ParentRouteChoice> {
    let Ok(analysed) = timing.analyse() else {
        return Vec::new();
    };
    let finals: BTreeSet<RouteId> = parent_routes.values().copied().collect();
    let mut slack = BTreeMap::<RouteId, _>::new();
    for arc in timing.arcs.values() {
        let TimingArcKind::Route { route, .. } = arc.kind else {
            continue;
        };
        if !finals.contains(&route) {
            continue;
        }
        let Some(&arc_slack) = analysed.slack.get(&arc.id) else {
            continue;
        };
        let entry = slack.entry(route).or_insert(arc_slack);
        *entry = (*entry).min(arc_slack);
    }
    let mut ordered: Vec<(_, RouteId)> = parent_routes
        .iter()
        .filter_map(|(&parent, realised)| slack.get(realised).map(|&s| (s, parent)))
        .collect();
    ordered.sort();
    ordered
        .into_iter()
        .map(|(_, route)| ParentRouteChoice { route })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::identity::{
        InstanceId, PrimitiveId, RoutedSinkId, TimingArcId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::timing_graph::{ExactDelay, TimingArc, TimingNodeId};
    use crate::compile::fragment_synth::union::tests::seam_tree;
    use crate::redstone::world::block::Facing;

    fn set(tree: &mut RealisedRouteTree, x: i32, z: i32, state: crate::redstone::world::block::BlockState) {
        let at = Anchor { x, y: 0, z };
        tree.cells.iter_mut().find(|cell| cell.at == at).expect("cell exists").state = state;
    }

    fn kind_at(tree: &RealisedRouteTree, x: i32, z: i32) -> BlockKind {
        let at = Anchor { x, y: 0, z };
        tree.cells.iter().find(|cell| cell.at == at).expect("cell exists").state.kind
    }

    /// `seam_tree(3, tail)`: dust x0..x2, a repeater at x3, two branches
    /// (z = 0, 1) of `tail` dust from x4, each ending in a terminal
    /// repeater at x = 4 + tail.
    #[test]
    fn prune_route_removes_only_strength_redundant_non_terminal_repeaters() {
        // Safe removal: a branch-private refresh at x5 behind the trunk's x3
        // repeater is redundant (x4..x7 is four dust into a repeater
        // terminal). x3 itself has no earlier repeater, so it stays; both
        // terminal repeaters stay even though the walk would allow them.
        let mut safe = seam_tree(3, 4);
        set(&mut safe, 5, 0, crate::compile::repeater(Facing::East));
        let before = safe.clone();
        assert!(prune_route(&mut safe), "the x5 refresh is removable");
        assert_eq!(kind_at(&safe, 5, 0), BlockKind::RedstoneWire);
        assert_eq!(kind_at(&safe, 3, 0), BlockKind::Repeater, "no earlier repeater: refused");
        assert_eq!(kind_at(&safe, 8, 0), BlockKind::Repeater, "terminals are never candidates");
        assert_eq!(kind_at(&safe, 8, 1), BlockKind::Repeater);
        assert_eq!(safe.branches, before.branches, "paths and terminals are untouched");
        assert_eq!(safe.floors, before.floors);
        assert_eq!(
            safe.cells.iter().map(|c| c.at).collect::<Vec<_>>(),
            before.cells.iter().map(|c| c.at).collect::<Vec<_>>(),
            "cell order and coordinates are preserved"
        );

        // No upstream refresh at all: nothing changes, exact state kept.
        let mut lonely = seam_tree(3, 4);
        let untouched = lonely.clone();
        assert!(!prune_route(&mut lonely));
        assert_eq!(lonely, untouched);

        // Branch starvation: with x1 refreshed, x3 could go for the z=1
        // branch (its own x6 repeater), but the z=0 branch would then run
        // 2 + 16 dust from x1 and reach zero, so x3 stays. x6 on z=1 is
        // tried first (deeper) and is itself refused: from x3 it would face
        // 16 dust. Nothing changes, and the failed attempts leave every
        // repeater with its original facing.
        let mut starved = seam_tree(3, 16);
        set(&mut starved, 1, 0, crate::compile::repeater(Facing::East));
        set(&mut starved, 6, 1, crate::compile::repeater(Facing::East));
        let kept = starved.clone();
        assert!(!prune_route(&mut starved));
        assert_eq!(starved, kept, "failed attempts revert exactly");
    }

    /// Greedy order is downstream first, and each attempt is proven against
    /// the cells as they stand after earlier deletions. `seam_tree(3, 13)`
    /// with refreshes at x1 (trunk) and x8 (each branch): both x8s go (from
    /// x3: 4 + 1 + 8 dust into a repeater terminal), and x3 -- which WOULD
    /// have passed against the original cells (x8 refreshed it) -- now faces
    /// 2 + 13 dust from x1 and reaches zero, so it is reverted exactly. x1
    /// has no earlier repeater and is refused as before.
    #[test]
    fn prune_route_removes_several_and_reverts_an_upstream_attempt_against_the_pruned_state() {
        let mut tree = seam_tree(3, 13);
        set(&mut tree, 1, 0, crate::compile::repeater(Facing::East));
        set(&mut tree, 8, 0, crate::compile::repeater(Facing::East));
        set(&mut tree, 8, 1, crate::compile::repeater(Facing::East));
        let mut expected = tree.clone();
        set(&mut expected, 8, 0, crate::compile::dust());
        set(&mut expected, 8, 1, crate::compile::dust());

        assert!(prune_route(&mut tree));
        assert_eq!(kind_at(&tree, 8, 0), BlockKind::RedstoneWire);
        assert_eq!(kind_at(&tree, 8, 1), BlockKind::RedstoneWire);
        assert_eq!(kind_at(&tree, 3, 0), BlockKind::Repeater, "x3 is refused once the x8s are dust");
        assert_eq!(kind_at(&tree, 1, 0), BlockKind::Repeater, "no earlier repeater: refused");
        assert_eq!(tree, expected, "exactly the two branch refreshes changed, nothing else");
    }

    /// A route's terminal cell may be owned by what it delivers into (a
    /// block lever, a gate landing) rather than by the tree; the walk then
    /// reads `terminal.state`. A non-conductor terminal still refuses, and
    /// the upstream refresh must still be a route-owned repeater.
    #[test]
    fn strength_walk_reads_the_terminal_state_when_the_tree_does_not_own_the_cell() {
        let disown_terminals = |tree: &mut RealisedRouteTree| {
            let terminals: BTreeSet<Anchor> = tree.branches.iter().map(|b| b.terminal.at).collect();
            tree.cells.retain(|cell| !terminals.contains(&cell.at));
        };
        // Terminal repeaters recorded on the branches only: x5 still goes.
        let mut lever = seam_tree(3, 4);
        set(&mut lever, 5, 0, crate::compile::repeater(Facing::East));
        disown_terminals(&mut lever);
        assert!(prune_route(&mut lever), "a repeater terminal off the cell list is a conductor");
        assert_eq!(kind_at(&lever, 5, 0), BlockKind::RedstoneWire);
        assert_eq!(kind_at(&lever, 3, 0), BlockKind::Repeater, "the source is never assumed");

        // A dust terminal costs one like any dust: from x3, 14 dust leave
        // strength 1 and the dust terminal takes it to zero, where the same
        // tail into a repeater terminal passes.
        let mut long = seam_tree(3, 14);
        set(&mut long, 5, 0, crate::compile::repeater(Facing::East));
        disown_terminals(&mut long);
        let mut into_repeater = long.clone();
        assert!(prune_route(&mut into_repeater), "14 dust into a repeater terminal keeps signal");
        for branch in &mut long.branches {
            branch.terminal.state = crate::compile::dust();
        }
        let kept = long.clone();
        assert!(!prune_route(&mut long));
        assert_eq!(long, kept);

        // A non-conductor terminal is refused outright.
        let mut stone = seam_tree(3, 4);
        set(&mut stone, 5, 0, crate::compile::repeater(Facing::East));
        disown_terminals(&mut stone);
        for branch in &mut stone.branches {
            branch.terminal.state = crate::compile::stone();
        }
        let kept = stone.clone();
        assert!(!prune_route(&mut stone));
        assert_eq!(stone, kept);
    }

    #[test]
    fn prune_descriptors_order_parent_routes_by_slack_then_id_once_each() {
        let node = |instance: u32| {
            TimingNodeId::PrimitiveOutput(PrimitiveId {
                instance: InstanceId(instance),
                node: TopologyNodeId(0),
            })
        };
        let route_arc = |id: u32, from: u32, to: u32, route: u32, ordinal: u16, delay: u64| {
            TimingArc::new(
                TimingArcId(id),
                node(from),
                node(to),
                TimingArcKind::Route {
                    route: RouteId(route),
                    sink: RoutedSinkId { route: RouteId(route), ordinal },
                },
                ExactDelay(delay),
            )
        };
        // 0 -> 1 via route 7 (slack 0, critical); 0 -> 2 via route 7 sink 1
        // (slack 4), route 5 (slack 6) and route 9 (slack 10).
        let arcs = [
            route_arc(0, 0, 1, 7, 0, 10),
            route_arc(1, 0, 2, 7, 1, 6),
            route_arc(2, 0, 2, 5, 0, 4),
            route_arc(3, 0, 2, 9, 0, 0),
        ];
        let graph = RealisedTimingGraph::new([node(0), node(1), node(2)], arcs).unwrap();
        // Parent routes 5 and 7 kept their ids; parent 3 was absorbed into
        // child tree 9 and parent 2 into tree 7 (a pass-through chain);
        // parent 11's tree is not timed.
        let parents = BTreeMap::from([
            (RouteId(2), RouteId(7)),
            (RouteId(3), RouteId(9)),
            (RouteId(5), RouteId(5)),
            (RouteId(7), RouteId(7)),
            (RouteId(11), RouteId(11)),
        ]);
        assert_eq!(
            prune_descriptors(&graph, &parents),
            vec![
                ParentRouteChoice { route: RouteId(2) },
                ParentRouteChoice { route: RouteId(7) },
                ParentRouteChoice { route: RouteId(5) },
                ParentRouteChoice { route: RouteId(3) },
            ],
            "slack is read from the final tree (2 and 7 tie at 0, then 5, then 3 via tree 9), \
             ties break on the original id, 9 is never named and 11 is not offered"
        );
    }
}
