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

/// 一個方向上的連接目標，最多兩個。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Connections {
    items: [Option<Position>; 2],
}

impl Connections {
    pub fn none() -> Self {
        Connections { items: [None, None] }
    }

    pub fn one(pos: Position) -> Self {
        Connections { items: [Some(pos), None] }
    }

    fn push(&mut self, pos: Position) {
        if self.items[0].is_none() {
            self.items[0] = Some(pos);
        } else if self.items[1].is_none() {
            self.items[1] = Some(pos);
        }
    }

    pub fn is_empty(self) -> bool {
        self.items[0].is_none()
    }

    /// 走訪所有連接目標。
    pub fn iter(self) -> impl Iterator<Item = Position> {
        self.items.into_iter().flatten()
    }
}

/// 從 `from` 的紅石粉往 `direction` 看，連到哪些紅石粉。
///
/// 回傳最多兩個位置。一個方向可以同時有兩個合法目標嗎？在原版裡不會 ——
/// 往上要求水平鄰居**是**導體，往下要求它**不是**，兩條互斥。但回傳集合
/// 而非單一值，是為了讓這個不變式由型別而非巧合來保證：先前的 `Option`
/// 版本在兩條規則同時成立時只回傳其中一個，造成兩條相鄰的線之間出現
/// **單向**的邊 —— 而物理上沒有這種東西。
pub fn dust_connections(world: &World, from: Position, direction: Facing) -> Connections {
    let mut found = Connections::none();
    let neighbour = from.offset(direction);

    // 同層
    if is_dust(world, neighbour) {
        found.push(neighbour);
    }

    let neighbour_conducts = is_conductive(world, neighbour);

    // 往上：水平鄰居**必須是導體**（訊號沿著它爬），且本格正上方不能是導體（會擋住）
    if neighbour_conducts && is_dust(world, neighbour.up()) && !is_conductive(world, from.up()) {
        found.push(neighbour.up());
    }

    // 往下：水平鄰居**必須不是導體**（否則擋住），下方才連得到
    if !neighbour_conducts && is_dust(world, neighbour.down()) {
        found.push(neighbour.down());
    }

    found
}

/// 單一目標版本，保留給只需要「有沒有連接」的呼叫端。
///
/// **新的程式碼應該用 `dust_connections`** —— 這個版本在一個方向有多個
/// 目標時只回傳第一個。
pub fn dust_connects(world: &World, from: Position, direction: Facing) -> Option<Position> {
    dust_connections(world, from, direction).iter().next()
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

    #[test]
    fn signal_does_not_climb_a_non_conductive_block() {
        // 玻璃承載得了紅石粉，但不導電 —— 訊號爬不上去
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, glass());
        w.set(2, 2, 2, dust());

        let from = Position::new(1, 1, 2);
        assert!(
            dust_connections(&w, from, Facing::East)
                .iter()
                .all(|p| p != Position::new(2, 2, 2)),
            "dust must not climb glass -- vanilla looks down instead"
        );
    }

    #[test]
    fn dust_does_not_connect_across_thin_air() {
        // 鄰居那格是空氣，斜上方有粉 —— 遊戲裡擺不出來，但載入的檔案可能有
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 2, 2, dust()); // 浮空，(2,1,2) 是空氣

        let from = Position::new(1, 1, 2);
        assert!(
            dust_connections(&w, from, Facing::East)
                .iter()
                .all(|p| p != Position::new(2, 2, 2)),
            "air is not a conductor, so nothing climbs it"
        );
    }

    #[test]
    fn the_up_and_down_rules_are_mutually_exclusive() {
        // 先前的 Option 版本在兩條規則同時成立時只回傳一個，造成單向的邊。
        // 加上「往上要求鄰居導電」之後兩條互斥，一個方向最多一個目標。
        let mut w = World::new(6, 6, 6);
        place_dust_on_stone(&mut w, 1, 1, 2); // 石頭 y=1、粉 y=2
        w.set(2, 2, 2, glass());
        w.set(2, 3, 2, dust()); // 玻璃上方
        w.set(2, 0, 2, stone());
        w.set(2, 1, 2, dust()); // 玻璃下方

        let from = Position::new(1, 2, 2);
        let found = dust_connections(&w, from, Facing::East);
        let targets: Vec<Position> = found.iter().collect();

        assert_eq!(
            targets.len(),
            1,
            "with a non-conductive neighbour only the down-rule may fire, got {targets:?}"
        );
        assert_eq!(targets[0], Position::new(2, 1, 2), "it must be the lower wire");
    }

    #[test]
    fn adjacency_is_symmetric() {
        // 兩條相鄰的線之間不能有單向的邊
        let mut w = World::new(8, 8, 8);
        place_dust_on_stone(&mut w, 2, 1, 3);
        w.set(3, 2, 3, stone());
        w.set(3, 3, 3, dust());

        let lower = Position::new(2, 2, 3);
        let upper = Position::new(3, 3, 3);

        let forward: Vec<Position> = dust_connections(&w, lower, Facing::East).iter().collect();
        let backward: Vec<Position> = dust_connections(&w, upper, Facing::West).iter().collect();

        assert_eq!(
            forward.contains(&upper),
            backward.contains(&lower),
            "connection must be mutual: forward={forward:?} backward={backward:?}"
        );
    }
}
