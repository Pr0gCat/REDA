//! Parent Route Repack: replace strength-redundant repeaters on one
//! parent-owned route with dust, or merge a redundant pair of them into one
//! repeater further downstream.
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

/// Merge one strength-redundant pair of refreshes into a single repeater
/// further downstream: turn an upstream `U` and a downstream `D` into dust
/// and put `U`'s own repeater back on one dust cell strictly between them.
/// Greedy and downstream first over a candidate vector frozen before the
/// first mutation, so a retained step never re-enumerates. Returns whether
/// anything changed.
///
/// This is the pair `prune_route` has to refuse: neither refresh carries its
/// branches once it is dust, so neither can simply go, yet one repeater in
/// the middle does carry them. The router placed `U` for an earlier branch,
/// refreshed the trunk again at `D` for a later one, and never went back to
/// ask whether one repeater between them would have served both.
///
/// The trial is proven from `U`, never from `N`: starting there re-proves
/// every branch across the whole relocated window and demands that `U` still
/// have an earlier route-owned refresh standing, which is also what keeps
/// this pass off the route's source repeater.
///
/// Delays are not refreshed here, exactly as in `prune_route`.
pub(crate) fn relocate_refresh(tree: &mut RealisedRouteTree) -> bool {
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
    let repeaters = |tree: &RealisedRouteTree| {
        tree.cells
            .iter()
            .filter(|cell| cell.state.kind == BlockKind::Repeater)
            .count()
    };

    let mut changed = false;
    'pairs: for (position, &(_, down)) in candidates.iter().enumerate() {
        // Frozen candidates go stale as steps are retained: `D` has to be a
        // repeater on the tree as it stands now.
        let Some(sunk) = tree.cells.iter().position(|cell| cell.at == down) else {
            continue;
        };
        if tree.cells[sunk].state.kind != BlockKind::Repeater {
            continue;
        }
        // A refresh `prune_route` deletes outright is its business, not this
        // pass's: relocating it would spend a repeater to keep what costs
        // nothing to drop.
        let mut probe = tree.clone();
        probe.cells[sunk].state = crate::compile::dust();
        if branches_carry_through(&probe, down) {
            continue;
        }
        for &(_, up) in &candidates[position + 1..] {
            let Some(source) = tree.cells.iter().position(|cell| cell.at == up) else {
                continue;
            };
            if tree.cells[source].state.kind != BlockKind::Repeater
                || tree.cells[source].state.facing != tree.cells[sunk].state.facing
            {
                continue;
            }
            // Equal, non-empty branch membership over one identical straight
            // horizontal slice. That is what makes `U`'s recorded state the
            // right state for a cell further along the same run: the
            // successor faces the way `U` already faces, so nothing about a
            // repeater has to be reconstructed here.
            let mut shared: Option<&[Anchor]> = None;
            let mut agrees = true;
            for branch in &tree.branches {
                let from = branch.path.iter().position(|at| *at == up);
                let to = branch.path.iter().position(|at| *at == down);
                match (from, to) {
                    (None, None) => {}
                    (Some(from), Some(to)) if from < to => {
                        let slice = &branch.path[from..=to];
                        agrees = *shared.get_or_insert(slice) == slice;
                    }
                    _ => agrees = false,
                }
                if !agrees {
                    break;
                }
            }
            let Some(shared) = shared.filter(|_| agrees) else {
                continue;
            };
            // One constant horizontal step, on whichever axis the trunk runs:
            // no y in it, one cell of travel, and the same delta throughout.
            let delta = |from: Anchor, to: Anchor| (to.x - from.x, to.y - from.y, to.z - from.z);
            let step = delta(shared[0], shared[1]);
            let straight = step.1 == 0
                && step.0.abs() + step.2.abs() == 1
                && shared.windows(2).all(|p| delta(p[0], p[1]) == step);
            if !straight {
                continue;
            }
            // Owned so the retained step below can take the tree.
            let window: Vec<Anchor> = shared[1..shared.len() - 1].to_vec();
            for &at in window.iter().rev() {
                let Some(target) = tree.cells.iter().position(|cell| cell.at == at) else {
                    continue;
                };
                if tree.cells[target].state.kind != BlockKind::RedstoneWire
                    || terminals.contains(&at)
                {
                    continue;
                }
                let mut trial = tree.clone();
                trial.cells[target].state = tree.cells[source].state.clone();
                trial.cells[source].state = crate::compile::dust();
                trial.cells[sunk].state = crate::compile::dust();
                // The point of the whole step: exactly one repeater fewer.
                // The guards above already make that so, and this is what
                // holds them to it -- an `N` that landed on a repeater, or
                // on `U` or `D`, would save nothing or two.
                if repeaters(&trial) + 1 != repeaters(tree) {
                    continue;
                }
                if !branches_carry_through(&trial, up) {
                    continue;
                }
                *tree = trial;
                changed = true;
                continue 'pairs;
            }
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

    /// The straight parent trunk the relocation pass exists for:
    /// `seam_tree(0, tail)` with only its `z = 0` branch and cells retained,
    /// then route-owned East refreshes at `x = 9` and `x = 18`. Dust runs
    /// x1..x`tail` into a repeater terminal at x`tail + 1`, and the source
    /// repeater at x0 is the branch's only earlier refresh. `tail` stays a
    /// parameter so the no-feasible-`N` refusal can stretch the same shape.
    fn linear_relocation_route(tail: usize) -> RealisedRouteTree {
        let mut tree = seam_tree(0, tail);
        tree.branches.truncate(1);
        let straight: BTreeSet<Anchor> = tree.branches[0].path.iter().copied().collect();
        tree.cells.retain(|cell| straight.contains(&cell.at));
        set(&mut tree, 9, 0, crate::compile::repeater(Facing::East));
        set(&mut tree, 18, 0, crate::compile::repeater(Facing::East));
        tree
    }

    /// Move one interior cell of a straight fixture off the line, in the cell
    /// list and in every path that names it.
    fn divert(tree: &mut RealisedRouteTree, x: i32, to: Anchor) {
        let from = Anchor { x, y: 0, z: 0 };
        tree.cells
            .iter_mut()
            .find(|cell| cell.at == from)
            .expect("cell exists")
            .at = to;
        for branch in &mut tree.branches {
            for at in branch.path.iter_mut().filter(|at| **at == from) {
                *at = to;
            }
        }
    }

    /// Swap x and z on every anchor a tree owns -- cells, floors, each
    /// branch's root, path and terminal -- and quarter-turn every horizontal
    /// facing with them, so a fixture that ran along +x runs along +z and
    /// still faces the way it travels.
    fn onto_z_axis(tree: &mut RealisedRouteTree) {
        let turn = |at: Anchor| Anchor {
            x: at.z,
            y: at.y,
            z: at.x,
        };
        let face = |facing: Option<Facing>| {
            facing.map(|facing| match facing {
                Facing::East => Facing::South,
                Facing::South => Facing::East,
                Facing::West => Facing::North,
                Facing::North => Facing::West,
                other => other,
            })
        };
        for cell in tree.cells.iter_mut().chain(tree.floors.iter_mut()) {
            cell.at = turn(cell.at);
            cell.state.facing = face(cell.state.facing);
        }
        for branch in &mut tree.branches {
            branch.root = turn(branch.root);
            for at in &mut branch.path {
                *at = turn(*at);
            }
            branch.terminal.at = turn(branch.terminal.at);
            branch.terminal.state.facing = face(branch.terminal.state.facing);
        }
    }

    /// Every route-owned repeater of a single-run fixture, by its coordinate
    /// `along` the run.
    fn repeater_anchors(tree: &RealisedRouteTree, along: impl Fn(Anchor) -> i32) -> Vec<i32> {
        let mut anchors: Vec<i32> = tree
            .cells
            .iter()
            .filter(|cell| cell.state.kind == BlockKind::Repeater)
            .map(|cell| along(cell.at))
            .collect();
        anchors.sort();
        anchors
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

    /// The shape the pass exists for: two refreshes that neither carry the
    /// branch alone, so pruning either is refused, but one repeater placed
    /// between them carries it. From x0 the walk has strength 6 left at x9,
    /// so `N` cannot be further than x14 past it, and from `N` the tail
    /// x19..x26 into the terminal repeater needs `N >= 12`.
    #[test]
    fn refresh_relocation_merges_a_redundant_pair_into_one_downstream_repeater() {
        let tree = linear_relocation_route(26);
        assert_eq!(repeater_anchors(&tree, |at| at.x), vec![0, 9, 18, 27]);

        // Neither refresh is directly removable: without x18 the suffix from
        // x9 runs 17 dust, and without x9 the suffix from x0 runs 17 dust.
        let mut pruned = tree.clone();
        assert!(
            !prune_route(&mut pruned),
            "direct pruning takes neither x9 nor x18"
        );
        assert_eq!(pruned, tree);

        // The proof runs from `U`, not from `N`, so a trial must both reach
        // `N` from x0's strength and carry the tail out of `N`.
        for (n, carries) in [(17, false), (16, false), (15, true), (13, true)] {
            let mut trial = tree.clone();
            set(&mut trial, 9, 0, crate::compile::dust());
            set(&mut trial, 18, 0, crate::compile::dust());
            set(&mut trial, n, 0, crate::compile::repeater(Facing::East));
            assert_eq!(
                branches_carry_through(&trial, Anchor { x: 9, y: 0, z: 0 }),
                carries,
                "manual trial at N = {n}"
            );
        }

        // Downstream first: the reversed window stops at the deepest
        // feasible cell, x15, though x13 also carries.
        let mut relocated = tree.clone();
        assert!(relocate_refresh(&mut relocated));
        assert_eq!(repeater_anchors(&relocated, |at| at.x), vec![0, 15, 27]);
        let mut expected = tree.clone();
        set(&mut expected, 9, 0, crate::compile::dust());
        set(&mut expected, 18, 0, crate::compile::dust());
        set(&mut expected, 15, 0, crate::compile::repeater(Facing::East));
        assert_eq!(
            relocated, expected,
            "one repeater saved, x15 carries x9's own state"
        );

        // Idempotent: the only pair left is (x0, x15), and x0 has no earlier
        // route-owned refresh, so the proof starting at `U` refuses it --
        // which is also what keeps the source refresh from being deleted.
        let settled = relocated.clone();
        assert!(
            !relocate_refresh(&mut relocated),
            "the saved tree has no second pair"
        );
        assert_eq!(relocated, settled);
    }

    /// The same saving on the same shape laid along z instead of x. A parent
    /// trunk runs whichever way the floorplan puts it, so "straight
    /// horizontal" has to mean one constant step with no y in it -- not one
    /// constant step in x -- or half of every real parent route is refused
    /// for the axis it happens to sit on.
    #[test]
    fn refresh_relocation_saves_a_repeater_on_a_z_axis_route() {
        let mut tree = linear_relocation_route(26);
        onto_z_axis(&mut tree);
        assert_eq!(repeater_anchors(&tree, |at| at.z), vec![0, 9, 18, 27]);
        assert!(
            tree.cells.iter().all(|c| c.at.x == 0 && c.at.y == 0),
            "the whole route now runs along z"
        );

        // Nothing else moved: direct pruning still takes neither refresh.
        let mut pruned = tree.clone();
        assert!(
            !prune_route(&mut pruned),
            "direct pruning takes neither z9 nor z18"
        );
        assert_eq!(pruned, tree);

        let mut relocated = tree.clone();
        assert!(
            relocate_refresh(&mut relocated),
            "a z run is as straight as an x run"
        );
        assert_eq!(repeater_anchors(&relocated, |at| at.z), vec![0, 15, 27]);
        let mut expected = tree.clone();
        let along_z = crate::compile::repeater(Facing::South);
        set(&mut expected, 0, 9, crate::compile::dust());
        set(&mut expected, 0, 18, crate::compile::dust());
        set(&mut expected, 0, 15, along_z);
        assert_eq!(
            relocated, expected,
            "z15 carries z9's own state, which faces along z"
        );

        let settled = relocated.clone();
        assert!(!relocate_refresh(&mut relocated), "still no second pair");
        assert_eq!(relocated, settled);
    }

    /// Every guard in one table. Each fixture makes the (x18, x9) pair -- and
    /// every other pair on that tree -- unusable, so the call must be false
    /// and the tree must come back exactly as it went in.
    #[test]
    fn refresh_relocation_refuses_unsafe_pairs_and_leaves_the_tree_unchanged() {
        // A second branch that leaves the trunk at x12 carries x9 but not
        // x18, so the two refreshes no longer serve the same branches.
        let unequal_membership = {
            let mut tree = linear_relocation_route(26);
            let mut stub = tree.branches[0].clone();
            stub.path.truncate(13);
            stub.terminal.at = Anchor { x: 12, y: 0, z: 0 };
            stub.terminal.state = crate::compile::repeater(Facing::East);
            tree.branches.push(stub);
            tree
        };
        // A jog in z, then in y: the shared slice is no longer one straight
        // horizontal run, so x9's facing is not x`N`'s facing.
        let bend = {
            let mut tree = linear_relocation_route(26);
            divert(&mut tree, 13, Anchor { x: 13, y: 0, z: 1 });
            tree
        };
        let vertical = {
            let mut tree = linear_relocation_route(26);
            divert(&mut tree, 13, Anchor { x: 13, y: 1, z: 0 });
            tree
        };
        // Adjacent refreshes: there is no cell strictly between them.
        let empty_window = {
            let mut tree = linear_relocation_route(26);
            set(&mut tree, 18, 0, crate::compile::dust());
            set(&mut tree, 10, 0, crate::compile::repeater(Facing::East));
            tree
        };
        // With x24 added, both x24 and x18 carry without themselves, so
        // `prune_route` deletes them outright and this pass stands aside
        // rather than spending a repeater to keep one.
        let directly_prunable = {
            let mut tree = linear_relocation_route(26);
            set(&mut tree, 24, 0, crate::compile::repeater(Facing::East));
            tree
        };
        // The window is not the route's to write: no owned cell at all, then
        // owned cells that are not conductors.
        let unowned_window = {
            let mut tree = linear_relocation_route(26);
            tree.cells.retain(|cell| !(10..=17).contains(&cell.at.x));
            tree
        };
        let non_conductor_window = {
            let mut tree = linear_relocation_route(26);
            for x in 10..=17 {
                set(&mut tree, x, 0, crate::compile::stone());
            }
            tree
        };
        // A 40-dust tail: every reachable cell is too far from the terminal
        // and every cell close enough is unreachable from x0.
        let no_feasible_window = linear_relocation_route(40);

        let cases = [
            ("unequal branch membership", unequal_membership),
            ("bend in the shared slice", bend),
            ("vertical step in the shared slice", vertical),
            ("empty window", empty_window),
            ("directly prunable D", directly_prunable),
            ("no owned cell in the window", unowned_window),
            ("non-conductor window", non_conductor_window),
            ("no strength-feasible N", no_feasible_window),
        ];
        for (case, tree) in cases {
            let mut refused = tree.clone();
            assert!(!relocate_refresh(&mut refused), "{case}");
            assert_eq!(refused, tree, "{case}: a refusal must not touch the tree");
        }
    }
}
