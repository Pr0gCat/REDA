//! 訊號強度傳播與方塊充能。
//!
//! 從所有訊號源開始 BFS，每經過一格紅石粉強度 -1，強度 0 就停止。
//!
//! 用 BFS 依**功率流方向**展開，而不是原版那種對方塊放置順序敏感的遞迴 ——
//! 所以同一個電路擺在任何座標結果都相同。這是 Alternate Current 的思路。

use std::collections::VecDeque;

use crate::redstone::rules::taxonomy::{flags_of, power_emitted_toward, BlockPower};
use crate::redstone::simulator::connectivity::dust_connections;
use crate::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// 紅石訊號的最大強度。
pub const MAX_SIGNAL_STRENGTH: u8 = 15;

/// 重算全世界紅石粉的訊號強度，回傳強度有改變的位置。
///
/// 回傳空的 `Vec` 表示已經是穩定狀態。呼叫端用這份清單排程鄰居更新 ——
/// 回傳「改變了幾格」會逼呼叫端自己再掃一次世界。
pub fn recompute_dust_strengths(world: &mut World) -> Vec<Position> {
    let (size_x, _size_y, size_z) = world.size();
    let cell_count = world.cells().len();

    // palette 通常只有幾十個項目，先算出哪些索引是紅石粉，
    // 掃描就退化成每格一次整數比較
    let dust_palette_indices: Vec<bool> = world
        .palette()
        .entries()
        .iter()
        .map(|state| state.kind == BlockKind::RedstoneWire)
        .collect();

    // 收集所有紅石粉的位置，以及每格的目標強度（用 World::index 當鍵，
    // 一個扁平 Vec 就夠，不必付 HashMap 的雜湊成本）
    let mut dust_positions = Vec::new();
    let mut queue: VecDeque<(Position, u8)> = VecDeque::new();
    let mut target: Vec<u8> = vec![0u8; cell_count];

    let layer = (size_x as usize) * (size_z as usize);
    for (flat, &palette_idx) in world.cells().iter().enumerate() {
        if !dust_palette_indices[palette_idx as usize] {
            continue;
        }
        let y = (flat / layer) as i32;
        let rem = flat % layer;
        let z = (rem / size_x as usize) as i32;
        let x = (rem % size_x as usize) as i32;
        dust_positions.push(Position::new(x, y, z));
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
            let (kind, strength) = block_signal_at(world, neighbour);
            if kind == BlockPower::Strong {
                best = best.max(strength);
            }
        }

        if best > 0 {
            let flat = world
                .index(pos.x, pos.y, pos.z)
                .expect("dust position must be in-bounds");
            target[flat] = best;
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
            for neighbour in dust_connections(world, pos, facing).iter() {
                let flat = world
                    .index(neighbour.x, neighbour.y, neighbour.z)
                    .expect("dust_connections only returns in-bounds positions");
                let current = target[flat];
                if next_strength > current {
                    target[flat] = next_strength;
                    queue.push_back((neighbour, next_strength));
                }
            }
        }
    }

    // 寫回，收集改變的位置
    let mut changed = Vec::new();
    for &pos in &dust_positions {
        let flat = world
            .index(pos.x, pos.y, pos.z)
            .expect("dust position must be in-bounds");
        let want = target[flat];
        let state = world.get(pos.x, pos.y, pos.z);
        if state.power != want {
            let mut updated = state.clone();
            updated.power = want;
            world.set(pos.x, pos.y, pos.z, updated);
            changed.push(pos);
        }
    }

    changed
}

