//! 訊號強度傳播與方塊充能。
//!
//! 從所有訊號源開始 BFS，每經過一格紅石粉強度 -1，強度 0 就停止。
//!
//! 用 BFS 依**功率流方向**展開，而不是原版那種對方塊放置順序敏感的遞迴 ——
//! 所以同一個電路擺在任何座標結果都相同。這是 Alternate Current 的思路。

use std::collections::VecDeque;

use crate::redstone::rules::taxonomy::{flags_of, power_emitted_toward, BlockPower};
use crate::redstone::simulator::connectivity::dust_connects;
use crate::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// 紅石訊號的最大強度。
pub const MAX_SIGNAL_STRENGTH: u8 = 15;

/// 重算全世界紅石粉的訊號強度，回傳有多少格改變了。
///
/// 回傳 0 表示已經是穩定狀態。
pub fn recompute_dust_strengths(world: &mut World) -> usize {
    let (size_x, size_y, size_z) = world.size();

    // 收集所有紅石粉的位置，以及每格的目標強度
    let mut dust_positions = Vec::new();
    let mut queue: VecDeque<(Position, u8)> = VecDeque::new();
    let mut target: std::collections::HashMap<Position, u8> = std::collections::HashMap::new();

    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                let pos = Position::new(x, y, z);
                let state = world.get(x, y, z);
                if state.kind == BlockKind::RedstoneWire {
                    dust_positions.push(pos);
                    target.insert(pos, 0);
                }
            }
        }
    }

    // 每格紅石粉的初始強度：來自相鄰的非紅石粉訊號源
    for &pos in &dust_positions {
        let mut best = 0u8;

        // 直接驅動紅石粉的元件（紅石塊、拉桿、中繼器正前方…）
        //
        // 刻意排除紅石粉鄰居：粉對粉的傳遞完全交給下面的 BFS（經
        // `dust_connects`，含爬升／下降規則）。若在這裡也採計鄰居粉的
        // `power` 欄位，讀到的會是**這次 recompute 開始前**留下的舊值——
        // 對第一次算尚無影響（舊值皆為 0），但穩定電路上再算一次時，
        // 舊值會被當成「訊號源」重新灌回來，讓強度不會隨著上游訊號源
        // 消失而歸零，也讓已經穩定的結果在下一次重算時無謂地變動。
        for facing in ALL_SIX {
            let neighbour = pos.offset(facing);
            let neighbour_state = world.get(neighbour.x, neighbour.y, neighbour.z);
            if neighbour_state.kind == BlockKind::RedstoneWire {
                continue;
            }
            // 鄰居是往「朝向我們」的方向送出，也就是 facing 的反方向
            let output = power_emitted_toward(neighbour_state, facing.opposite());
            if output.drives_dust {
                best = best.max(output.strength);
            }
        }

        // 強充能的方塊也能驅動相鄰的紅石粉
        for facing in ALL_SIX {
            let neighbour = pos.offset(facing);
            if block_power_at(world, neighbour) == BlockPower::Strong {
                best = MAX_SIGNAL_STRENGTH;
            }
        }

        if best > 0 {
            target.insert(pos, best);
            queue.push_back((pos, best));
        }
    }

    // BFS：沿著連接關係往外傳，每格 -1
    while let Some((pos, strength)) = queue.pop_front() {
        if strength <= 1 {
            continue;
        }
        let next_strength = strength - 1;
        for facing in HORIZONTAL {
            if let Some(neighbour) = dust_connects(world, pos, facing) {
                let current = target.get(&neighbour).copied().unwrap_or(0);
                if next_strength > current {
                    target.insert(neighbour, next_strength);
                    queue.push_back((neighbour, next_strength));
                }
            }
        }
    }

    // 寫回，統計改變的格數
    let mut changed = 0;
    for &pos in &dust_positions {
        let want = target.get(&pos).copied().unwrap_or(0);
        let state = world.get(pos.x, pos.y, pos.z);
        if state.power != want {
            let mut updated = state.clone();
            updated.power = want;
            world.set(pos.x, pos.y, pos.z, updated);
            changed += 1;
        }
    }

    changed
}

