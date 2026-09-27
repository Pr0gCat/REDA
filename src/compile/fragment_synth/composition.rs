//! Parent-owned composition of independently generated chunk worlds.

use std::collections::HashMap;

use thiserror::Error;

use crate::compile::stone;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

pub type Cell = (i32, i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalContract {
    pub at: Cell,
    pub producer: usize,
    pub consumer: usize,
    /// Face containing the producer's delivery repeater. The consumer's
    /// reader occupies the opposite face; both repeaters face this direction.
    pub delivery_face: Facing,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompositionError {
    #[error("composition needs at least one child world")]
    NoChildren,
    #[error("child {child} overlaps child {other} at {at:?}")]
    Overlap {
        child: usize,
        other: usize,
        at: Cell,
    },
    #[error("child {child} at {at:?} violates child {other}'s halo at {other_at:?}")]
    Halo {
        child: usize,
        other: usize,
        at: Cell,
        other_at: Cell,
    },
    #[error("portal {portal} references missing child {child}")]
    MissingChild { portal: usize, child: usize },
    #[error("portal {portal} must use a horizontal delivery face")]
    VerticalPortal { portal: usize },
    #[error("portal {portal} cell {at:?} is outside the composed world")]
    OutOfBounds { portal: usize, at: Cell },
    #[error("portal {portal} expected {expected:?} at {at:?}, found {actual:?}")]
    WrongBlock {
        portal: usize,
        at: Cell,
        expected: BlockKind,
        actual: BlockKind,
    },
    #[error("portal {portal} repeater at {at:?} faces {actual:?}, expected {expected:?}")]
    WrongFacing {
        portal: usize,
        at: Cell,
        expected: Facing,
        actual: Option<Facing>,
    },
}

fn step(at: Cell, facing: Facing) -> Cell {
    let next = Position::new(at.0, at.1, at.2).offset(facing);
    (next.x, next.y, next.z)
}

fn occupied_cells(world: &World) -> impl Iterator<Item = (Cell, BlockState)> + '_ {
    let (sx, sy, sz) = world.size();
    (0..sy).flat_map(move |y| {
        (0..sz).flat_map(move |z| {
            (0..sx).filter_map(move |x| {
                let state = world.get(x, y, z);
                (state.kind != BlockKind::Air).then(|| ((x, y, z), state.clone()))
            })
        })
    })
}

fn check_repeater(
    portal: usize,
    world: &World,
    at: Cell,
    expected_facing: Facing,
) -> Result<(), CompositionError> {
    if world.index(at.0, at.1, at.2).is_none() {
        return Err(CompositionError::OutOfBounds { portal, at });
    }
    let state = world.get(at.0, at.1, at.2);
    if state.kind != BlockKind::Repeater {
        return Err(CompositionError::WrongBlock {
            portal,
            at,
            expected: BlockKind::Repeater,
            actual: state.kind,
        });
    }
    if state.facing != Some(expected_facing) {
        return Err(CompositionError::WrongFacing {
            portal,
            at,
            expected: expected_facing,
            actual: state.facing,
        });
    }
    Ok(())
}

fn check_air(portal: usize, world: &World, at: Cell) -> Result<(), CompositionError> {
    if world.index(at.0, at.1, at.2).is_none() {
        return Err(CompositionError::OutOfBounds { portal, at });
    }
    let actual = world.get(at.0, at.1, at.2).kind;
    if actual != BlockKind::Air {
        return Err(CompositionError::WrongBlock {
            portal,
            at,
            expected: BlockKind::Air,
            actual,
        });
    }
    Ok(())
}

/// Merge private child worlds in caller-supplied canonical order, validate
/// their empty halos and parent portal contracts, then seal the portal cells.
pub fn compose_chunk_worlds(
    children: &[&World],
    portals: &[PortalContract],
    minimum_size: Cell,
) -> Result<World, CompositionError> {
    if children.is_empty() {
        return Err(CompositionError::NoChildren);
    }

    let size = children.iter().fold(minimum_size, |acc, world| {
        let world = world.size();
        (acc.0.max(world.0), acc.1.max(world.1), acc.2.max(world.2))
    });
    let mut merged = World::new(size.0, size.1, size.2);
    let mut owners = HashMap::new();

    for (child, world) in children.iter().enumerate() {
        for (at, state) in occupied_cells(world) {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let neighbour = (at.0 + dx, at.1 + dy, at.2 + dz);
                        if let Some(&other) =
                            owners.get(&neighbour).filter(|&&owner| owner != child)
                        {
                            if neighbour == at {
                                return Err(CompositionError::Overlap { child, other, at });
                            }
                            return Err(CompositionError::Halo {
                                child,
                                other,
                                at,
                                other_at: neighbour,
                            });
                        }
                    }
                }
            }
            owners.insert(at, child);
            merged.set(at.0, at.1, at.2, state);
        }
    }

    for (portal, contract) in portals.iter().enumerate() {
        let Some(producer) = children.get(contract.producer) else {
            return Err(CompositionError::MissingChild {
                portal,
                child: contract.producer,
            });
        };
        let Some(consumer) = children.get(contract.consumer) else {
            return Err(CompositionError::MissingChild {
                portal,
                child: contract.consumer,
            });
        };
        if matches!(contract.delivery_face, Facing::Up | Facing::Down) {
            return Err(CompositionError::VerticalPortal { portal });
        }

        let delivery = step(contract.at, contract.delivery_face);
        let reader = step(contract.at, contract.delivery_face.opposite());
        check_repeater(portal, producer, delivery, contract.delivery_face)?;
        check_repeater(portal, consumer, reader, contract.delivery_face)?;
        check_air(portal, &merged, contract.at)?;
        for face in [
            Facing::North,
            Facing::South,
            Facing::East,
            Facing::West,
            Facing::Up,
            Facing::Down,
        ] {
            if face != contract.delivery_face && face != contract.delivery_face.opposite() {
                check_air(portal, &merged, step(contract.at, face))?;
            }
        }
        merged.set(contract.at.0, contract.at.1, contract.at.2, stone());
    }

    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_conflicts_return_typed_refusals() {
        let mut first = World::new(4, 4, 4);
        first.set(1, 1, 1, stone());

        let mut overlapping = World::new(4, 4, 4);
        overlapping.set(1, 1, 1, stone());
        assert!(matches!(
            compose_chunk_worlds(&[&first, &overlapping], &[], (4, 4, 4)),
            Err(CompositionError::Overlap { .. })
        ));

        let mut adjacent = World::new(4, 4, 4);
        adjacent.set(2, 1, 1, stone());
        assert!(matches!(
            compose_chunk_worlds(&[&first, &adjacent], &[], (4, 4, 4)),
            Err(CompositionError::Halo { .. })
        ));
    }
}
