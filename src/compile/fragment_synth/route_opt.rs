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
use crate::compile::routing::route_step_is_legal;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState};

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
/// first mutation, so a retained step never re-enumerates. Every pair is
/// enumerated: what keeps that affordable is the route's own structure and
/// not a work cap, since a pair whose window can be read exactly offers only
/// the cells that break its one over-long run of dust. Returns whether
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
/// A window that is not one straight run gets exactly one trial, and only
/// once every straight pair for that `D` has failed: see
/// [`first_legal_bend_cell`] for what "legal" has to mean once the geometry
/// stops saying it for free.
///
/// Delays are not refreshed here, exactly as in `prune_route`.
pub(crate) fn relocate_refresh(tree: &mut RealisedRouteTree) -> bool {
    relocate_refresh_outcome(tree).changed
}

/// What [`relocate_refresh`] did: whether it changed the tree at all, and
/// whether any step it kept came from the one bend trial a downstream
/// refresh is allowed.
///
/// The bend bit is proof material -- the focused tests and the single
/// real-circuit attribution run read it -- and reaches no trace, fingerprint
/// or counter. Production callers see the boolean they always saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RefreshRelocationOutcome {
    pub(crate) changed: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) used_bend_fallback: bool,
}

/// [`relocate_refresh`] itself, with the bend bit kept.
pub(crate) fn relocate_refresh_outcome(tree: &mut RealisedRouteTree) -> RefreshRelocationOutcome {
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
    // Reading a window off intervals needs every anchor to name one cell: a
    // repeated cell anchor makes the map below disagree with the position
    // lookups a trial uses, and a repeated path anchor puts one cell at two
    // indices, so an interval derived from either can exclude the other.
    // Neither happens on a realised route; where one does, every cell of
    // every window is tried, which is what this pass has always done.
    let named_once = {
        let mut cells = BTreeSet::new();
        tree.cells.iter().all(|cell| cells.insert(cell.at))
            && tree.branches.iter().all(|branch| {
                let mut path = BTreeSet::new();
                branch.path.iter().all(|at| path.insert(*at))
            })
    };

    let mut outcome = RefreshRelocationOutcome::default();
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
        let sunk_state = std::mem::replace(&mut tree.cells[sunk].state, crate::compile::dust());
        let prunable = branches_carry_through(tree, down);
        tree.cells[sunk].state = sunk_state;
        if prunable {
            continue;
        }
        // The tree as it stands for this `D`, which is the tree every window
        // below is read from: the check above put `D` back, a failed trial
        // restores exactly what it touched, and the trial that does not fail
        // leaves for the next `D`. So this is live, including after an
        // earlier pair was retained.
        let kinds: BTreeMap<Anchor, BlockKind> = if named_once {
            tree.cells
                .iter()
                .map(|cell| (cell.at, cell.state.kind))
                .collect()
        } else {
            BTreeMap::new()
        };
        // The one bend cell this `D` may still try, remembered in exactly
        // the `U` and reversed-window order the straight trials take, so
        // which cell it is never depends on when those trials gave up.
        let mut bend: Option<(Anchor, Anchor)> = None;
        for &(_, up) in &candidates[position + 1..] {
            let Some(source) = tree.cells.iter().position(|cell| cell.at == up) else {
                continue;
            };
            if tree.cells[source].state.kind != BlockKind::Repeater {
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
                // `D` is going, so it need not face the way `U` does: what
                // has to hold instead is that `U`'s own unchanged state is a
                // legal step at `N` on every branch that names it, which one
                // straight run gave for free. Only the first legal cell is
                // remembered, and only one is, so a bent window cannot turn
                // into a search.
                if named_once && bend.is_none() {
                    bend = first_legal_bend_cell(tree, &kinds, &terminals, shared, up, down)
                        .map(|at| (up, at));
                }
                continue;
            }
            // Equal facings are a straight-path condition: along one run `N`
            // inherits `U`'s state unexamined, so `D` has to be the same
            // refresh moved along and not a different one.
            if tree.cells[source].state.facing != tree.cells[sunk].state.facing {
                continue;
            }
            // Owned so the edits below can mutate the tree in place.
            let window: Vec<Anchor> = shared[1..shared.len() - 1].to_vec();
            // Which offsets could carry this pair at all, where the tree is
            // regular enough to say. This decides no relocation -- every
            // cell it keeps is still proven by the trial below -- it only
            // spares the trials that provably cannot carry.
            let offsets = if named_once {
                let Some(offsets) = relocation_offsets(tree, &kinds, up, down) else {
                    continue;
                };
                Some(offsets)
            } else {
                None
            };
            for (offset, &at) in window.iter().enumerate().rev() {
                // `window[i]` is `shared[i + 1]`.
                if let Some((lowest, highest)) = offsets {
                    if !(lowest..=highest).contains(&(offset + 1)) {
                        continue;
                    }
                }
                let Some(target) = tree.cells.iter().position(|cell| cell.at == at) else {
                    continue;
                };
                if tree.cells[target].state.kind != BlockKind::RedstoneWire
                    || terminals.contains(&at)
                {
                    continue;
                }
                if relocation_trial(tree, source, sunk, target, up) {
                    outcome.changed = true;
                    continue 'pairs;
                }
            }
        }
        // Every straight trial for this `D` has failed. One bend cell may be
        // tried now -- the same three replacements, proven by the same gate --
        // and if it fails this pair is done: no second bend cell is ever
        // mutated for this `D`.
        let Some((up, at)) = bend else {
            continue;
        };
        let (Some(source), Some(target)) = (
            tree.cells.iter().position(|cell| cell.at == up),
            tree.cells.iter().position(|cell| cell.at == at),
        ) else {
            continue;
        };
        if relocation_trial(tree, source, sunk, target, up) {
            outcome.changed = true;
            outcome.used_bend_fallback = true;
        }
    }
    outcome
}

