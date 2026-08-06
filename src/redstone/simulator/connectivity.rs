//! 紅石粉的連接判定。
//!
//! 紅石粉不只連接同層的鄰居 —— 它會沿方塊爬上爬下，而能不能爬取決於
//! 中間那格是不是**導體**：
//!
//! - **往上**：目標在斜上方時，本格的正上方若是導體就擋住
//! - **往下**：目標在斜下方時，水平鄰居那格若是導體就擋住
//!
//! 這兩條的方向不對稱，是最容易寫錯的地方，所以規則集中在這個檔案。

use crate::redstone::rules::taxonomy::flags_of;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

/// 某格是不是紅石粉。
fn is_dust(world: &World, pos: Position) -> bool {
    world.get(pos.x, pos.y, pos.z).kind == BlockKind::RedstoneWire
}

/// 某格是不是導體。
fn is_conductive(world: &World, pos: Position) -> bool {
    flags_of(world.get(pos.x, pos.y, pos.z)).is_conductive()
}

/// 從 `from` 的紅石粉往 `direction` 看，連到哪一格紅石粉。
///
/// 回傳 `None` 表示該方向沒有連接。
pub fn dust_connects(world: &World, from: Position, direction: Facing) -> Option<Position> {
    let neighbour = from.offset(direction);

    // 同層
    if is_dust(world, neighbour) {
        return Some(neighbour);
    }

    // 往上：本格正上方若是導體就擋住
    let above_neighbour = neighbour.up();
    if is_dust(world, above_neighbour) && !is_conductive(world, from.up()) {
        return Some(above_neighbour);
    }

    // 往下：水平鄰居那格若是導體就擋住
    let below_neighbour = neighbour.down();
    if is_dust(world, below_neighbour) && !is_conductive(world, neighbour) {
        return Some(below_neighbour);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState};

    fn block(name: &str, kind: BlockKind) -> BlockState {
        let mut b = BlockState::air();
        b.kind = kind;
        b.name = name.to_string();
        b
    }

    fn dust() -> BlockState {
        block("minecraft:redstone_wire", BlockKind::RedstoneWire)
    }

    fn stone() -> BlockState {
        block("minecraft:stone", BlockKind::Solid)
    }

    fn glass() -> BlockState {
        block("minecraft:glass", BlockKind::Glass)
    }

    /// 在 `pos` 放一塊石頭當載體，並在其上放紅石粉。
    fn place_dust_on_stone(world: &mut World, x: i32, y: i32, z: i32) {
        world.set(x, y, z, stone());
        world.set(x, y + 1, z, dust());
    }

    #[test]
    fn dust_connects_to_dust_on_the_same_level() {
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        place_dust_on_stone(&mut w, 2, 0, 2);

        let from = Position::new(1, 1, 2);
        assert_eq!(
            dust_connects(&w, from, Facing::East),
            Some(Position::new(2, 1, 2))
        );
    }

    #[test]
    fn dust_does_not_connect_to_empty_space() {
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);

        let from = Position::new(1, 1, 2);
        assert_eq!(dust_connects(&w, from, Facing::East), None);
    }

    #[test]
    fn dust_climbs_up_when_the_block_above_is_not_conductive() {
        let mut w = World::new(5, 5, 5);
        // 低處：石頭 y=0，粉 y=1
        place_dust_on_stone(&mut w, 1, 0, 2);
        // 高處：石頭 y=1，粉 y=2
        w.set(2, 1, 2, stone());
        w.set(2, 2, 2, dust());
        // 低處粉的正上方留空 —— 允許爬升

        let from = Position::new(1, 1, 2);
        assert_eq!(
            dust_connects(&w, from, Facing::East),
            Some(Position::new(2, 2, 2)),
            "dust should climb when nothing conductive blocks it"
        );
    }

    #[test]
    fn a_conductive_block_above_cuts_the_upward_connection() {
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, stone());
        w.set(2, 2, 2, dust());
        // 低處粉的正上方放導體 —— 切斷爬升
        w.set(1, 2, 2, stone());

        let from = Position::new(1, 1, 2);
        assert_eq!(
            dust_connects(&w, from, Facing::East),
            None,
            "a conductive block above must cut the upward connection"
        );
    }

    #[test]
    fn a_non_conductive_block_above_does_not_cut_the_upward_connection() {
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, stone());
        w.set(2, 2, 2, dust());
        // 玻璃不導電 —— 不切斷
        w.set(1, 2, 2, glass());

        let from = Position::new(1, 1, 2);
        assert_eq!(
            dust_connects(&w, from, Facing::East),
            Some(Position::new(2, 2, 2)),
            "glass is not conductive and must not cut the connection"
        );
    }

    #[test]
    fn dust_descends_when_the_neighbour_is_not_conductive() {
        let mut w = World::new(5, 5, 5);
        // 高處：石頭 y=1，粉 y=2
        w.set(1, 1, 2, stone());
        w.set(1, 2, 2, dust());
        // 低處：石頭 y=0，粉 y=1
        place_dust_on_stone(&mut w, 2, 0, 2);
        // 位置 (2,2,2) 留空 —— 允許下降

        let from = Position::new(1, 2, 2);
        assert_eq!(
            dust_connects(&w, from, Facing::East),
            Some(Position::new(2, 1, 2)),
            "dust should descend into the lower wire"
        );
    }

    #[test]
    fn a_conductive_neighbour_cuts_the_downward_connection() {
        let mut w = World::new(5, 5, 5);
        w.set(1, 1, 2, stone());
        w.set(1, 2, 2, dust());
        place_dust_on_stone(&mut w, 2, 0, 2);
        // 鄰居位置放導體 —— 切斷下降
        w.set(2, 2, 2, stone());

        let from = Position::new(1, 2, 2);
        assert_eq!(
            dust_connects(&w, from, Facing::East),
            None,
            "a conductive neighbour must cut the downward connection"
        );
    }
}
