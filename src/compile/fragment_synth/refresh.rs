//! Move an interior refresh off a staircase onto the preceding flat cell.
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