/// The pass's one trial, whatever offered the cell: copy `U`'s state onto
/// `N`, dust `U` and `D`, and keep that only when the tree came away with
/// exactly one repeater fewer and every branch through `U` still carries.
/// Anything else restores the three exact prior states and refuses.
///
/// That count is the point of the whole step. The guards on the cell already
/// make it so, and this is what holds them to it -- an `N` that landed on a
/// repeater, or on `U` or `D` themselves, would save nothing or two.
fn relocation_trial(
    tree: &mut RealisedRouteTree,
    source: usize,
    sunk: usize,
    target: usize,
    up: Anchor,
) -> bool {
    let repeaters = |tree: &RealisedRouteTree| {
        tree.cells
            .iter()
            .filter(|cell| cell.state.kind == BlockKind::Repeater)
            .count()
    };
    let moved = tree.cells[source].state.clone();
    let standing = repeaters(tree);
    let target_state = std::mem::replace(&mut tree.cells[target].state, moved);
    let source_state = std::mem::replace(&mut tree.cells[source].state, crate::compile::dust());
    let sunk_state = std::mem::replace(&mut tree.cells[sunk].state, crate::compile::dust());
    if repeaters(tree) + 1 == standing && branches_carry_through(tree, up) {
        return true;
    }
    tree.cells[target].state = target_state;
    tree.cells[source].state = source_state;
    tree.cells[sunk].state = sunk_state;
    false
}

/// The deepest cell of a shared `U..=D` window that is not one straight run
/// at which `U`'s own refresh could stand, or `None` where the window offers
/// none.
///
/// The offset bound and the route-owned, non-terminal dust conditions are
/// the straight path's own, asked here in the same reversed order. What one
/// straight run gave for free and this has to ask for is the step itself: a
/// repeater reads from one side and drives the other, so `U`'s unchanged
/// state has to be a legal step at `N` on every branch that names `N`, by
/// `route_step_is_legal` -- the same authority the router lays cells by.
///
/// Legality filters the enumeration rather than deciding it: an illegal cell
/// is passed over and the next one asked, so the answer is the deepest legal
/// cell rather than the deepest cell when it happens to be legal.
fn first_legal_bend_cell(
    tree: &RealisedRouteTree,
    kinds: &BTreeMap<Anchor, BlockKind>,
    terminals: &BTreeSet<Anchor>,
    shared: &[Anchor],
    up: Anchor,
    down: Anchor,
) -> Option<Anchor> {
    let (lowest, highest) = relocation_offsets(tree, kinds, up, down)?;
    let source = tree.cells.iter().position(|cell| cell.at == up)?;
    let moved = tree.cells[source].state.clone();
    shared[1..shared.len() - 1]
        .iter()
        .enumerate()
        .rev()
        .find(|&(offset, &at)| {
            // The window entry at `offset` is `shared[offset + 1]`, exactly
            // as the straight trials count it.
            (lowest..=highest).contains(&(offset + 1))
                && kinds.get(&at) == Some(&BlockKind::RedstoneWire)
                && !terminals.contains(&at)
                && branches_accept_step(tree, at, &moved)
        })
        .map(|(_, &at)| at)
}

/// Every branch that names `at` names it exactly once, has a cell on each
/// side of it there, and takes `state` at it as a legal step. A branch that
/// does not name `at` is not asked; at least one has to.
///
/// A branch that names `at` twice is refused rather than read: which of the
/// two occurrences the repeater would have to be legal at is not a question
/// this pass answers. An end of a path is refused for the same reason --
/// there is no cell on one side for the repeater to read from or drive.
fn branches_accept_step(tree: &RealisedRouteTree, at: Anchor, state: &BlockState) -> bool {
    let mut affected = false;
    for branch in &tree.branches {
        let mut named = branch
            .path
            .iter()
            .enumerate()
            .filter(|(_, cell)| **cell == at);
        let Some((index, _)) = named.next() else {
            continue;
        };
        if named.next().is_some() {
            return false;
        }
        affected = true;
        let (Some(&previous), Some(&next)) = (
            index
                .checked_sub(1)
                .and_then(|before| branch.path.get(before)),
            branch.path.get(index + 1),
        ) else {
            return false;
        };
        if !route_step_is_legal(previous, at, next, state) {
            return false;
        }
    }
    affected
}