/// 這一格方塊被充能到什麼程度，以及**多強**。
///
/// `block_power_at` 只回答種類，這個版本連強度一起回答。比較器透過方塊
/// 傳出的是 0..15 的類比值 —— 只回傳「強充能」會把它壓成 15，等於把
/// 比較器變成一個開關。
///
/// 回傳 `(BlockPower::None, 0)` 表示沒有充能。
pub fn block_signal_at(world: &World, pos: Position) -> (BlockPower, u8) {
    let state = world.get(pos.x, pos.y, pos.z);
    if !flags_of(state).is_conductive() {
        return (BlockPower::None, 0);
    }

    let mut best_kind = BlockPower::None;
    let mut best_strength = 0u8;

    for facing in ALL_SIX {
        let neighbour = pos.offset(facing);
        let neighbour_state = world.get(neighbour.x, neighbour.y, neighbour.z);
        let output = power_emitted_toward(neighbour_state, facing.opposite());

        match output.block_power {
            BlockPower::Strong => {
                // 強充能勝過弱充能；同為強充能時取較大的強度
                if best_kind != BlockPower::Strong || output.strength > best_strength {
                    best_kind = BlockPower::Strong;
                    best_strength = output.strength;
                }
            }
            BlockPower::Weak => {
                if best_kind == BlockPower::None {
                    best_kind = BlockPower::Weak;
                    best_strength = output.strength;
                } else if best_kind == BlockPower::Weak && output.strength > best_strength {
                    best_strength = output.strength;
                }
            }
            BlockPower::None => {}
        }
    }

    (best_kind, best_strength)
}

/// 這一格方塊被充能到什麼程度。
///
/// 只有**強充能**的方塊能再驅動相鄰的紅石粉；弱充能的不行 —— 這是繞線時
/// 每段線都必須以主動元件收尾的原因。
///
/// 需要強度時用 `block_signal_at`。
pub fn block_power_at(world: &World, pos: Position) -> BlockPower {
    block_signal_at(world, pos).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

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
        assert_eq!(changed.len(), 5, "all five dust cells went from 0 to non-zero");

        let changed_again = recompute_dust_strengths(&mut w);
        assert_eq!(changed_again.len(), 0, "a second pass changes nothing");
    }

    #[test]
    fn recompute_is_cheap_on_a_world_with_no_dust() {
        // 空世界的成本就是「找不到東西」的成本 —— tick 迴圈每個 game tick
        // 都會呼叫一次，所以這條路徑必須便宜
        let mut w = World::new(64, 32, 64);
        let start = std::time::Instant::now();
        for _ in 0..10 {
            let changed = recompute_dust_strengths(&mut w);
            assert!(changed.is_empty());
        }
        let per_call = start.elapsed() / 10;

        // debug build 會慢很多，這個上限只是要抓住數量級的退步
        assert!(
            per_call < std::time::Duration::from_millis(50),
            "recompute on an empty 64x32x64 world took {per_call:?} per call"
        );
    }

    #[test]
    fn a_strongly_powered_block_passes_on_the_real_strength_not_fifteen() {
        // 比較器透過方塊傳出的是類比值。壓成 15 等於把比較器變成開關。
        let mut w = World::new(10, 5, 10);

        // 石頭當被充能的方塊，粉在它旁邊
        w.set(5, 1, 5, stone());
        w.set(6, 0, 5, stone());
        w.set(6, 1, 5, dust());

        // 比較器在石頭西邊，朝東（指向石頭），輸出強度 7。
        // （Facing 是全域座標系：East 是 +x，比較器在 x=4、石頭在 x=5，
        // 所以要朝 East 才能指到石頭 —— 見 `Position::offset` 與
        // `power_emitted_toward` 對中繼器／比較器的方向比對。）
        let mut comparator = named("minecraft:comparator", BlockKind::Comparator);
        comparator.lit = true;
        comparator.power = 7;
        comparator.facing = Some(Facing::East);
        w.set(6, 1, 5, dust());
        w.set(4, 1, 5, comparator);

        let (kind, strength) = block_signal_at(&w, Position::new(5, 1, 5));
        assert_eq!(kind, BlockPower::Strong, "the comparator strongly powers the stone");
        assert_eq!(strength, 7, "and it must pass on 7, not 15");
    }

    #[test]
    fn block_power_at_still_agrees_with_block_signal_at() {
        let mut w = World::new(10, 5, 10);
        w.set(5, 1, 5, stone());
        let mut lever = named("minecraft:lever", BlockKind::Lever);
        lever.lit = true;
        w.set(4, 1, 5, lever);

        let pos = Position::new(5, 1, 5);
        assert_eq!(block_power_at(&w, pos), block_signal_at(&w, pos).0);
    }

    #[test]
    fn an_unpowered_block_reports_no_signal() {
        let mut w = World::new(10, 5, 10);
        w.set(5, 1, 5, stone());
        assert_eq!(
            block_signal_at(&w, Position::new(5, 1, 5)),
            (BlockPower::None, 0)
        );
    }
}
