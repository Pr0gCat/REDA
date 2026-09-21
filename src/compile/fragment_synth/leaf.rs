//! Leaf synthesis of one chunk inside its allocated contract.
//!
//! Runs the shipping planner and verifier on the chunk's netlist with the
//! child's **local** portal pins, realises the candidate into a world sized
//! exactly to the local envelope, then refuses any occupied cell the parent
//! did not allocate.  The planner already refuses primitives and route
//! anchors outside the world (`World::set` would drop them silently); the
//! scan here closes the rest: cells inside the world but in the parent-owned
//! `x = 0` column or `z = 0` caller row.  The far and top halo faces lie
//! outside this exact-size world and are refused by the planner.
//!
//! No retries, no repair, no second placer or router.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use thiserror::Error;

use crate::compile::fragment_synth::allocation::{ChildAllocation, Prism};
use crate::compile::fragment_synth::partition::{Chunk, ChunkId};
use crate::compile::geometry::Anchor;
use crate::compile::planner::{self, PlannerError, PortRole};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// One compiled chunk in its local frame; the parent translates it by
/// [`ChildAllocation::origin`] when composing.
#[derive(Debug, Clone)]
pub struct LeafArtifact {
    pub chunk: ChunkId,
    /// Local world, `(0, 0, 0)` at the allocation's origin, spanning the
    /// local region plus the halo column and caller row.
    pub world: World,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum LeafError {
    #[error("allocation is for chunk {allocated:?}, not {chunk:?}")]
    AllocationMismatch { chunk: ChunkId, allocated: ChunkId },
    #[error("planner refused the chunk: {0}")]
    Planner(#[source] PlannerError),
    #[error("{role:?} port {signal} realised at {actual:?}, expected local {expected:?}")]
    PortMismatch {
        signal: String,
        role: PortRole,
        expected: Anchor,
        actual: Option<(i32, i32, i32)>,
    },
    #[error("{kind:?} at local {at:?} lies outside the allocated region")]
    EscapedCell { at: Anchor, kind: BlockKind },
}

/// Compile `chunk` under `allocation`.
pub fn synthesise_leaf(
    chunk: &Chunk,
    allocation: &ChildAllocation,
) -> Result<LeafArtifact, LeafError> {
    if chunk.id != allocation.chunk {
        return Err(LeafError::AllocationMismatch {
            chunk: chunk.id.clone(),
            allocated: allocation.chunk.clone(),
        });
    }
    let placements = allocation.port_placements();
    let region = allocation.local_region();
    let size = (region.max.x + 1, region.max.y + 1, region.max.z + 1);

    let candidate =
        planner::plan_from_netlist(&chunk.netlist, &placements).map_err(LeafError::Planner)?;
    let realised = planner::realise_and_verify(&candidate, &chunk.netlist, size)
        .map_err(LeafError::Planner)?;
    for portal in &allocation.portals {
        let expected = allocation.to_local(portal.pin.at);
        let actual = match portal.role {
            PortRole::Input => realised.ports.input_positions.get(&portal.signal),
            PortRole::Output => realised.ports.output_positions.get(&portal.signal),
        }
        .copied();
        if actual != Some((expected.x, expected.y, expected.z)) {
            return Err(LeafError::PortMismatch {
                signal: portal.signal.clone(),
                role: portal.role,
                expected,
                actual,
            });
        }
    }
    refuse_escapes(&realised.world, &region)?;

    Ok(LeafArtifact {
        chunk: chunk.id.clone(),
        world: realised.world,
    })
}

/// The first occupied cell, in `(y, z, x)` scan order, outside `region`.
fn refuse_escapes(world: &World, region: &Prism) -> Result<(), LeafError> {
    let (sx, sy, sz) = world.size();
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let kind = world.get(x, y, z).kind;
                let at = Anchor { x, y, z };
                if kind != BlockKind::Air && !region.contains(at) {
                    return Err(LeafError::EscapedCell { at, kind });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::{allocate, AllocationLimits, RegionMask};
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::stone;
    use crate::compile::{Gate, Netlist};

    const LIMITS: AllocationLimits = AllocationLimits {
        delay_budget_ticks: 4,
        corridor_capacity: 8,
    };

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        }
    }

    fn allocated(net: &Netlist, max_gates: usize) -> (Vec<Chunk>, Vec<ChildAllocation>) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), max_gates).unwrap();
        let plan = allocate(net, &chunks, LIMITS).unwrap();
        (chunks, plan.children)
    }

    fn allocation_for<'a>(children: &'a [ChildAllocation], chunk: &Chunk) -> &'a ChildAllocation {
        children.iter().find(|c| c.chunk == chunk.id).unwrap()
    }

    #[test]
    fn pinned_chunks_compile_inside_their_local_region() {
        let net = chain();
        let (chunks, children) = allocated(&net, 1);
        for chunk in &chunks {
            let allocation = allocation_for(&children, chunk);
            let leaf = synthesise_leaf(chunk, allocation).unwrap();
            assert_eq!(leaf.chunk, chunk.id);
            let region = allocation.local_region();
            assert_eq!(
                leaf.world.size(),
                (region.max.x + 1, region.max.y + 1, region.max.z + 1)
            );
            assert!(refuse_escapes(&leaf.world, &region).is_ok());
        }
    }

    #[test]
    fn shrunken_region_is_refused_by_the_planner() {
        let net = chain();
        let (chunks, children) = allocated(&net, 1);
        let mut allocation = allocation_for(&children, &chunks[0]).clone();
        let mut bounds = allocation.region.bounds();
        bounds.max = Anchor {
            x: bounds.min.x + 1,
            y: bounds.max.y,
            z: bounds.min.z + 1,
        };
        allocation.region = RegionMask::Prism(bounds);
        assert!(matches!(
            synthesise_leaf(&chunks[0], &allocation),
            Err(LeafError::Planner(PlannerError::UnrealisableNode { .. }))
        ));
    }

    #[test]
    fn occupied_halo_cell_is_an_escape_with_its_coordinate() {
        let region = Prism {
            min: Anchor { x: 1, y: 0, z: 1 },
            max: Anchor { x: 3, y: 3, z: 3 },
        };
        let mut world = World::new(4, 4, 4);
        world.set(2, 1, 2, stone());
        assert_eq!(refuse_escapes(&world, &region), Ok(()));
        world.set(2, 1, 0, stone());
        assert_eq!(
            refuse_escapes(&world, &region),
            Err(LeafError::EscapedCell {
                at: Anchor { x: 2, y: 1, z: 0 },
                kind: BlockKind::Solid,
            })
        );
    }

    #[test]
    fn mismatched_chunk_is_refused() {
        let net = chain();
        let (chunks, children) = allocated(&net, 1);
        let wrong = allocation_for(&children, &chunks[1]);
        assert_eq!(
            synthesise_leaf(&chunks[0], wrong).err(),
            Some(LeafError::AllocationMismatch {
                chunk: chunks[0].id.clone(),
                allocated: chunks[1].id.clone(),
            })
        );
    }
}
