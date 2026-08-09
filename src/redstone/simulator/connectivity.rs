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

use crate::redstone::rules::taxonomy::{accepts_dust_connection, flags_of};
use crate::redstone::simulator::position::{Position, HORIZONTAL};
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

/// Which of a dust cell's four horizontal sides are attached, in the sense
/// vanilla's `redstone_wire` blockstate means by `north`/`south`/`east`/
/// `west` being something other than `none`.
///
/// Indexed by `HORIZONTAL`'s order. Not a connectivity question -- two dust
/// cells that are electrically one node is `dust_connections`' business --
/// but a *shape* question, and shape is what decides which blocks the dust
/// powers (`propagate::dust_power_toward`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DustSides([bool; 4]);

impl DustSides {
    /// No side attached at all.
    pub const NONE: DustSides = DustSides([false; 4]);

    #[inline]
    fn index(direction: Facing) -> Option<usize> {
        HORIZONTAL.iter().position(|&d| d == direction)
    }

    /// Is the side facing `direction` attached? Vertical directions are
    /// never attached -- dust has no vertical sides.
    #[inline]
    pub fn has(self, direction: Facing) -> bool {
        Self::index(direction).is_some_and(|i| self.0[i])
    }

    /// Is every side unattached?
    #[inline]
    pub fn is_empty(self) -> bool {
        !self.0.iter().any(|&b| b)
    }
}

/// Whether the dust at `from` attaches to whatever lies in `direction`.
///
/// Deliberately built on `dust_connections` rather than restating the
/// climb/descend geometry, so there is exactly one place in this crate that
/// decides when dust reaches over a step. The one thing `dust_connections`
/// cannot answer is a side attached to something that is not dust -- a
/// redstone block, a torch, a repeater lying along the axis -- so that is
/// the only case added here, via `accepts_dust_connection`.
pub fn dust_side_connected(world: &World, from: Position, direction: Facing) -> bool {
    if !dust_connections(world, from, direction).is_empty() {
        return true;
    }
    let neighbour = from.offset(direction);
    accepts_dust_connection(world.get(neighbour.x, neighbour.y, neighbour.z), direction)
}

/// The dust cell's shape at `pos`, after vanilla's rule that a wire attached
/// on **one axis only** is filled out into a straight run.
///
/// That fill is not cosmetic. A run whose last cell touches nothing but the
/// cell behind it still has a direction, and still powers the block in front
/// of it, precisely because vanilla gives it the missing far side. Measured
/// live: joining a lone dust cell from one side leaves it reading
/// `east=side,west=side`, and the lamp on its far side lights (conformance
/// probe `dust_shape_decides_which_block_it_powers`, the control at the end).
///
/// The one case this deliberately does not reproduce is vanilla's
/// *history*-dependence: a wire with no attached side renders as a dot if it
/// was placed that way and as a four-way cross if it once had connections
/// and lost them. That distinction is unobservable here, because a dot and a
/// cross power exactly the same set of blocks -- nothing horizontally -- so
/// this function reports the honest geometric answer, `NONE`, for both.
pub fn dust_sides(world: &World, pos: Position) -> DustSides {
    let mut sides = [false; 4];
    for (i, &direction) in HORIZONTAL.iter().enumerate() {
        sides[i] = dust_side_connected(world, pos, direction);
    }
    if !sides.iter().any(|&b| b) {
        return DustSides::NONE;
    }

    // Read every flag before writing any of them: vanilla decides all four
    // fills from the pre-fill state, so filling in one axis must not then
    // suppress the other.
    let attached = |d: Facing| sides[DustSides::index(d).expect("HORIZONTAL")];
    let no_north_south = !attached(Facing::North) && !attached(Facing::South);
    let no_east_west = !attached(Facing::East) && !attached(Facing::West);
    for (i, &direction) in HORIZONTAL.iter().enumerate() {
        sides[i] |= match direction {
            Facing::East | Facing::West => no_north_south,
            Facing::North | Facing::South => no_east_west,
            _ => false,
        };
    }

    DustSides(sides)
}

