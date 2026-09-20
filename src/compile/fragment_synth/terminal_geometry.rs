//! Shared physical input-terminal geometry for placement and seed materialisation.

use thiserror::Error;

use crate::compile::geometry::{self, Anchor, CellFacing};
use crate::compile::physical::{self, PortKind};
use crate::compile::topology::Primitive;
use crate::redstone::world::block::Facing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrimitiveInputTerminal {
    pub support: Anchor,
    pub terminal: Anchor,
    pub allowed_entry: Facing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrimitiveOutputTerminal {
    pub support: Anchor,
    pub route_anchor: Anchor,
    pub allowed_exit: Facing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum PrimitiveTerminalError {
    #[error("primitive {primitive:?} does not accept a routed input")]
    UnsupportedInput { primitive: Primitive },
    #[error("primitive {primitive:?} does not provide a routed output")]
    UnsupportedOutput { primitive: Primitive },
    #[error("primitive {primitive:?} has no physical input socket at ordinal {ordinal}")]
    InvalidInputOrdinal {
        primitive: Primitive,
        ordinal: usize,
    },
}

pub(crate) fn primitive_output_terminal(
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
) -> Result<PrimitiveOutputTerminal, PrimitiveTerminalError> {
    let variant = &physical::variants(primitive)[usize::from(facing.index())];
    let kind = match primitive {
        Primitive::Torch => PortKind::TorchOutput,
        Primitive::Repeater => PortKind::RepeaterFront,
        Primitive::Comparator | Primitive::Lamp | Primitive::Lever => {
            return Err(PrimitiveTerminalError::UnsupportedOutput { primitive });
        }
    };
    let port = variant.port(kind);
    let support = Anchor {
        x: anchor.x + port.position.x,
        y: anchor.y + port.position.y,
        z: anchor.z + port.position.z,
    };
    Ok(PrimitiveOutputTerminal {
        support,
        route_anchor: step(support, port.direction),
        allowed_exit: port.direction,
    })
}

pub(crate) fn source_escape_corridor(route_anchor: Anchor, allowed_exit: Facing) -> [Anchor; 3] {
    let exit = step(route_anchor, allowed_exit);
    let runway = step(exit, allowed_exit);
    let mouth = step(runway, allowed_exit);
    [exit, runway, mouth]
}

pub(crate) fn primitive_input_terminal(
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
    ordinal: usize,
) -> Result<PrimitiveInputTerminal, PrimitiveTerminalError> {
    let variant = &physical::variants(primitive)[usize::from(facing.index())];
    let (port, direction) = match primitive {
        Primitive::Torch => {
            let direction = geometry::input_directions(facing)
                .get(ordinal)
                .copied()
                .ok_or(PrimitiveTerminalError::InvalidInputOrdinal { primitive, ordinal })?;
            (variant.port(PortKind::TorchInput), direction)
        }
        Primitive::Repeater if ordinal == 0 => {
            let port = variant.port(PortKind::RepeaterRear);
            (port, port.direction)
        }
        Primitive::Repeater => {
            return Err(PrimitiveTerminalError::InvalidInputOrdinal { primitive, ordinal });
        }
        Primitive::Comparator | Primitive::Lamp | Primitive::Lever => {
            return Err(PrimitiveTerminalError::UnsupportedInput { primitive });
        }
    };
    let support = Anchor {
        x: anchor.x + port.position.x,
        y: anchor.y + port.position.y,
        z: anchor.z + port.position.z,
    };
    Ok(PrimitiveInputTerminal {
        support,
        terminal: step(support, direction),
        allowed_entry: direction,
    })
}

fn step(at: Anchor, direction: Facing) -> Anchor {
    match direction {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torch_ordinals_match_materialised_input_direction_order() {
        let anchor = Anchor { x: 4, y: 2, z: 7 };
        let terminals = (0..3)
            .map(|ordinal| {
                primitive_input_terminal(Primitive::Torch, CellFacing::NORTH, anchor, ordinal)
                    .unwrap()
                    .terminal
            })
            .collect::<Vec<_>>();

        assert_eq!(
            terminals,
            [
                Anchor { x: 3, y: 2, z: 7 },
                Anchor { x: 5, y: 2, z: 7 },
                Anchor { x: 4, y: 2, z: 8 },
            ]
        );
    }

    #[test]
    fn impossible_input_ordinals_are_rejected_instead_of_falling_back() {
        let anchor = Anchor { x: 0, y: 1, z: 0 };
        assert_eq!(
            primitive_input_terminal(Primitive::Torch, CellFacing::NORTH, anchor, 3),
            Err(PrimitiveTerminalError::InvalidInputOrdinal {
                primitive: Primitive::Torch,
                ordinal: 3,
            })
        );
        assert_eq!(
            primitive_input_terminal(Primitive::Repeater, CellFacing::EAST, anchor, 1),
            Err(PrimitiveTerminalError::InvalidInputOrdinal {
                primitive: Primitive::Repeater,
                ordinal: 1,
            })
        );
    }

    #[test]
    fn output_escape_corridor_follows_the_physical_port_direction() {
        let anchor = Anchor { x: 4, y: 2, z: 7 };
        let output =
            primitive_output_terminal(Primitive::Torch, CellFacing::NORTH, anchor).unwrap();

        // The torch at z=7 drives the block at z=6; the route's first dust
        // sits past that block at z=5 and the corridor runs on from there.
        assert_eq!(output.support, Anchor { x: 4, y: 2, z: 6 });
        assert_eq!(output.route_anchor, Anchor { x: 4, y: 2, z: 5 });
        assert_eq!(output.allowed_exit, Facing::North);
        assert_eq!(
            source_escape_corridor(output.route_anchor, output.allowed_exit),
            [
                Anchor { x: 4, y: 2, z: 4 },
                Anchor { x: 4, y: 2, z: 3 },
                Anchor { x: 4, y: 2, z: 2 },
            ]
        );
    }
}
