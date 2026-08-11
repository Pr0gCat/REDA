use crate::compile::topology::Primitive;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::{BlockKind, Face, Facing};

/// The electrically distinct endpoint roles exposed by physical primitives.
///
/// Side inputs have a stable left/right name relative to the primitive's
/// Minecraft `facing` direction, rather than collapsing two real locations
/// into one ambiguous side port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortKind {
    TorchInput,
    TorchOutput,
    RepeaterRear,
    RepeaterSideLeft,
    RepeaterSideRight,
    RepeaterFront,
    ComparatorRear,
    ComparatorSideLeft,
    ComparatorSideRight,
    ComparatorFront,
    LeverOutput,
    LampInput,
}

/// One local block belonging to a physical primitive variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalBlock {
    pub position: Position,
    pub kind: BlockKind,
    pub facing: Option<Facing>,
    pub face: Option<Face>,
}

/// A typed endpoint on a local block. `direction` uses Minecraft's existing
/// coordinate convention: +X east, +Y up, and +Z south.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalPort {
    pub kind: PortKind,
    pub position: Position,
    pub direction: Facing,
}

/// One orientation of a primitive, expressed entirely in local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalVariant {
    pub blocks: &'static [LocalBlock],
    pub ports: &'static [PhysicalPort],
}

impl PhysicalVariant {
    pub fn port(&self, kind: PortKind) -> &PhysicalPort {
        self.ports
            .iter()
            .find(|port| port.kind == kind)
            .unwrap_or_else(|| panic!("variant has no {kind:?} port"))
    }

    pub fn block_at(&self, position: Position) -> &LocalBlock {
        self.blocks
            .iter()
            .find(|block| block.position == position)
            .unwrap_or_else(|| panic!("variant has no block at {position:?}"))
    }
}

const ORIGIN: Position = Position { x: 0, y: 0, z: 0 };
const NORTH: Position = Position { x: 0, y: 0, z: -1 };
const SOUTH: Position = Position { x: 0, y: 0, z: 1 };
const EAST: Position = Position { x: 1, y: 0, z: 0 };
const WEST: Position = Position { x: -1, y: 0, z: 0 };

const fn block(
    position: Position,
    kind: BlockKind,
    facing: Option<Facing>,
    face: Option<Face>,
) -> LocalBlock {
    LocalBlock {
        position,
        kind,
        facing,
        face,
    }
}

const fn port(kind: PortKind, position: Position, direction: Facing) -> PhysicalPort {
    PhysicalPort {
        kind,
        position,
        direction,
    }
}

static TORCH_NORTH_BLOCKS: [LocalBlock; 2] = [
    block(ORIGIN, BlockKind::Solid, None, None),
    block(NORTH, BlockKind::WallTorch, Some(Facing::North), None),
];
static TORCH_SOUTH_BLOCKS: [LocalBlock; 2] = [
    block(ORIGIN, BlockKind::Solid, None, None),
    block(SOUTH, BlockKind::WallTorch, Some(Facing::South), None),
];
static TORCH_EAST_BLOCKS: [LocalBlock; 2] = [
    block(ORIGIN, BlockKind::Solid, None, None),
    block(EAST, BlockKind::WallTorch, Some(Facing::East), None),
];
static TORCH_WEST_BLOCKS: [LocalBlock; 2] = [
    block(ORIGIN, BlockKind::Solid, None, None),
    block(WEST, BlockKind::WallTorch, Some(Facing::West), None),
];

static TORCH_NORTH_PORTS: [PhysicalPort; 2] = [
    port(PortKind::TorchInput, ORIGIN, Facing::South),
    port(PortKind::TorchOutput, NORTH, Facing::North),
];
static TORCH_SOUTH_PORTS: [PhysicalPort; 2] = [
    port(PortKind::TorchInput, ORIGIN, Facing::North),
    port(PortKind::TorchOutput, SOUTH, Facing::South),
];
static TORCH_EAST_PORTS: [PhysicalPort; 2] = [
    port(PortKind::TorchInput, ORIGIN, Facing::West),
    port(PortKind::TorchOutput, EAST, Facing::East),
];
static TORCH_WEST_PORTS: [PhysicalPort; 2] = [
    port(PortKind::TorchInput, ORIGIN, Facing::East),
    port(PortKind::TorchOutput, WEST, Facing::West),
];

static TORCH_VARIANTS: [PhysicalVariant; 4] = [
    PhysicalVariant {
        blocks: &TORCH_NORTH_BLOCKS,
        ports: &TORCH_NORTH_PORTS,
    },
    PhysicalVariant {
        blocks: &TORCH_SOUTH_BLOCKS,
        ports: &TORCH_SOUTH_PORTS,
    },
    PhysicalVariant {
        blocks: &TORCH_EAST_BLOCKS,
        ports: &TORCH_EAST_PORTS,
    },
    PhysicalVariant {
        blocks: &TORCH_WEST_BLOCKS,
        ports: &TORCH_WEST_PORTS,
    },
];