/// The two horizontal directions perpendicular to `direction`.
fn perpendicular(direction: Facing) -> [Facing; 2] {
    match direction {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        _ => [Facing::North, Facing::South],
    }
}

/// Whether the dust at `pos` powers the block lying in `direction` --
/// **geometry only**, saying nothing about whether the dust carries a signal
/// right now.
///
/// Separated from `propagate::dust_power_toward` deliberately: `compile`'s
/// `net_reach` asks this question of a freshly emitted world whose `power`
/// fields are still placeholders, and must not be made to trust them.
///
/// The rule, measured against a real 1.20.1 server (conformance category
/// `dust-directionality`):
///
/// | direction | powered? |
/// |---|---|
/// | down | always -- the block a dust cell stands on |
/// | up | never |
/// | horizontal `D` | only if the `D.opposite()` side is attached and **neither** perpendicular side is |
///
/// So a straight run powers the blocks at both ends of its own axis, and a
/// corner, a T, a four-way cross and a lone dot power no horizontal
/// neighbour at all: one bend anywhere in the cell costs it its direction.
pub fn dust_powers_block_toward(world: &World, pos: Position, direction: Facing) -> bool {
    match direction {
        Facing::Down => true,
        Facing::Up => false,
        horizontal => {
            let sides = dust_sides(world, pos);
            sides.has(horizontal.opposite())
                && !perpendicular(horizontal).iter().any(|&d| sides.has(d))
        }
    }
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

    // ── Dust shape, and which blocks it powers ────────────────────────
    //
    // Every expectation below is measured, not reasoned about: conformance
    // category `dust-directionality` against a real 1.20.1 server.

    /// A straight east-west run of `len` dust cells at y=1, starting at
    /// x=1, on a stone floor.
    fn lay_run(world: &mut World, len: i32) {
        for x in 1..=len {
            place_dust_on_stone(world, x, 0, 2);
        }
    }

    fn sides_of(world: &World, pos: Position) -> Vec<Facing> {
        [Facing::North, Facing::South, Facing::East, Facing::West]
            .into_iter()
            .filter(|&d| dust_sides(world, pos).has(d))
            .collect()
    }

    #[test]
    fn a_run_of_two_is_straight_along_its_own_axis() {
        let mut w = World::new(8, 4, 6);
        lay_run(&mut w, 2);
        assert_eq!(
            sides_of(&w, Position::new(1, 1, 2)),
            vec![Facing::East, Facing::West],
            "the first cell attaches east to its neighbour, and vanilla fills in the west"
        );
    }

    #[test]
    fn a_one_sided_stub_is_completed_into_a_straight_run() {
        // Measured directly: a lone cell joined from one side reads
        // `east=side,west=side` on a live server. Without this fill a run's
        // last cell would have no axis and would power nothing, which is
        // the opposite of what the game does.
        let mut w = World::new(8, 4, 6);
        lay_run(&mut w, 2);
        let end = Position::new(2, 1, 2);
        assert_eq!(sides_of(&w, end), vec![Facing::East, Facing::West]);
        assert!(
            dust_powers_block_toward(&w, end, Facing::East),
            "the completed far side is what gives the run's last cell its direction"
        );
    }

    #[test]
    fn a_lone_dot_has_no_side_and_powers_no_horizontal_block() {
        let mut w = World::new(8, 4, 6);
        place_dust_on_stone(&mut w, 1, 0, 2);
        let dot = Position::new(1, 1, 2);
        assert!(dust_sides(&w, dot).is_empty());
        for d in HORIZONTAL {
            assert!(
                !dust_powers_block_toward(&w, dot, d),
                "a dot must power nothing toward {d:?} -- measured, and the \
                 opposite of what the vanilla source reads like at a glance"
            );
        }
    }

    #[test]
    fn a_straight_run_powers_the_block_at_both_ends_of_its_axis() {
        let mut w = World::new(10, 4, 6);
        lay_run(&mut w, 3);
        let middle = Position::new(2, 1, 2);
        assert!(dust_powers_block_toward(&w, middle, Facing::East));
        assert!(dust_powers_block_toward(&w, middle, Facing::West));
        assert!(!dust_powers_block_toward(&w, middle, Facing::North));
        assert!(!dust_powers_block_toward(&w, middle, Facing::South));
    }

    #[test]
    fn one_perpendicular_branch_costs_the_cell_its_direction() {
        let mut w = World::new(10, 4, 8);
        lay_run(&mut w, 3);
        let corner = Position::new(3, 1, 2);
        assert!(dust_powers_block_toward(&w, corner, Facing::East));

        // Join it from the south: a corner now, and corners power nothing.
        place_dust_on_stone(&mut w, 3, 0, 3);
        assert_eq!(sides_of(&w, corner), vec![Facing::South, Facing::West]);
        for d in HORIZONTAL {
            assert!(
                !dust_powers_block_toward(&w, corner, d),
                "a corner must power nothing toward {d:?}"
            );
        }
    }

    #[test]
    fn a_t_junction_powers_no_horizontal_block_either() {
        let mut w = World::new(10, 4, 8);
        lay_run(&mut w, 3);
        place_dust_on_stone(&mut w, 3, 0, 3);
        place_dust_on_stone(&mut w, 3, 0, 1);
        let tee = Position::new(3, 1, 2);
        assert_eq!(
            sides_of(&w, tee),
            vec![Facing::North, Facing::South, Facing::West]
        );
        for d in HORIZONTAL {
            assert!(!dust_powers_block_toward(&w, tee, d));
        }
    }

    #[test]
    fn a_climb_side_counts_as_a_connection() {
        // The cell at the foot of a climb still has an axis, so it still
        // powers the block on its far side.
        let mut w = World::new(10, 5, 6);
        place_dust_on_stone(&mut w, 2, 0, 2);
        w.set(1, 1, 2, stone());
        w.set(1, 2, 2, dust()); // dust up a step to the west
        let foot = Position::new(2, 1, 2);
        assert_eq!(sides_of(&w, foot), vec![Facing::East, Facing::West]);
        assert!(dust_powers_block_toward(&w, foot, Facing::East));
    }

    #[test]
    fn an_aligned_repeater_connects_but_a_perpendicular_one_does_not() {
        // Neither repeater is powered. What matters is the axis it lies on:
        // measured live, an aligned repeater beside a run kills the run's
        // direction and a perpendicular one leaves it alone.
        let repeater = |facing: Facing| {
            let mut r = block("minecraft:repeater", BlockKind::Repeater);
            r.facing = Some(facing);
            r
        };

        let mut aligned = World::new(10, 4, 8);
        lay_run(&mut aligned, 3);
        aligned.set(3, 0, 3, stone());
        aligned.set(3, 1, 3, repeater(Facing::North));
        let cell = Position::new(3, 1, 2);
        assert!(dust_side_connected(&aligned, cell, Facing::South));
        assert!(!dust_powers_block_toward(&aligned, cell, Facing::East));

        let mut across = World::new(10, 4, 8);
        lay_run(&mut across, 3);
        across.set(3, 0, 3, stone());
        across.set(3, 1, 3, repeater(Facing::East));
        assert!(!dust_side_connected(&across, cell, Facing::South));
        assert!(
            dust_powers_block_toward(&across, cell, Facing::East),
            "a repeater lying across the run is invisible to it"
        );
    }

    #[test]
    fn dust_always_powers_the_block_below_and_never_the_one_above() {
        // Shape-independent, both ways round: check it on a dot, which has
        // no shape at all, and on a corner, which has one that powers
        // nothing horizontally.
        let mut w = World::new(10, 4, 8);
        place_dust_on_stone(&mut w, 1, 0, 2);
        let dot = Position::new(1, 1, 2);
        assert!(dust_powers_block_toward(&w, dot, Facing::Down));
        assert!(!dust_powers_block_toward(&w, dot, Facing::Up));

        lay_run(&mut w, 3);
        place_dust_on_stone(&mut w, 3, 0, 3);
        let corner = Position::new(3, 1, 2);
        assert!(dust_powers_block_toward(&w, corner, Facing::Down));
        assert!(!dust_powers_block_toward(&w, corner, Facing::Up));
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
