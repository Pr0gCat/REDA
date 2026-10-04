//! Structural relocation and removal proposals for route-owned refreshes.
//! This mutates only a draft manifest. Callers must still certify the complete
//! source rewrite and electrical world, then qualify its retained waveforms.
use super::candidate::ExpandedPhysicalCandidate;
use crate::redstone::world::block::BlockKind;
use std::collections::BTreeMap;

pub fn flatten_refreshes(candidate: &mut ExpandedPhysicalCandidate) -> usize {
    let mut moved = 0;
    for route in candidate.routes.values_mut() {
        let mut proposals = BTreeMap::new();
        for branch in &route.branches {
            for i in 2..branch.path.len().saturating_sub(1) {
                let before = branch.path[i - 1];
                let at = branch.path[i];
                let next = branch.path[i + 1];
                let earlier = branch.path[i - 2];
                let Some(relay) = route
                    .cells
                    .iter()
                    .find(|b| b.at == at && b.state.kind == BlockKind::Repeater)
                else {
                    continue;
                };
                if next.y == at.y || before.y != at.y || earlier.y != at.y {
                    continue;
                }
                let Some(dust) = route
                    .cells
                    .iter()
                    .find(|b| b.at == before && b.state.kind == BlockKind::RedstoneWire)
                else {
                    continue;
                };
                if !crate::compile::routing::route_step_is_legal(earlier, before, at, &relay.state)
                {
                    continue;
                }
                if candidate
                    .observations
                    .values()
                    .any(|o| o.site.at == before || o.site.at == at)
                {
                    continue;
                }
                proposals.insert(at, (before, relay.state.clone(), dust.state.clone()));
            }
        }
        for (at, (before, relay, dust)) in proposals {
            // Preserve every branch's refresh count and prefix membership.
            if route
                .branches
                .iter()
                .any(|b| b.path.contains(&at) != b.path.contains(&before))
            {
                continue;
            }

            if route
                .cells
                .iter()
                .find(|b| b.at == before)
                .is_none_or(|b| b.state.kind != BlockKind::RedstoneWire)
            {
                continue;
            }
            for cell in &mut route.cells {
                if cell.at == at {
                    cell.state = dust.clone();
                } else if cell.at == before {
                    cell.state = relay.clone();
                }
            }
            moved += 1;
        }
    }
    moved
}

/// Structurally eligible interior relays, in stable route/coordinate order.
/// Eligibility is not a strength or functional proof; certify each resulting draft.
pub fn removable_flat_refreshes(
    candidate: &ExpandedPhysicalCandidate,
) -> Vec<(super::identity::RouteId, crate::compile::geometry::Anchor)> {
    let mut sites = Vec::new();
    for (id, route) in &candidate.routes {
        for cell in &route.cells {
            if cell.state.kind == BlockKind::Repeater
                && removal_state(candidate, *id, cell.at).is_some()
            {
                sites.push((*id, cell.at));
            }
        }
    }
    sites.sort_unstable();
    sites
}

fn removal_state(
    candidate: &ExpandedPhysicalCandidate,
    id: super::identity::RouteId,
    at: crate::compile::geometry::Anchor,
) -> Option<crate::redstone::world::block::BlockState> {
    let route = candidate.routes.get(&id)?;
    let relay = route.cells.iter().find(|c| c.at == at)?;
    if relay.state.kind != BlockKind::Repeater
        || candidate.observations.values().any(|o| o.site.at == at)
        || candidate
            .placements
            .values()
            .any(|p| p.blocks.iter().any(|b| b.at == at))
        || candidate
            .boundaries
            .values()
            .any(|p| p.blocks.iter().any(|b| b.at == at))
    {
        return None;
    }
    let mut replacement = None;
    for branch in &route.branches {
        let Some(index) = branch.path.iter().position(|p| *p == at) else {
            continue;
        };
        if index == 0
            || index + 1 >= branch.path.len()
            || branch.terminal.at == at
            || branch.terminal.repeaters == 0
        {
            return None;
        }
        let before = branch.path[index - 1];
        let after = branch.path[index + 1];
        if before.y != at.y
            || after.y != at.y
            || !crate::compile::routing::route_step_is_legal(before, at, after, &relay.state)
        {
            return None;
        }
        let dust = route
            .cells
            .iter()
            .find(|c| c.at == before && c.state.kind == BlockKind::RedstoneWire)?;
        replacement = Some(dust.state.clone());
    }
    replacement
}

/// Remove one eligible refresh from a draft, updating every affected branch count.
/// Floor, paths, bindings and observations remain intact. Full certification and
/// retained-history quality decide whether the resulting electrical world is usable.
pub fn remove_flat_refresh(
    candidate: &mut ExpandedPhysicalCandidate,
    id: super::identity::RouteId,
    at: crate::compile::geometry::Anchor,
) -> bool {
    let Some(state) = removal_state(candidate, id, at) else {
        return false;
    };
    let route = candidate.routes.get_mut(&id).unwrap();
    route.cells.iter_mut().find(|c| c.at == at).unwrap().state = state;
    for branch in &mut route.branches {
        if branch.path.contains(&at) {
            branch.terminal.repeaters -= 1;
        }
    }
    true
}

/// The remaining isolated terminal can refresh a dust run of at most fifteen.
/// This is a proposal recognizer; certification and retained waveform replay
/// remain mandatory, including interactions outside the edited route.
pub fn adjacent_terminal_refresh(candidate: &ExpandedPhysicalCandidate, id: super::identity::RouteId, at: crate::compile::geometry::Anchor) -> bool {
    let Some(route)=candidate.routes.get(&id) else { return false; };
    let Some(relay)=route.cells.iter().find(|c|c.at==at) else {return false;};
    let mut matched=false;
    for branch in &route.branches {
        let Some(i)=branch.path.iter().position(|p|*p==at) else {continue;};
        let Some(next)=branch.path.get(i+1) else {return false;};
        if *next != branch.terminal.at {return false;}
        let Some(terminal)=route.cells.iter().find(|c|c.at==*next) else {return false;};
        if terminal.state.kind!=BlockKind::Repeater || terminal.state.facing!=relay.state.facing {return false;}
        let mut dust=1usize; let mut refreshed=false;
        for p in branch.path[..i].iter().rev() {
            let Some(cell)=route.cells.iter().find(|c|c.at==*p) else {return false;};
            if cell.state.kind==BlockKind::Repeater {refreshed=true;break;}
            if cell.state.kind!=BlockKind::RedstoneWire {return false;}
            dust+=1;
        }
        if !refreshed || dust>15 {return false;}
        matched=true;
    }
    matched
}
