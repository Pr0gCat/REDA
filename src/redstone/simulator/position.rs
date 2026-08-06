//! 座標與方向的基本運算。
//!
//! Minecraft 的座標系：+X 東、+Y 上、+Z 南。

use crate::redstone::world::block::Facing;

/// 世界座標。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// 四個水平方向，順序固定。
///
/// 順序固定是刻意的 —— 模擬器對鄰居的走訪順序必須可重現，否則同一個電路
/// 每次跑出來的結果可能不同。
pub const HORIZONTAL: [Facing; 4] = [Facing::North, Facing::South, Facing::East, Facing::West];

/// 六個方向，順序固定。
pub const ALL_SIX: [Facing; 6] = [
    Facing::North,
    Facing::South,
    Facing::East,
    Facing::West,
    Facing::Up,
    Facing::Down,
];

impl Position {
    pub fn new(x: i32, y: i32, z: i32) -> Self {
        Position { x, y, z }
    }

    /// 往指定方向移動一格。
    pub fn offset(self, facing: Facing) -> Position {
        match facing {
            Facing::North => Position::new(self.x, self.y, self.z - 1),
            Facing::South => Position::new(self.x, self.y, self.z + 1),
            Facing::East => Position::new(self.x + 1, self.y, self.z),
            Facing::West => Position::new(self.x - 1, self.y, self.z),
            Facing::Up => Position::new(self.x, self.y + 1, self.z),
            Facing::Down => Position::new(self.x, self.y - 1, self.z),
        }
    }

    pub fn up(self) -> Position {
        self.offset(Facing::Up)
    }

    pub fn down(self) -> Position {
        self.offset(Facing::Down)
    }
}

/// 相反方向。
pub fn opposite(facing: Facing) -> Facing {
    facing.opposite()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_moves_one_block_in_each_direction() {
        let p = Position::new(10, 20, 30);
        assert_eq!(p.offset(Facing::North), Position::new(10, 20, 29));
        assert_eq!(p.offset(Facing::South), Position::new(10, 20, 31));
        assert_eq!(p.offset(Facing::East), Position::new(11, 20, 30));
        assert_eq!(p.offset(Facing::West), Position::new(9, 20, 30));
        assert_eq!(p.offset(Facing::Up), Position::new(10, 21, 30));
        assert_eq!(p.offset(Facing::Down), Position::new(10, 19, 30));
    }

    #[test]
    fn up_and_down_are_shorthand_for_offset() {
        let p = Position::new(1, 2, 3);
        assert_eq!(p.up(), p.offset(Facing::Up));
        assert_eq!(p.down(), p.offset(Facing::Down));
    }

    #[test]
    fn opposite_reverses_every_direction() {
        for f in ALL_SIX {
            assert_eq!(opposite(opposite(f)), f);
            assert_ne!(opposite(f), f);
        }
    }

    #[test]
    fn horizontal_holds_exactly_the_four_compass_directions() {
        assert_eq!(HORIZONTAL.len(), 4);
        assert!(!HORIZONTAL.contains(&Facing::Up));
        assert!(!HORIZONTAL.contains(&Facing::Down));
    }
}