/// 這一格方塊被充能到什麼程度。
///
/// 只有**強充能**的方塊能再驅動相鄰的紅石粉；弱充能的不行 —— 這是繞線時
/// 每段線都必須以主動元件收尾的原因。
pub fn block_power_at(world: &World, pos: Position) -> BlockPower {
    let state = world.get(pos.x, pos.y, pos.z);
    if !flags_of(state).is_conductive() {
        return BlockPower::None;
    }

    let mut best = BlockPower::None;

    for facing in ALL_SIX {
        let neighbour = pos.offset(facing);
        let neighbour_state = world.get(neighbour.x, neighbour.y, neighbour.z);
        // 鄰居是往「朝向我們」的方向送出，也就是 facing 的反方向
        let output = power_emitted_toward(neighbour_state, facing.opposite());

        match output.block_power {
            BlockPower::Strong => return BlockPower::Strong,
            BlockPower::Weak => best = BlockPower::Weak,
            BlockPower::None => {}
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState};

    fn named(name: &str, kind: BlockKind) -> BlockState {
        let mut b = BlockState::air();
        b.kind = kind;
        b.name = name.to_string();
        b
    }

    fn stone() -> BlockState {
        named("minecraft:stone", BlockKind::Solid)
    }

    fn dust() -> BlockState {
        named("minecraft:redstone_wire", BlockKind::RedstoneWire)
    }

    fn redstone_block() -> BlockState {
        named("minecraft:redstone_block", BlockKind::RedstoneBlock)
    }

    /// 鋪一條長度 `len` 的紅石粉，從 x=1 開始，載體是石頭。
    fn lay_wire(world: &mut World, len: i32) {
        for x in 1..=len {
            world.set(x, 0, 0, stone());
            world.set(x, 1, 0, dust());
        }
    }

    #[test]
    fn an_unpowered_wire_stays_at_zero() {
        let mut w = World::new(20, 3, 3);
        lay_wire(&mut w, 5);
        recompute_dust_strengths(&mut w);
        for x in 1..=5 {
            assert_eq!(w.get(x, 1, 0).power, 0, "dust at x={x}");
        }
    }

    #[test]
    fn strength_drops_by_one_per_block() {
        let mut w = World::new(20, 3, 3);
        lay_wire(&mut w, 5);
        // 在 x=0 放紅石塊當電源
        w.set(0, 1, 0, redstone_block());

        recompute_dust_strengths(&mut w);

        assert_eq!(w.get(1, 1, 0).power, 15, "adjacent to the source");
        assert_eq!(w.get(2, 1, 0).power, 14);
        assert_eq!(w.get(3, 1, 0).power, 13);
        assert_eq!(w.get(4, 1, 0).power, 12);
        assert_eq!(w.get(5, 1, 0).power, 11);
    }

    #[test]
    fn a_wire_longer_than_fifteen_dies_out() {
        let mut w = World::new(30, 3, 3);
        lay_wire(&mut w, 20);
        w.set(0, 1, 0, redstone_block());

        recompute_dust_strengths(&mut w);

        assert_eq!(w.get(15, 1, 0).power, 1, "the fifteenth block still has 1");
        assert_eq!(w.get(16, 1, 0).power, 0, "the sixteenth is dead");
        assert_eq!(w.get(20, 1, 0).power, 0);
    }

    #[test]
    fn removing_the_source_clears_the_whole_wire() {
        let mut w = World::new(20, 3, 3);
        lay_wire(&mut w, 5);
        w.set(0, 1, 0, redstone_block());
        recompute_dust_strengths(&mut w);
        assert_eq!(w.get(1, 1, 0).power, 15);

        w.set(0, 1, 0, BlockState::air());
        recompute_dust_strengths(&mut w);

        for x in 1..=5 {
            assert_eq!(w.get(x, 1, 0).power, 0, "dust at x={x} after source removal");
        }
    }

    #[test]
    fn dust_only_weakly_powers_the_block_beneath_it() {
        let mut w = World::new(20, 3, 3);
        lay_wire(&mut w, 3);
        w.set(0, 1, 0, redstone_block());
        recompute_dust_strengths(&mut w);

        // 粉底下的石頭是弱充能 —— 不能再驅動相鄰的粉
        assert_eq!(
            block_power_at(&w, Position::new(1, 0, 0)),
            BlockPower::Weak,
            "a block under dust is only weakly powered"
        );
    }

    #[test]
    fn a_torch_does_not_light_dust_through_its_own_support_block() {
        // 探測發現的殺手案例：火把立在石頭上，火把不該充能它的支撐塊，
        // 所以只能透過支撐塊才碰得到訊號的粉應該保持暗。
        //
        // 探測用的粉刻意放在跟火把支撐塊同一層、但不直接貼著火把本身
        // 的位置 —— 如果粉直接貼著火把，火把會直接充能它（這是合法的
        // 另一條訊號路徑，見 taxonomy 的方向性測試），會跟這裡要抓的
        // 「火把餵自己支撐塊」這個 bug 混在一起，測不出來。
        let mut w = World::new(10, 5, 10);
        w.set(5, 1, 5, stone()); // 火把的支撐塊
        let mut torch = named("minecraft:redstone_torch", BlockKind::Torch);
        torch.lit = true;
        w.set(5, 2, 5, torch); // 立在支撐塊上，跟支撐塊同一縱列

        // 探測粉：跟支撐塊同一層水平相鄰，自己的支撐塊是另一塊石頭。
        // 它完全不碰到火把本身。
        w.set(6, 0, 5, stone());
        w.set(6, 1, 5, dust());

        recompute_dust_strengths(&mut w);

        assert_eq!(
            w.get(6, 1, 5).power,
            0,
            "a torch must not power its support block, so dust reachable only through that block stays dark"
        );
    }

    #[test]
    fn recompute_reports_how_many_cells_changed() {
        let mut w = World::new(20, 3, 3);
        lay_wire(&mut w, 5);
        w.set(0, 1, 0, redstone_block());

        let changed = recompute_dust_strengths(&mut w);
        assert_eq!(changed, 5, "all five dust cells went from 0 to non-zero");

        let changed_again = recompute_dust_strengths(&mut w);
        assert_eq!(changed_again, 0, "a second pass changes nothing");
    }
}