/// The offsets into the shared `U..=D` slice -- counted from `U`, the way
/// the slice itself counts -- at which a relocated refresh could still carry
/// every branch through `U`, or `None` when no cell can. Sound in the one
/// direction that matters: an offset left out provably fails
/// `branches_carry_through`, so dropping it cannot cost a relocation, while
/// an offset kept is still proven by the walk itself.
///
/// Each affected branch is read exactly as that walk will read it -- from
/// its own earlier route-owned refresh, `terminal.state` standing in for a
/// terminal cell the tree does not own -- with `U` and `D` already dust and
/// `N` not yet placed. A cell the walk cannot conduct sinks the whole pair,
/// since `N` only ever replaces one dust cell of the window and nothing else
/// out here ever becomes a conductor. Otherwise only a run of more than
/// `MAX_SIGNAL_STRENGTH - 1` dust in a row has to be broken: one repeater
/// breaks one run, so a second such run is fatal, and leaving neither half
/// of `[a, b]` over that length puts `N` in `[b - longest, a + longest]`.
///
/// Every range is read back to slice offsets before it meets another one.
/// Branches that reach `U` through feeders of different lengths name the
/// same cell at different path indices, and raw indices from two of them
/// intersect to nonsense.
fn relocation_offsets(
    tree: &RealisedRouteTree,
    kinds: &BTreeMap<Anchor, BlockKind>,
    up: Anchor,
    down: Anchor,
) -> Option<(usize, usize)> {
    let longest = usize::from(MAX_SIGNAL_STRENGTH) - 1;
    let (mut lowest, mut highest) = (0usize, usize::MAX);
    for branch in &tree.branches {
        let Some(from) = branch.path.iter().position(|at| *at == up) else {
            continue;
        };
        let refresh = branch.path[..from]
            .iter()
            .rposition(|at| kinds.get(at) == Some(&BlockKind::Repeater))?;
        if branch.path.last() != Some(&branch.terminal.at) {
            return None;
        }
        // The suffix the walk will cross, one entry per cell: `dust[i]` is
        // path index `refresh + 1 + i`.
        let mut dust = Vec::with_capacity(branch.path.len() - refresh);
        for at in &branch.path[refresh + 1..] {
            let kind = if *at == up || *at == down {
                Some(BlockKind::RedstoneWire)
            } else {
                kinds
                    .get(at)
                    .copied()
                    .or_else(|| (*at == branch.terminal.at).then_some(branch.terminal.state.kind))
            };
            match kind {
                Some(BlockKind::RedstoneWire) => dust.push(true),
                Some(BlockKind::Repeater) => dust.push(false),
                _ => return None,
            }
        }
        let mut over_long: Option<(usize, usize)> = None;
        let mut index = 0;
        while index < dust.len() {
            if !dust[index] {
                index += 1;
                continue;
            }
            let start = index;
            while index < dust.len() && dust[index] {
                index += 1;
            }
            if index - start > longest {
                if over_long.is_some() {
                    return None;
                }
                over_long = Some((refresh + 1 + start, refresh + index));
            }
        }
        // Every run of this branch survives as it stands: it asks nothing of
        // `N`, wherever the other branches put it.
        let Some((first, last)) = over_long else {
            continue;
        };
        // One repeater cannot break a run longer than two survivable halves,
        // and a break that would have to land upstream of `U` is not this
        // pair's to make: `N` is strictly inside the window.
        if last - longest > first + longest || first + longest < from {
            return None;
        }
        lowest = lowest.max((last - longest).saturating_sub(from));
        highest = highest.min(first + longest - from);
    }
    Some((lowest, highest))
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
    let kinds: BTreeMap<Anchor, BlockKind> = tree
        .cells
        .iter()
        .map(|cell| (cell.at, cell.state.kind))
        .collect();
    let mut affected = tree
        .branches
        .iter()
        .filter(|branch| branch.path.contains(&at))
        .peekable();
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
            let kind = kinds
                .get(cell)
                .copied()
                .or_else(|| (*cell == branch.terminal.at).then_some(branch.terminal.state.kind));
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
    use crate::compile::fragment_synth::candidate::{PlacedBlock, RouteTarget};
    use crate::compile::fragment_synth::identity::{
        InstanceId, PortId, PrimitiveId, RoutedSinkId, TimingArcId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::timing_graph::{ExactDelay, TimingArc, TimingNodeId};
    use crate::compile::fragment_synth::union::tests::seam_tree;
    use crate::redstone::world::block::Facing;

    fn set(
        tree: &mut RealisedRouteTree,
        x: i32,
        z: i32,
        state: crate::redstone::world::block::BlockState,
    ) {
        let at = Anchor { x, y: 0, z };
        tree.cells
            .iter_mut()
            .find(|cell| cell.at == at)
            .expect("cell exists")
            .state = state;
    }

    fn kind_at(tree: &RealisedRouteTree, x: i32, z: i32) -> BlockKind {
        let at = Anchor { x, y: 0, z };
        tree.cells
            .iter()
            .find(|cell| cell.at == at)
            .expect("cell exists")
            .state
            .kind
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

    /// Rewrite every anchor a tree owns -- cells, floors, and each branch's
    /// root, path and terminal -- through `moved`.
    fn remap_anchors(tree: &mut RealisedRouteTree, moved: impl Fn(Anchor) -> Anchor) {
        for cell in tree.cells.iter_mut().chain(tree.floors.iter_mut()) {
            cell.at = moved(cell.at);
        }
        for branch in &mut tree.branches {
            branch.root = moved(branch.root);
            for at in &mut branch.path {
                *at = moved(*at);
            }
            branch.terminal.at = moved(branch.terminal.at);
        }
    }

    /// Swap x and z on every anchor a tree owns and quarter-turn every
    /// horizontal facing with them, so a fixture that ran along +x runs along
    /// +z and still faces the way it travels.
    fn onto_z_axis(tree: &mut RealisedRouteTree) {
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
            cell.state.facing = face(cell.state.facing);
        }
        for branch in &mut tree.branches {
            branch.terminal.state.facing = face(branch.terminal.state.facing);
        }
        remap_anchors(tree, |at| Anchor {
            x: at.z,
            y: at.y,
            z: at.x,
        });
    }

    /// Re-lay a straight `y = 0`, `z = 0` fixture so consecutive cells step by
    /// `step` instead of `(1, 0, 0)`: the anchor at x`k` moves to `k * step`.
    /// Every step of the run stays identical, so the constant-delta test
    /// cannot be what rejects it.
    fn step_by(tree: &mut RealisedRouteTree, step: (i32, i32, i32)) {
        remap_anchors(tree, |at| Anchor {
            x: at.x * step.0,
            y: at.x * step.1,
            z: at.x * step.2,
        });
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
        assert_eq!(
            kind_at(&safe, 3, 0),
            BlockKind::Repeater,
            "no earlier repeater: refused"
        );
        assert_eq!(
            kind_at(&safe, 8, 0),
            BlockKind::Repeater,
            "terminals are never candidates"
        );
        assert_eq!(kind_at(&safe, 8, 1), BlockKind::Repeater);
        assert_eq!(
            safe.branches, before.branches,
            "paths and terminals are untouched"
        );
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
        assert_eq!(
            kind_at(&tree, 3, 0),
            BlockKind::Repeater,
            "x3 is refused once the x8s are dust"
        );
        assert_eq!(
            kind_at(&tree, 1, 0),
            BlockKind::Repeater,
            "no earlier repeater: refused"
        );
        assert_eq!(
            tree, expected,
            "exactly the two branch refreshes changed, nothing else"
        );
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
        assert!(
            prune_route(&mut lever),
            "a repeater terminal off the cell list is a conductor"
        );
        assert_eq!(kind_at(&lever, 5, 0), BlockKind::RedstoneWire);
        assert_eq!(
            kind_at(&lever, 3, 0),
            BlockKind::Repeater,
            "the source is never assumed"
        );

        // A dust terminal costs one like any dust: from x3, 14 dust leave
        // strength 1 and the dust terminal takes it to zero, where the same
        // tail into a repeater terminal passes.
        let mut long = seam_tree(3, 14);
        set(&mut long, 5, 0, crate::compile::repeater(Facing::East));
        disown_terminals(&mut long);
        let mut into_repeater = long.clone();
        assert!(
            prune_route(&mut into_repeater),
            "14 dust into a repeater terminal keeps signal"
        );
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
                    sink: RoutedSinkId {
                        route: RouteId(route),
                        ordinal,
                    },
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

    /// `N` may not land on a cell another branch delivers into. A 29-dust
    /// tail leaves exactly one cell in the (x18, x9) window that carries the
    /// pair: x15, the last cell x0's strength reaches and the first that is
    /// within 14 dust of the terminal. The control saves a repeater there;
    /// with a second branch delivering onto that same cell the pass has to
    /// come away with nothing instead of overwriting the delivery.
    #[test]
    fn refresh_relocation_will_not_overwrite_another_branch_delivery() {
        let mut control = linear_relocation_route(29);
        assert!(
            relocate_refresh(&mut control),
            "x15 is the one cell in the window that carries"
        );
        assert_eq!(repeater_anchors(&control, |at| at.x), vec![0, 15, 30]);

        // The delivery arrives from the side, so it shares no cell with the
        // trunk but its own landing: both refreshes still serve exactly the
        // branches they did, and x15 is the same cell the control took.
        let mut delivered = linear_relocation_route(29);
        let mut delivery = delivered.branches[0].clone();
        delivery.sink.ordinal = 1;
        delivery.path = (0..=3).rev().map(|z| Anchor { x: 15, y: 0, z }).collect();
        delivery.root = delivery.path[0];
        delivery.terminal.sink = delivery.sink;
        delivery.terminal.at = Anchor { x: 15, y: 0, z: 0 };
        delivery.terminal.state = crate::compile::dust();
        for &at in &delivery.path[..3] {
            let state = crate::compile::dust();
            delivered.cells.push(PlacedBlock { at, state });
        }
        delivered.branches.push(delivery);

        let before = delivered.clone();
        assert!(
            !relocate_refresh(&mut delivered),
            "x15 is a delivery terminal, and nothing else in the window carries"
        );
        assert_eq!(delivered, before, "a refusal must not touch the tree");
    }

    /// Every guard in one table. Each fixture makes the (x18, x9) pair -- and
    /// every other pair on that tree -- unusable, so the call must be false
    /// and the tree must come back exactly as it went in.
    ///
    /// Geometry alone is no longer one of those guards: a window that is not
    /// one straight run is refused only where no cell of it can hold `U`'s
    /// refresh legally, which is what the two uniform runs below stand for.
    /// A window that does offer one is
    /// `refresh_bend_relocation_moves_one_pair_across_a_valid_bend`.
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
        // x18 turned to face back up the run. Everything else about the pair
        // is exactly the shape that succeeds, so only the facings differ:
        // two repeaters pointing different ways are not one refresh moved
        // along one run, and there is no single state for `N` to inherit.
        let facing_mismatch = {
            let mut tree = linear_relocation_route(26);
            set(&mut tree, 18, 0, crate::compile::repeater(Facing::West));
            tree
        };
        // Uniform runs that no per-step comparison can fault: every step is
        // identical, but one travels two cells at once and the other climbs.
        // Only the "horizontal, one cell" test rejects these as straight --
        // and the bend path, which is offered both of them, finds no cell
        // where a repeater could read from one side and drive the other,
        // since every step of either run has y or a second axis in it.
        let uniform_diagonal = {
            let mut tree = linear_relocation_route(26);
            step_by(&mut tree, (1, 0, 1));
            tree
        };
        let uniform_staircase = {
            let mut tree = linear_relocation_route(26);
            step_by(&mut tree, (1, 1, 0));
            // The one row here the bend path could physically be offered:
            // a one-cell rise per step is a contiguous route, and it is
            // refused for having no legal site rather than for its shape.
            tree.validate()
                .expect("a one-cell rise per step is a contiguous route");
            tree
        };

        let cases = [
            ("unequal branch membership", unequal_membership),
            ("empty window", empty_window),
            ("directly prunable D", directly_prunable),
            ("no owned cell in the window", unowned_window),
            ("non-conductor window", non_conductor_window),
            ("no strength-feasible N", no_feasible_window),
            ("mismatched U and D facings", facing_mismatch),
            ("uniformly diagonal run", uniform_diagonal),
            ("uniformly climbing run", uniform_staircase),
        ];
        for (case, tree) in cases {
            let mut refused = tree.clone();
            assert!(!relocate_refresh(&mut refused), "{case}");
            assert_eq!(refused, tree, "{case}: a refusal must not touch the tree");
        }
    }

    /// `linear_relocation_route(26)` joined by a second branch that comes
    /// down -z through fourteen cells of its own, refreshed at (9, -9),
    /// runs the trunk from x9 to x18 and then leaves it for its own
    /// terminal at (18, 12). Both branches carry the identical x9..x18
    /// slice, so (x18, x9) is a legal pair -- but they name that slice at
    /// different indices: x9 is index 9 on the trunk and index 14 on the
    /// feeder.
    fn converging_relocation_route() -> RealisedRouteTree {
        let mut tree = linear_relocation_route(26);
        let mut feeder = tree.branches[0].clone();
        feeder.sink.ordinal = 1;
        feeder.path = (1..=14)
            .rev()
            .map(|z| Anchor { x: 9, y: 0, z: -z })
            .chain((9..=18).map(|x| Anchor { x, y: 0, z: 0 }))
            .chain((1..=12).map(|z| Anchor { x: 18, y: 0, z }))
            .collect();
        feeder.root = feeder.path[0];
        feeder.terminal.sink = feeder.sink;
        feeder.terminal.at = *feeder.path.last().expect("the feeder has a path");
        feeder.terminal.state = crate::compile::repeater(Facing::South);
        let refresh = Anchor { x: 9, y: 0, z: -9 };
        for &at in feeder.path.iter().filter(|at| at.z != 0) {
            let state = if at == refresh || at == feeder.terminal.at {
                crate::compile::repeater(Facing::South)
            } else {
                crate::compile::dust()
            };
            tree.cells.push(PlacedBlock { at, state });
        }
        tree.branches.push(feeder);
        tree
    }

    /// Two branches through one pair name its window at different indices,
    /// so what each of them allows has to be read back to the shared slice
    /// before the two are put together. Here the trunk allows path indices
    /// 12..=15 and the feeder allows exactly path index 20: as raw indices
    /// those meet nowhere, and as offsets into the x9..x18 slice they meet
    /// on exactly one cell -- offset 6, x15.
    #[test]
    fn refresh_relocation_intersects_converging_branches_on_the_shared_slice() {
        let tree = converging_relocation_route();

        // Neither refresh is removable on its own, on either branch.
        let mut pruned = tree.clone();
        assert!(
            !prune_route(&mut pruned),
            "direct pruning takes neither x9 nor x18"
        );
        assert_eq!(pruned, tree);

        // x16 and x17 starve the trunk and x14 starves the feeder's own
        // tail, so x15 is the only cell of the window that carries both --
        // and it is also the deepest one the trunk alone would allow.
        for (n, carries) in [(17, false), (16, false), (15, true), (14, false)] {
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

        let mut relocated = tree.clone();
        assert!(
            relocate_refresh(&mut relocated),
            "x15 carries both branches"
        );
        let mut expected = tree.clone();
        set(&mut expected, 9, 0, crate::compile::dust());
        set(&mut expected, 18, 0, crate::compile::dust());
        set(&mut expected, 15, 0, crate::compile::repeater(Facing::East));
        assert_eq!(
            relocated, expected,
            "the one cell both branches allow, and only it"
        );

        let settled = relocated.clone();
        assert!(!relocate_refresh(&mut relocated), "no second pair");
        assert_eq!(relocated, settled);
    }

    /// The terminal cell may belong to what the route delivers into rather
    /// than to the tree, and then `terminal.state` stands in for it -- for
    /// the strength walk, and equally for whatever decides which cells that
    /// walk is worth asking about. A 29-dust tail leaves exactly one
    /// feasible cell, x15, and it is feasible only because the recorded
    /// repeater terminal ends the run of dust: read as a non-conductor
    /// instead, the pair would look impossible and the saving would be lost.
    #[test]
    fn refresh_relocation_reads_the_terminal_state_when_the_tree_does_not_own_the_cell() {
        let mut tree = linear_relocation_route(29);
        let terminal = tree.branches[0].terminal.at;
        assert_eq!(terminal, Anchor { x: 30, y: 0, z: 0 });
        assert_eq!(tree.branches[0].terminal.state.kind, BlockKind::Repeater);
        tree.cells.retain(|cell| cell.at != terminal);

        let mut relocated = tree.clone();
        assert!(
            relocate_refresh(&mut relocated),
            "the recorded terminal is a repeater"
        );
        assert_eq!(
            repeater_anchors(&relocated, |at| at.x),
            vec![0, 15],
            "the same x15 the owned control takes, with the terminal off the cell list"
        );
        let mut expected = tree.clone();
        set(&mut expected, 9, 0, crate::compile::dust());
        set(&mut expected, 18, 0, crate::compile::dust());
        set(&mut expected, 15, 0, crate::compile::repeater(Facing::East));
        assert_eq!(
            relocated, expected,
            "one repeater saved, nothing else touched"
        );
    }

    /// `linear_relocation_route` with `decoys` further East refreshes every
    /// 15 cells from x30, and everything from x28 on folded into a
    /// horizontal staircase. The (x18, x9) pair keeps the exact straight
    /// shape that relocates to x15, and it is the most upstream pair, so the
    /// greedy walk reaches it last. Every pair among the decoys -- and every
    /// pair reaching across them -- is refused by the straight-run test, so
    /// all `decoys` decides is how many refused pairs stand between the pass
    /// and the one pair it can take.
    fn staircase_decoy_route(decoys: usize) -> RealisedRouteTree {
        let last = 30 + 15 * (decoys as i32 - 1);
        let mut tree = linear_relocation_route(last as usize + 14);
        for j in 0..decoys as i32 {
            set(
                &mut tree,
                30 + 15 * j,
                0,
                crate::compile::repeater(Facing::East),
            );
        }
        // Alternating unit steps in x and z: no slice of three cells or more
        // out here has one constant delta.
        remap_anchors(&mut tree, |at| {
            if at.x < 28 {
                return at;
            }
            let along = at.x - 28;
            Anchor {
                x: 28 + (along + 1) / 2,
                y: at.y,
                z: along / 2,
            }
        });
        tree
    }

    /// Nothing about a route's size may change what this pass takes from
    /// it. Both fixtures hold the same one relocatable pair, the most
    /// upstream on the tree, and `decoys` only decides how many refused
    /// pairs the greedy walk enumerates before reaching it: 60 of them make
    /// that walk long, not doubtful. What bounds the search on the long
    /// fixture is the tree's own structure -- from x0 the (x18, x9) window
    /// holds exactly one cell that both reaches and carries -- so the answer
    /// is the same relocation, to the same x15, as on the short one.
    #[test]
    fn refresh_relocation_takes_the_one_relocatable_pair_however_large_the_route() {
        for decoys in [3, 60] {
            let tree = staircase_decoy_route(decoys);
            let mut relocated = tree.clone();
            assert!(
                relocate_refresh(&mut relocated),
                "{decoys} decoys: the (x18, x9) pair is reached"
            );
            assert_eq!(
                kind_at(&relocated, 15, 0),
                BlockKind::Repeater,
                "{decoys} decoys: saved at x15"
            );
            assert_eq!(kind_at(&relocated, 9, 0), BlockKind::RedstoneWire);
            assert_eq!(kind_at(&relocated, 18, 0), BlockKind::RedstoneWire);
            let mut expected = tree.clone();
            set(&mut expected, 9, 0, crate::compile::dust());
            set(&mut expected, 18, 0, crate::compile::dust());
            set(&mut expected, 15, 0, crate::compile::repeater(Facing::East));
            assert_eq!(
                relocated, expected,
                "{decoys} decoys: exactly one pair moved, nothing else"
            );

            // Deterministic and idempotent: the only pair left is (x0, x15),
            // whose `U` has no earlier route-owned refresh.
            let settled = relocated.clone();
            assert!(
                !relocate_refresh(&mut relocated),
                "{decoys} decoys: no second pair"
            );
            assert_eq!(
                relocated, settled,
                "{decoys} decoys: a refusal leaves no partial mutation"
            );
        }
    }

    /// `linear_relocation_route(26)` with the cell at x`stair` lifted one
    /// block. Every consecutive step of the trunk stays adjacent -- one cell
    /// horizontally, at most one vertically -- so the route is still
    /// contiguous and `validate` says so, but the shared x9..x18 window is no
    /// longer one constant horizontal step and the old straight test refuses
    /// the pair outright.
    ///
    /// `stair` is a parameter for the reason `tail` is one: where the rise
    /// sits decides which window cells keep both of their neighbours on the
    /// level, which is exactly what the first-legal-cell order is read from.
    /// Nothing else about the fixture moves, so the horizontal axis is
    /// unchanged and no repeater has to be re-laid.
    fn bent_relocation_route(stair: i32) -> RealisedRouteTree {
        let mut tree = linear_relocation_route(26);
        divert(
            &mut tree,
            stair,
            Anchor {
                x: stair,
                y: 1,
                z: 0,
            },
        );
        tree
    }

    /// Give `tree` one more branch running `path`, owning every cell of it
    /// the tree does not already hold, and ending in `terminal`.
    fn add_branch(
        tree: &mut RealisedRouteTree,
        path: &[Anchor],
        terminal: crate::redstone::world::block::BlockState,
    ) {
        let ordinal = tree.branches.len() as u16;
        let mut branch = tree.branches[0].clone();
        branch.sink.ordinal = ordinal;
        branch.target = RouteTarget::DeclaredOutput(PortId(u32::from(ordinal) + 1));
        branch.root = path[0];
        branch.path = path.to_vec();
        branch.terminal.sink = branch.sink;
        branch.terminal.at = *path.last().expect("a branch has a path");
        branch.terminal.state = terminal.clone();
        let owned: BTreeSet<Anchor> = tree.cells.iter().map(|cell| cell.at).collect();
        for (index, &at) in path.iter().enumerate() {
            if owned.contains(&at) {
                continue;
            }
            let state = if index + 1 == path.len() {
                terminal.clone()
            } else {
                crate::compile::dust()
            };
            tree.cells.push(PlacedBlock { at, state });
        }
        tree.branches.push(branch);
    }

    /// Which cells of the x10..x17 window could hold `U`'s own East repeater
    /// at all, by the router's own step authority.
    fn locally_legal_window_cells(tree: &RealisedRouteTree) -> Vec<i32> {
        let moved = crate::compile::repeater(Facing::East);
        let path = &tree.branches[0].path;
        (10..=17)
            .filter(|&x| {
                let index = path
                    .iter()
                    .position(|at| at.x == x)
                    .expect("every window cell is on the trunk");
                route_step_is_legal(path[index - 1], path[index], path[index + 1], &moved)
            })
            .collect()
    }

    /// The same saving the straight pass makes, across a window that is not
    /// one straight run. Nothing about the pair changes except the geometry:
    /// `prune_route` still refuses both refreshes, the shared slice is no
    /// longer one constant step -- which is the whole of what used to refuse
    /// it -- and the deepest window cell whose neighbours are both on the
    /// level takes `U`'s state and saves the repeater.
    #[test]
    fn refresh_bend_relocation_moves_one_pair_across_a_valid_bend() {
        let tree = bent_relocation_route(13);
        tree.validate()
            .expect("the bent fixture is a contiguous route");
        assert_eq!(repeater_anchors(&tree, |at| at.x), vec![0, 9, 18, 27]);

        // The old straight eligibility, read off the fixture: the shared
        // x9..x18 slice no longer has one constant delta.
        let shared = &tree.branches[0].path[9..=18];
        let delta = |from: Anchor, to: Anchor| (to.x - from.x, to.y - from.y, to.z - from.z);
        let step = delta(shared[0], shared[1]);
        assert!(
            !shared
                .windows(2)
                .all(|pair| delta(pair[0], pair[1]) == step),
            "the window is not one straight run, so the straight path refuses this pair"
        );

        // Nothing else moved: direct pruning still takes neither refresh.
        let mut pruned = tree.clone();
        assert!(
            !prune_route(&mut pruned),
            "direct pruning takes neither x9 nor x18"
        );
        assert_eq!(pruned, tree);

        // The rise and both of its neighbours have y in a step, so no
        // repeater can read from one side and drive the other there.
        assert_eq!(
            locally_legal_window_cells(&tree),
            vec![10, 11, 15, 16, 17],
            "x12, the rise itself and x14 lose a neighbour to the rise"
        );

        // Strength, proven from `U` exactly as the straight pass proves it:
        // x16 and x17 are legal steps but out of x0's reach, and x15 is the
        // deepest cell that is both legal and reachable.
        for (n, carries) in [(17, false), (16, false), (15, true), (12, true)] {
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

        let mut relocated = tree.clone();
        assert_eq!(
            relocate_refresh_outcome(&mut relocated),
            RefreshRelocationOutcome {
                changed: true,
                used_bend_fallback: true,
            },
            "the one retained step came from the bend fallback"
        );
        assert_eq!(
            repeater_anchors(&relocated, |at| at.x),
            vec![0, 15, 27],
            "exactly one repeater fewer"
        );
        let mut expected = tree.clone();
        set(&mut expected, 9, 0, crate::compile::dust());
        set(&mut expected, 18, 0, crate::compile::dust());
        set(&mut expected, 15, 0, crate::compile::repeater(Facing::East));
        assert_eq!(
            relocated, expected,
            "one repeater saved, x15 carries x9's own state"
        );
        relocated
            .validate()
            .expect("the saved tree is still a contiguous route");

        // Idempotent, exactly as the straight pass is: the only pair left is
        // (x0, x15), whose `U` has no earlier route-owned refresh.
        let settled = relocated.clone();
        assert!(
            !relocate_refresh(&mut relocated),
            "the saved tree has no second pair"
        );
        assert_eq!(relocated, settled);
    }

    /// One bend cell per downstream refresh, and it is the first legal cell
    /// the reversed window offers rather than the deepest cell the offsets
    /// allow. With the rise at x15 the two deepest feasible cells each lose a
    /// neighbour to it and are passed over without ever being mutated; x13 is
    /// the first that survives, and x12 -- legal, feasible and left as dust --
    /// proves the enumeration stopped there rather than ran out.
    #[test]
    fn refresh_bend_relocation_tries_only_the_first_legal_cell_per_downstream() {
        let tree = bent_relocation_route(15);
        tree.validate()
            .expect("the bent fixture is a contiguous route");
        let mut pruned = tree.clone();
        assert!(
            !prune_route(&mut pruned),
            "direct pruning takes neither x9 nor x18"
        );
        assert_eq!(pruned, tree);

        assert_eq!(
            locally_legal_window_cells(&tree),
            vec![10, 11, 12, 13, 17],
            "x14, the rise itself and x16 lose a neighbour to the rise"
        );
        // Every one of x12, x13 and x14 carries the pair, so strength is not
        // what picks between them.
        for (n, carries) in [(16, false), (14, true), (13, true), (12, true)] {
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

        let mut relocated = tree.clone();
        assert_eq!(
            relocate_refresh_outcome(&mut relocated),
            RefreshRelocationOutcome {
                changed: true,
                used_bend_fallback: true,
            }
        );
        assert_eq!(
            repeater_anchors(&relocated, |at| at.x),
            vec![0, 13, 27],
            "the first legal cell, not the deepest feasible one"
        );
        let mut expected = tree.clone();
        set(&mut expected, 9, 0, crate::compile::dust());
        set(&mut expected, 18, 0, crate::compile::dust());
        set(&mut expected, 13, 0, crate::compile::repeater(Facing::East));
        assert_eq!(
            relocated, expected,
            "exactly one window cell was written: x14 and the rise were filtered, \
             and legal x12 behind x13 was never reached"
        );
        assert_eq!(kind_at(&relocated, 12, 0), BlockKind::RedstoneWire);
        assert_eq!(kind_at(&relocated, 14, 0), BlockKind::RedstoneWire);
    }

    /// The bend cell is asked of every branch that names it, and of no
    /// branch that does not. A second branch crossing `N` on the other axis,
    /// a branch rooted on `N` with no cell behind it, and a branch that names
    /// `N` twice each refuse the candidate before anything is mutated, while
    /// a branch that never names `N` changes nothing.
    #[test]
    fn refresh_bend_relocation_checks_every_branch_and_repeated_occurrence() {
        let at = |x: i32, z: i32| Anchor { x, y: 0, z };
        let base = bent_relocation_route(13);
        let mut control = base.clone();
        assert!(
            relocate_refresh(&mut control),
            "x15 is the one legal, reachable cell of the bent window"
        );
        assert_eq!(kind_at(&control, 15, 0), BlockKind::Repeater);

        // Crossing `N` along z: the repeater would have to read from the
        // north and drive the east at the same time.
        let crossing = {
            let mut tree = base.clone();
            add_branch(
                &mut tree,
                &[
                    at(15, -3),
                    at(15, -2),
                    at(15, -1),
                    at(15, 0),
                    at(15, 1),
                    at(15, 2),
                ],
                crate::compile::repeater(Facing::South),
            );
            tree
        };
        // Rooted on `N`: there is no cell for the repeater to read from.
        let rooted = {
            let mut tree = base.clone();
            add_branch(
                &mut tree,
                &[at(15, 0), at(15, 1), at(15, 2)],
                crate::compile::repeater(Facing::South),
            );
            tree
        };
        // `N` twice on one path: no interval read off that branch can be
        // trusted, so the bend path is off for the whole tree.
        let repeated = {
            let mut tree = base.clone();
            add_branch(
                &mut tree,
                &[
                    at(15, 1),
                    at(15, 0),
                    at(15, -1),
                    at(14, -1),
                    at(14, 0),
                    at(15, 0),
                    at(16, 0),
                    at(17, 0),
                ],
                crate::compile::dust(),
            );
            tree
        };
        for (case, tree) in [
            ("branch crossing N on the other axis", crossing),
            ("branch rooted on N", rooted),
            ("branch naming N twice", repeated),
        ] {
            let mut refused = tree.clone();
            assert!(!relocate_refresh(&mut refused), "{case}");
            assert_eq!(refused, tree, "{case}: a refusal must not touch the tree");
        }

        // A branch that never names `N` is not asked about it.
        let mut unrelated = base.clone();
        add_branch(
            &mut unrelated,
            &[at(20, -3), at(20, -2), at(20, -1), at(20, 0)],
            crate::compile::dust(),
        );
        unrelated
            .validate()
            .expect("the side branch is a contiguous route");
        let mut expected = unrelated.clone();
        set(&mut expected, 9, 0, crate::compile::dust());
        set(&mut expected, 18, 0, crate::compile::dust());
        set(&mut expected, 15, 0, crate::compile::repeater(Facing::East));
        assert!(relocate_refresh(&mut unrelated), "the bend still carries");
        assert_eq!(
            unrelated, expected,
            "an unaffected branch changes neither the cell taken nor anything else"
        );
    }

    /// Nothing the pass decides may depend on the order its input happens to
    /// be in, and the boolean the union reads is exactly the outcome's
    /// `changed`. Order is compared canonically -- anchor to state, and the
    /// set of anchors that moved -- because the vectors themselves keep the
    /// order they came in: this pass sorts no production vector.
    #[test]
    fn refresh_bend_relocation_is_order_stable_and_preserves_the_bool_wrapper() {
        let at = |x: i32, z: i32| Anchor { x, y: 0, z };
        let mut tree = bent_relocation_route(13);
        add_branch(
            &mut tree,
            &[at(20, -3), at(20, -2), at(20, -1), at(20, 0)],
            crate::compile::dust(),
        );
        tree.validate().expect("the fixture is a contiguous route");

        let canonical = |tree: &RealisedRouteTree| {
            tree.cells
                .iter()
                .map(|cell| (cell.at, cell.state.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let mutated = |before: &RealisedRouteTree, after: &RealisedRouteTree| {
            let before = canonical(before);
            canonical(after)
                .into_iter()
                .filter(|(at, state)| before.get(at) != Some(state))
                .map(|(at, _)| at)
                .collect::<BTreeSet<_>>()
        };
        let shuffle = |tree: &RealisedRouteTree| {
            let mut shuffled = tree.clone();
            shuffled.cells.reverse();
            shuffled.branches.reverse();
            shuffled
        };

        let mut ordered = tree.clone();
        let outcome = relocate_refresh_outcome(&mut ordered);
        assert_eq!(
            outcome,
            RefreshRelocationOutcome {
                changed: true,
                used_bend_fallback: true,
            }
        );

        let shuffled_input = shuffle(&tree);
        assert_ne!(
            shuffled_input.cells, tree.cells,
            "the shuffle really reorders the input"
        );
        let mut shuffled = shuffled_input.clone();
        assert_eq!(relocate_refresh_outcome(&mut shuffled), outcome);
        assert_eq!(
            canonical(&shuffled),
            canonical(&ordered),
            "the same anchor-to-state map, whatever order the input came in"
        );
        assert_eq!(
            mutated(&shuffled_input, &shuffled),
            mutated(&tree, &ordered),
            "and the same mutation set"
        );
        assert_eq!(
            mutated(&tree, &ordered),
            BTreeSet::from([at(9, 0), at(15, 0), at(18, 0)]),
            "one cell written, two dusted"
        );
        assert_ne!(
            shuffled.cells, ordered.cells,
            "no production vector was sorted to get there"
        );

        // The union's own contract: the boolean is `changed` and nothing else.
        let mut wrapped = tree.clone();
        assert_eq!(relocate_refresh(&mut wrapped), outcome.changed);
        assert_eq!(wrapped, ordered, "the wrapper runs the same pass");

        // A straight retention and a refusal are each distinguished from a
        // retained bend.
        let mut straight = linear_relocation_route(26);
        assert_eq!(
            relocate_refresh_outcome(&mut straight),
            RefreshRelocationOutcome {
                changed: true,
                used_bend_fallback: false,
            },
            "the straight pass never reports a bend"
        );
        let mut refused = linear_relocation_route(40);
        assert_eq!(
            relocate_refresh_outcome(&mut refused),
            RefreshRelocationOutcome::default(),
            "a refusal reports neither"
        );
    }
}