static REPEATER_NORTH_BLOCKS: [LocalBlock; 1] = [block(
    ORIGIN,
    BlockKind::Repeater,
    Some(Facing::North),
    None,
)];
static REPEATER_SOUTH_BLOCKS: [LocalBlock; 1] = [block(
    ORIGIN,
    BlockKind::Repeater,
    Some(Facing::South),
    None,
)];
static REPEATER_EAST_BLOCKS: [LocalBlock; 1] =
    [block(ORIGIN, BlockKind::Repeater, Some(Facing::East), None)];
static REPEATER_WEST_BLOCKS: [LocalBlock; 1] =
    [block(ORIGIN, BlockKind::Repeater, Some(Facing::West), None)];

static REPEATER_NORTH_PORTS: [PhysicalPort; 4] = [
    port(PortKind::RepeaterRear, ORIGIN, Facing::North),
    port(PortKind::RepeaterSideLeft, ORIGIN, Facing::West),
    port(PortKind::RepeaterSideRight, ORIGIN, Facing::East),
    port(PortKind::RepeaterFront, ORIGIN, Facing::South),
];
static REPEATER_SOUTH_PORTS: [PhysicalPort; 4] = [
    port(PortKind::RepeaterRear, ORIGIN, Facing::South),
    port(PortKind::RepeaterSideLeft, ORIGIN, Facing::East),
    port(PortKind::RepeaterSideRight, ORIGIN, Facing::West),
    port(PortKind::RepeaterFront, ORIGIN, Facing::North),
];
static REPEATER_EAST_PORTS: [PhysicalPort; 4] = [
    port(PortKind::RepeaterRear, ORIGIN, Facing::East),
    port(PortKind::RepeaterSideLeft, ORIGIN, Facing::North),
    port(PortKind::RepeaterSideRight, ORIGIN, Facing::South),
    port(PortKind::RepeaterFront, ORIGIN, Facing::West),
];
static REPEATER_WEST_PORTS: [PhysicalPort; 4] = [
    port(PortKind::RepeaterRear, ORIGIN, Facing::West),
    port(PortKind::RepeaterSideLeft, ORIGIN, Facing::South),
    port(PortKind::RepeaterSideRight, ORIGIN, Facing::North),
    port(PortKind::RepeaterFront, ORIGIN, Facing::East),
];

static REPEATER_VARIANTS: [PhysicalVariant; 4] = [
    PhysicalVariant {
        blocks: &REPEATER_NORTH_BLOCKS,
        ports: &REPEATER_NORTH_PORTS,
    },
    PhysicalVariant {
        blocks: &REPEATER_SOUTH_BLOCKS,
        ports: &REPEATER_SOUTH_PORTS,
    },
    PhysicalVariant {
        blocks: &REPEATER_EAST_BLOCKS,
        ports: &REPEATER_EAST_PORTS,
    },
    PhysicalVariant {
        blocks: &REPEATER_WEST_BLOCKS,
        ports: &REPEATER_WEST_PORTS,
    },
];

static LEVER_NORTH_BLOCKS: [LocalBlock; 1] = [block(
    ORIGIN,
    BlockKind::Lever,
    Some(Facing::North),
    Some(Face::Floor),
)];
static LEVER_SOUTH_BLOCKS: [LocalBlock; 1] = [block(
    ORIGIN,
    BlockKind::Lever,
    Some(Facing::South),
    Some(Face::Floor),
)];
static LEVER_EAST_BLOCKS: [LocalBlock; 1] = [block(
    ORIGIN,
    BlockKind::Lever,
    Some(Facing::East),
    Some(Face::Floor),
)];
static LEVER_WEST_BLOCKS: [LocalBlock; 1] = [block(
    ORIGIN,
    BlockKind::Lever,
    Some(Facing::West),
    Some(Face::Floor),
)];
static LEVER_NORTH_PORTS: [PhysicalPort; 1] = [port(PortKind::LeverOutput, ORIGIN, Facing::North)];
static LEVER_SOUTH_PORTS: [PhysicalPort; 1] = [port(PortKind::LeverOutput, ORIGIN, Facing::South)];
static LEVER_EAST_PORTS: [PhysicalPort; 1] = [port(PortKind::LeverOutput, ORIGIN, Facing::East)];
static LEVER_WEST_PORTS: [PhysicalPort; 1] = [port(PortKind::LeverOutput, ORIGIN, Facing::West)];
static LEVER_VARIANTS: [PhysicalVariant; 4] = [
    PhysicalVariant {
        blocks: &LEVER_NORTH_BLOCKS,
        ports: &LEVER_NORTH_PORTS,
    },
    PhysicalVariant {
        blocks: &LEVER_SOUTH_BLOCKS,
        ports: &LEVER_SOUTH_PORTS,
    },
    PhysicalVariant {
        blocks: &LEVER_EAST_BLOCKS,
        ports: &LEVER_EAST_PORTS,
    },
    PhysicalVariant {
        blocks: &LEVER_WEST_BLOCKS,
        ports: &LEVER_WEST_PORTS,
    },
];

