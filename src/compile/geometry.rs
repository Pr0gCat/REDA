//! One place for the geometry a gate's cell has: which faces its inputs
//! arrive on, which face its output leaves by, and which `physical` variant a
//! facing selects.
//!
//! Before this, that was six modules. Five named `INPUT_DIRECTIONS` and
//! `OUTPUT_DIRECTION`; the sixth, `topology`, hardcoded the consequence as
//! footprint-area tables with no symbol to grep for. None of them could have
//! been asked what a gate turned east looks like, because none of them could
//! be asked anything -- they were constants.

use crate::redstone::simulator::position::{Position, HORIZONTAL};
use crate::redstone::world::block::Facing;

/// One of the four horizontal orientations a gate cell can be built in.
///
/// A `u8` index rather than a [`Facing`], for two reasons. `Facing` has `Up`
/// and `Down`, and a gate cell turned onto its side is not a thing this
/// compiler can build. And `physical::variants` is a four-element array
/// indexed positionally, which is what this index indexes -- the linkage
/// `every_variant_faces_the_facing_its_index_claims` proves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct CellFacing(u8);

impl CellFacing {
    /// What every gate this compiler has placed so far is built as.
    pub const NORTH: CellFacing = CellFacing(0);
    pub const SOUTH: CellFacing = CellFacing(1);
    pub const EAST: CellFacing = CellFacing(2);
    pub const WEST: CellFacing = CellFacing(3);

    /// The facing `physical::variants`' `index`-th entry is built for, or
    /// `None` for an index no variant array has.
    pub fn from_index(index: u8) -> Option<CellFacing> {
        (usize::from(index) < HORIZONTAL.len()).then_some(CellFacing(index))
    }

    /// Which entry of `physical::variants` this selects.
    pub fn index(self) -> u8 {
        self.0
    }

    /// The compass direction this cell's output leaves in.
    pub fn direction(self) -> Facing {
        HORIZONTAL[usize::from(self.0)]
    }
}

/// `offset`, written for a north-facing cell, read on one turned to `facing`.
///
/// The turn is about Y, so heights are untouched: a torch's support stays
/// beside its torch and a repeater's floor stays under it whichever way the
/// pair is turned.
pub fn rotate(offset: (i32, i32, i32), facing: CellFacing) -> (i32, i32, i32) {
    let (x, y, z) = offset;
    match facing.direction() {
        Facing::North => (x, y, z),
        Facing::South => (-x, y, -z),
        Facing::East => (-z, y, x),
        Facing::West => (z, y, -x),
        // `CellFacing` indexes `HORIZONTAL`, which has neither.
        Facing::Up | Facing::Down => unreachable!("CellFacing is horizontal by construction"),
    }
}

/// `direction`, as read on a cell turned to `facing` from north.
pub fn turn(direction: Facing, facing: CellFacing) -> Facing {
    let unit = Position::new(0, 0, 0).offset(direction);
    match rotate((unit.x, unit.y, unit.z), facing) {
        (0, 0, -1) => Facing::North,
        (0, 0, 1) => Facing::South,
        (1, 0, 0) => Facing::East,
        (-1, 0, 0) => Facing::West,
        (0, 1, 0) => Facing::Up,
        (0, -1, 0) => Facing::Down,
        other => unreachable!("turning a unit vector gave {other:?}"),
    }
}

/// Every face a gate cell of this facing accepts an input on, in declared
/// input order.
///
/// Three, because the fourth horizontal face is the output's. A repeater can
/// only drive the block directly in front of it, and it has to stand on the
/// ground beside the support -- so an input can only ever approach along a
/// compass direction, and one of the four is already spoken for. Three is
/// therefore the hardware maximum fan-in every NOR gate this compiler places
/// has (see `place_nor_gate`'s own `assert!`), not a placeholder for something
/// larger later: a fourth input would need a face a repeater cannot stand on.
///
/// Derived by turning north's answer rather than tabulated per facing, so
/// there is one place to be wrong instead of four.
pub fn input_directions(facing: CellFacing) -> [Facing; 3] {
    const FACING_NORTH: [Facing; 3] = [Facing::West, Facing::East, Facing::South];
    FACING_NORTH.map(|direction| turn(direction, facing))
}

/// The face a gate cell of this facing sends its output out of.
pub fn output_direction(facing: CellFacing) -> Facing {
    facing.direction()
}

/// The cells a gate at `origin` accepts its declared inputs in, in declared
/// input order.
///
/// Six modules used to compute this, each from the same constant and each
/// slightly differently -- one off a support, one off a junction, one from a
/// `NorCell`'s `input_offsets`. They are the same cells.
pub fn gate_sockets(origin: Position, arity: usize, facing: CellFacing) -> Vec<Position> {
    input_directions(facing)
        .iter()
        .take(arity)
        .map(|&direction| origin.offset(direction))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::physical;
    use crate::compile::topology::Primitive;
    use crate::redstone::world::block::BlockKind;

    /// North is what every gate this compiler has ever placed is built as, so
    /// north has to be the identity and the default or Stage 0 changes
    /// behaviour it promised not to.
    #[test]
    fn north_is_the_identity_and_the_default() {
        assert_eq!(CellFacing::default(), CellFacing::NORTH);
        assert_eq!(output_direction(CellFacing::NORTH), Facing::North);
        assert_eq!(
            input_directions(CellFacing::NORTH),
            [Facing::West, Facing::East, Facing::South]
        );
        assert_eq!(rotate((1, 2, 3), CellFacing::NORTH), (1, 2, 3));
    }

    /// The fourth horizontal face is the output's, whichever way the cell is
    /// turned -- which is the whole reason a gate takes at most three inputs.
    #[test]
    fn a_cell_never_takes_an_input_from_the_face_its_output_leaves() {
        for index in 0..4u8 {
            let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
            let out = output_direction(facing);
            assert!(
                !input_directions(facing).contains(&out),
                "{facing:?} takes an input from {out:?}, where its output goes"
            );
        }
    }

    /// Turning a cell twice by the same quarter turn is turning it half way
    /// round, which is the cheapest check that `rotate` is a rotation and not
    /// four hand-written tables that happen to look plausible.
    #[test]
    fn turning_east_twice_is_turning_south_once() {
        for offset in [(1, 0, 0), (0, 0, -1), (-1, 2, 3)] {
            let twice = rotate(rotate(offset, CellFacing::EAST), CellFacing::EAST);
            assert_eq!(twice, rotate(offset, CellFacing::SOUTH), "for {offset:?}");
        }
    }

    /// `physical.rs` declares its variant arrays in `HORIZONTAL` order and
    /// says so nowhere. This is that statement, made checkable: relaxation
    /// picks a facing by index, and an index that means something else would
    /// build a gate pointing the wrong way with nothing to catch it.
    #[test]
    fn every_variant_faces_the_facing_its_index_claims() {
        for index in 0..4u8 {
            let facing = CellFacing::from_index(index).expect("0..4 is horizontal");
            let slot = usize::from(index);

            let torch = &physical::variants(Primitive::Torch)[slot];
            let torch_at = Position::new(0, 0, 0).offset(facing.direction());
            assert_eq!(
                torch.block_at(torch_at).kind,
                BlockKind::WallTorch,
                "variants(Torch)[{index}] has no torch at {torch_at:?}"
            );

            let repeater = &physical::variants(Primitive::Repeater)[slot];
            assert_eq!(
                repeater.block_at(Position::new(0, 0, 0)).facing,
                Some(facing.direction()),
                "variants(Repeater)[{index}] is not built facing {:?}",
                facing.direction()
            );
        }
    }
}
