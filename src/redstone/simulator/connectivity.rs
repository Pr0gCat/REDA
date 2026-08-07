//! 紅石粉的連接判定。
//!
//! 紅石粉不只連接同層的鄰居 —— 它會沿方塊爬上爬下，而能不能爬取決於水平
//! 鄰居那格頂面**是不是完整實心方形面**（能不能撐住紅石粉站上去）：
//!
//! - **往上**：目標在斜上方時，水平鄰居的頂面必須完整（訊號沿著它爬），
//!   且本格的正上方不能是導體（否則被擋住，見 `dust_climb_blocked_by_conductive_above_source`）
//! - **往下**：目標在斜下方時，水平鄰居的頂面若是完整的就擋住
//!
//! 這條規則跟「導不導電」是兩件獨立的事：玻璃頂面完整卻不導電，遊戲仍然
//! 讓紅石粉爬上玻璃（`conformance/results/1.20.1.json` 的 `conducts_glass`
//! 探針證實）；蜂蜜塊反過來，看起來是完整方塊，頂面卻連紅石粉都放不上去，
//! 兩種情形都不能用「導不導電」去判斷能不能爬。
//!
//! 往上與往下兩條的方向不對稱，是最容易寫錯的地方，所以規則集中在這個檔案。

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

/// 某格頂面是不是完整實心方形面 —— 紅石粉爬升/下降時，水平鄰居撐不撐得住它。
///
/// 這跟 `is_conductive` 是兩個獨立的屬性，不能互相替代：玻璃頂面完整卻
/// 不導電，紅石粉照樣爬得上去；蜂蜜塊看起來是完整立方體，頂面卻連紅石粉
/// 都放不上去（`java_1_20::SUPPORTS_NOTHING` 已經把它排除在外）。爬升／
/// 下降規則問的是「站不站得住」，不是「導不導電」。
fn supports_dust_step(world: &World, pos: Position) -> bool {
    flags_of(world.get(pos.x, pos.y, pos.z)).can_carry_dust()
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
/// 往上要求水平鄰居的頂面**能**撐住紅石粉，往下要求它**不能**，兩條互斥。但回傳集合
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

    let neighbour_steps = supports_dust_step(world, neighbour);

    // 往上：水平鄰居的頂面**必須是完整實心方形面**（訊號沿著它爬），且本格
    // 正上方不能是導體（會擋住）
    if neighbour_steps && is_dust(world, neighbour.up()) && !is_conductive(world, from.up()) {
        found.push(neighbour.up());
    }

    // 往下：水平鄰居的頂面**必須不是完整實心方形面**（否則擋住），下方才連得到
    if !neighbour_steps && is_dust(world, neighbour.down()) {
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

/// `dust_connections`, read in reverse: not "does dust already there join
/// `from`'s net", but "which cells, if they held dust, *would* join it".
///
/// This is the derivation a spacing/keep-out model needs. `dust_connections`
/// requires its same-layer, climb and descend targets to already be
/// `RedstoneWire`; `dust_reach` drops exactly that one requirement and keeps
/// every other condition identical -- same-layer is unconditional in both,
/// and climb/descend still gate on the same neighbour's ability to support a
/// dust step and on `from.up()`'s conductivity, because that gating is about
/// the *geometry already in the world* (a support block, an open ceiling),
/// not about whether the target itself happens to be dust yet.
///
/// Climb and descend stay mutually exclusive for the same reason they are in
/// `dust_connections`: they read the opposite polarity of the same
/// `neighbour` cell's ability to support a dust step (`supports_dust_step`,
/// not conductivity -- see this file's module doc comment). So this never
/// returns more than the same two slots `Connections` already provides for
/// `dust_connections`.
pub fn dust_reach(world: &World, from: Position, direction: Facing) -> Connections {
    let mut reach = Connections::none();
    let neighbour = from.offset(direction);

    // same layer: nothing gates this in `dust_connections` either.
    reach.push(neighbour);

    let neighbour_steps = supports_dust_step(world, neighbour);
    if neighbour_steps && !is_conductive(world, from.up()) {
        reach.push(neighbour.up());
    }
    if !neighbour_steps {
        reach.push(neighbour.down());
    }

    reach
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

    /// Looks like a full cube (same render model as glass) but genuinely
    /// supports nothing -- not dust, not a lever, not a button. Confirmed
    /// against the game, not reasoned about: see `SUPPORTS_NOTHING`'s doc
    /// comment in `java_1_20.rs`.
    fn honey_block() -> BlockState {
        block("minecraft:honey_block", BlockKind::Other)
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
    fn dust_climbs_a_full_but_non_conductive_step() {
        // Confirmed against a real 1.20.1 server (`conformance/results/1.20.1.json`,
        // probes `conducts_glass` and `dust_climb_blocked_by_nonconductive_step`):
        // dust climbs glass even though glass does not conduct. Climbing is
        // gated on the neighbour's ability to support a dust step (a full
        // top face), not on conductivity -- an earlier version of this test
        // asserted the opposite and was wrong.
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, glass());
        w.set(2, 2, 2, dust());

        let from = Position::new(1, 1, 2);
        assert!(
            dust_connections(&w, from, Facing::East)
                .iter()
                .any(|p| p == Position::new(2, 2, 2)),
            "dust must climb glass -- vanilla does, despite glass being non-conductive"
        );
    }

    #[test]
    fn dust_does_not_climb_a_step_that_supports_nothing() {
        // A honey block looks like a full cube but genuinely cannot hold
        // dust (see `honey_block`'s doc comment) -- unlike glass, which is
        // non-conductive but still climbable, honey block must not be
        // climbable at all.
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, honey_block());
        w.set(2, 2, 2, dust());

        let from = Position::new(1, 1, 2);
        assert!(
            dust_connections(&w, from, Facing::East)
                .iter()
                .all(|p| p != Position::new(2, 2, 2)),
            "dust must not climb a honey block -- it has no full top face to stand on"
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
    fn the_up_and_down_rules_are_mutually_exclusive_via_a_supportive_step() {
        // 先前的 Option 版本在兩條規則同時成立時只回傳一個，造成單向的邊。
        // 加上「往上要求鄰居能撐住紅石粉」之後兩條互斥，一個方向最多一個目標。
        // The neighbour here supports dust (stone), so only the up-rule fires.
        let mut w = World::new(6, 6, 6);
        place_dust_on_stone(&mut w, 1, 1, 2); // 石頭 y=1、粉 y=2
        w.set(2, 2, 2, stone());
        w.set(2, 3, 2, dust()); // 石頭上方
        w.set(2, 0, 2, stone());
        w.set(2, 1, 2, dust()); // 石頭下方 -- unreachable: the neighbour supports a step, so descend cannot also fire

        let from = Position::new(1, 2, 2);
        let found = dust_connections(&w, from, Facing::East);
        let targets: Vec<Position> = found.iter().collect();

        assert_eq!(
            targets.len(),
            1,
            "with a step-supporting neighbour only the up-rule may fire, got {targets:?}"
        );
        assert_eq!(targets[0], Position::new(2, 3, 2), "it must be the upper wire");
    }

    #[test]
    fn the_up_and_down_rules_are_mutually_exclusive_via_a_non_supportive_step() {
        // Mirror of the test above: a neighbour that supports nothing at all
        // (honey block -- see its doc comment) can only ever satisfy the
        // down-rule, never the up-rule, even though there is dust sitting
        // right above it too.
        let mut w = World::new(6, 6, 6);
        place_dust_on_stone(&mut w, 1, 1, 2); // 石頭 y=1、粉 y=2
        w.set(2, 2, 2, honey_block());
        w.set(2, 3, 2, dust()); // 蜂蜜塊上方 -- unreachable: the neighbour supports nothing, so climb cannot fire
        w.set(2, 0, 2, stone());
        w.set(2, 1, 2, dust()); // 蜂蜜塊下方

        let from = Position::new(1, 2, 2);
        let found = dust_connections(&w, from, Facing::East);
        let targets: Vec<Position> = found.iter().collect();

        assert_eq!(
            targets.len(),
            1,
            "with a non-supportive neighbour only the down-rule may fire, got {targets:?}"
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

    #[test]
    fn dust_reach_finds_an_empty_same_layer_neighbour_that_dust_connections_would_miss() {
        // `dust_connections` requires the target to already be dust, so an
        // empty neighbour is invisible to it -- but it would join the
        // network the moment something filled it in, which is exactly what
        // `dust_reach` has to report. The neighbour is open air, which also
        // satisfies the descend rule's "not conductive" gate, so the cell
        // below it is a second, independent reach target -- not a copy of
        // the same-layer one.
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);

        let from = Position::new(1, 1, 2);
        assert!(dust_connections(&w, from, Facing::East).is_empty());
        let reach: Vec<Position> = dust_reach(&w, from, Facing::East).iter().collect();
        assert!(reach.contains(&Position::new(2, 1, 2)), "reach was {reach:?}");
    }

    #[test]
    fn dust_reach_finds_an_open_climb_target() {
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, stone());
        // (2, 2, 2) left empty -- nothing sits there yet, but dust would climb into it.

        let from = Position::new(1, 1, 2);
        let reach: Vec<Position> = dust_reach(&w, from, Facing::East).iter().collect();
        assert!(reach.contains(&Position::new(2, 2, 2)), "reach was {reach:?}");
    }

    #[test]
    fn dust_reach_finds_an_open_descend_target() {
        let mut w = World::new(5, 5, 5);
        w.set(1, 1, 2, stone());
        w.set(1, 2, 2, dust());
        w.set(2, 0, 2, stone());
        // (2, 1, 2) left empty -- nothing sits there yet, but dust would descend into it.

        let from = Position::new(1, 2, 2);
        let reach: Vec<Position> = dust_reach(&w, from, Facing::East).iter().collect();
        assert!(reach.contains(&Position::new(2, 1, 2)), "reach was {reach:?}");
    }

    #[test]
    fn dust_reach_climbs_a_full_but_non_conductive_neighbour() {
        // Same shape as `dust_climbs_a_full_but_non_conductive_step`, but for
        // the empty-neighbour case: glass carries the climb even as a
        // hypothetical, since `dust_reach` gates on the same
        // `supports_dust_step` property `dust_connections` does.
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, glass());
        // (2, 2, 2) left empty.

        let from = Position::new(1, 1, 2);
        let reach: Vec<Position> = dust_reach(&w, from, Facing::East).iter().collect();
        assert!(reach.contains(&Position::new(2, 2, 2)), "reach was {reach:?}");
    }

    #[test]
    fn dust_reach_does_not_climb_a_neighbour_that_supports_nothing() {
        // Mirror of the test above: a honey block cannot carry the climb
        // even as a hypothetical, since it has no full top face at all.
        let mut w = World::new(5, 5, 5);
        place_dust_on_stone(&mut w, 1, 0, 2);
        w.set(2, 1, 2, honey_block());
        // (2, 2, 2) left empty.

        let from = Position::new(1, 1, 2);
        let reach: Vec<Position> = dust_reach(&w, from, Facing::East).iter().collect();
        assert!(!reach.contains(&Position::new(2, 2, 2)), "reach was {reach:?}");
    }
}