static LAMP_BLOCKS: [LocalBlock; 1] = [block(ORIGIN, BlockKind::Lamp, None, None)];
static LAMP_NORTH_PORTS: [PhysicalPort; 1] = [port(PortKind::LampInput, ORIGIN, Facing::North)];
static LAMP_SOUTH_PORTS: [PhysicalPort; 1] = [port(PortKind::LampInput, ORIGIN, Facing::South)];
static LAMP_EAST_PORTS: [PhysicalPort; 1] = [port(PortKind::LampInput, ORIGIN, Facing::East)];
static LAMP_WEST_PORTS: [PhysicalPort; 1] = [port(PortKind::LampInput, ORIGIN, Facing::West)];
static LAMP_VARIANTS: [PhysicalVariant; 4] = [
    PhysicalVariant {
        blocks: &LAMP_BLOCKS,
        ports: &LAMP_NORTH_PORTS,
    },
    PhysicalVariant {
        blocks: &LAMP_BLOCKS,
        ports: &LAMP_SOUTH_PORTS,
    },
    PhysicalVariant {
        blocks: &LAMP_BLOCKS,
        ports: &LAMP_EAST_PORTS,
    },
    PhysicalVariant {
        blocks: &LAMP_BLOCKS,
        ports: &LAMP_WEST_PORTS,
    },
];

/// Return every orientation currently needed to place `primitive`.
pub fn variants(primitive: Primitive) -> &'static [PhysicalVariant] {
    match primitive {
        Primitive::Torch => &TORCH_VARIANTS,
        Primitive::Repeater => &REPEATER_VARIANTS,
        Primitive::Comparator => &[],
        Primitive::Lever => &LEVER_VARIANTS,
        Primitive::Lamp => &LAMP_VARIANTS,
    }
}

#[cfg(test)]
fn is_orthogonal(left: Facing, right: Facing) -> bool {
    matches!(
        (left, right),
        (Facing::North | Facing::South, Facing::East | Facing::West)
            | (Facing::East | Facing::West, Facing::North | Facing::South)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::topology::Primitive;
    use crate::redstone::simulator::position::Position;
    use crate::redstone::world::block::{BlockKind, Facing};

    #[test]
    fn a_torch_variant_exposes_input_on_its_support_and_output_at_its_torch() {
        let variant = variants(Primitive::Torch)[0];
        let input = variant.port(PortKind::TorchInput);
        let output = variant.port(PortKind::TorchOutput);

        assert_eq!(variant.block_at(input.position).kind, BlockKind::Solid);
        assert_eq!(variant.block_at(output.position).kind, BlockKind::WallTorch);
        assert_eq!(input.direction, output.direction.opposite());
        assert_eq!(output.position, input.position.offset(output.direction));
    }

    #[test]
    fn repeater_rear_and_front_ports_are_opposite_and_side_ports_are_orthogonal() {
        let variant = variants(Primitive::Repeater)[0];
        let rear = variant.port(PortKind::RepeaterRear);
        let front = variant.port(PortKind::RepeaterFront);
        let left = variant.port(PortKind::RepeaterSideLeft);
        let right = variant.port(PortKind::RepeaterSideRight);

        assert_eq!(rear.direction.opposite(), front.direction);
        assert!(is_orthogonal(rear.direction, left.direction));
        assert_eq!(left.direction.opposite(), right.direction);
    }

    #[test]
    fn a_repeater_facing_north_accepts_at_its_north_rear_and_outputs_south() {
        let variant = variants(Primitive::Repeater)[0];
        let repeater = variant.block_at(Position::new(0, 0, 0));

        assert_eq!(repeater.facing, Some(Facing::North));
        assert_eq!(
            variant.port(PortKind::RepeaterRear).direction,
            Facing::North
        );
        assert_eq!(
            variant.port(PortKind::RepeaterFront).direction,
            Facing::South
        );
    }

    #[test]
    fn lever_and_lamp_variants_expose_their_single_signal_endpoints() {
        let lever = variants(Primitive::Lever)[0];
        let lamp = variants(Primitive::Lamp)[0];

        assert_eq!(
            lever.port(PortKind::LeverOutput).position,
            Position::new(0, 0, 0)
        );
        assert_eq!(
            lamp.port(PortKind::LampInput).position,
            Position::new(0, 0, 0)
        );
    }
}
