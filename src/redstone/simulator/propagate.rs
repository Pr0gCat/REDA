//! 訊號強度傳播與方塊充能。
//!
//! 從所有訊號源開始 BFS，每經過一格紅石粉強度 -1，強度 0 就停止。
//!
//! 用 BFS 依**功率流方向**展開，而不是原版那種對方塊放置順序敏感的遞迴 ——
//! 所以同一個電路擺在任何座標結果都相同。這是 Alternate Current 的思路。

use std::collections::{HashMap, HashSet, VecDeque};

use crate::redstone::rules::taxonomy::{flags_of, power_emitted_by, power_emitted_toward, BlockPower, PowerOutput};
use crate::redstone::world::block::Facing;
use crate::redstone::simulator::connectivity::{dust_connections, dust_powers_block_toward};
use crate::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// 紅石訊號的最大強度。
pub const MAX_SIGNAL_STRENGTH: u8 = 15;

/// 重算「這次可能受影響」的紅石粉網路，回傳強度有改變的位置。
///
/// 回傳空的 `Vec` 表示已經是穩定狀態。呼叫端用這份清單排程鄰居更新 ——
/// 回傳「改變了幾格」會逼呼叫端自己再掃一次世界。
///
/// 成本跟「這次真的可能受影響的紅石粉」成正比，不是世界體積，也不是
/// 世界裡紅石粉的**總數**：
///
/// - `World::take_dirty` 拿到的是自從上次呼叫以來被 `World::set` 動過的
///   格子（`World::set` 無條件記錄）。一個像七段顯示器解碼器那樣的真實
///   電路，紅石粉本身可能有十幾萬格，但每個 game tick 通常只有少數幾個
///   元件真的翻轉狀態 —— 每個 tick 都把十幾萬格粉全部重算一次，就算已經
///   是「只跟紅石粉數量成正比」也還是太貴。
/// - `active_dust_networks` 把每個髒格展開到它波及得到的紅石粉網路（見
///   該函式的說明），只有這些網路才需要重算；沒被波及的網路這次完全不
///   會被碰。
/// - 沒有任何格子是髒的（沒有任何呼叫端呼叫過 `World::set`）就直接跳過
///   整次計算 —— 這在等待中繼器延遲、佇列暫時空著的 tick 尤其重要。
///
/// `target` 也只為「BFS 真的碰到的格子」存強度 —— 而 `dust_connections`
/// 只會回傳紅石粉鄰居（見其實作），所以 BFS 走訪到的格子必定是
/// `active_dust` 的子集，一個大小跟這次受影響的紅石粉數量成正比的 map
/// 就夠。
pub fn recompute_dust_strengths(world: &mut World) -> Vec<Position> {
    let dirty = world.take_dirty();
    if dirty.is_empty() {
        return Vec::new();
    }

    let active_dust = active_dust_networks(world, &dirty);

    let mut queue: VecDeque<(Position, u8)> = VecDeque::new();
    let mut target: HashMap<Position, u8> = HashMap::with_capacity(active_dust.len());

    // 每格紅石粉的初始強度：來自相鄰的非紅石粉訊號源
    for &pos in &active_dust {
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
            for neighbour in dust_connections(world, pos, facing).iter() {
                let current = target.get(&neighbour).copied().unwrap_or(0);
                if next_strength > current {
                    target.insert(neighbour, next_strength);
                    queue.push_back((neighbour, next_strength));
                }
            }
        }
    }

    // 寫回，收集改變的位置
    let mut changed = Vec::new();
    for &pos in &active_dust {
        let want = target.get(&pos).copied().unwrap_or(0);
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

/// 從「這次可能受影響」的髒格清單，找出真正需要重算的紅石粉。
///
/// 分兩步：
///
/// 1. **從髒格找種子**：每個髒格往外展開到 2 跳鄰域內找紅石粉。2 跳是
///    因為一格紅石粉的初始強度最遠會看到 2 跳之外的方塊 ——
///    `block_signal_at` 檢查的是「鄰居的鄰居」（粉 → 導體 → 導體另一側
///    的訊號源），`dust_connections` 的爬升／下降規則也是看鄰居的上方或
///    下方（同樣是 2 跳）。只展開 1 跳會漏掉「兩格外的比較器把訊號送進
///    導體，導體再驅動粉」這種接法。
/// 2. **從種子洪水填滿整個網路**：找到種子之後，沿著 `dust_connections`
///    走訪整個連通的紅石粉網路（跟下面 BFS 傳播強度用的是同一套連接
///    規則），因為網路裡任何一格的強度都可能因為上游的改變而跟著變 ——
///    只重算種子本身、不管它所在的整條線，會漏掉沿線往後傳的變化。
///
/// 沒被任何髒格波及的網路完全不會出現在回傳值裡，維持原樣不用重算。
fn active_dust_networks(world: &World, dirty: &[usize]) -> Vec<Position> {
    let mut visited: HashSet<Position> = HashSet::new();
    let mut active: Vec<Position> = Vec::new();

    for &flat in dirty {
        let (x, y, z) = world.decode(flat);
        let origin = Position::new(x, y, z);

        // 2 跳鄰域：origin 本身、它的 6 個鄰居、以及鄰居的鄰居。
        let mut frontier = vec![origin];
        let mut within_two_hops = vec![origin];
        for _ in 0..2 {
            let mut next = Vec::with_capacity(frontier.len() * ALL_SIX.len());
            for &p in &frontier {
                for facing in ALL_SIX {
                    next.push(p.offset(facing));
                }
            }
            within_two_hops.extend_from_slice(&next);
            frontier = next;
        }

        for seed in within_two_hops {
            if visited.contains(&seed) {
                continue;
            }
            if world.get(seed.x, seed.y, seed.z).kind != BlockKind::RedstoneWire {
                continue;
            }

            // 種子找到了，沿著連接關係洪水填滿整個網路
            visited.insert(seed);
            active.push(seed);
            let mut stack = vec![seed];
            while let Some(pos) = stack.pop() {
                for facing in HORIZONTAL {
                    for neighbour in dust_connections(world, pos, facing).iter() {
                        if visited.insert(neighbour) {
                            active.push(neighbour);
                            stack.push(neighbour);
                        }
                    }
                }
            }
        }
    }

    active
}

/// What the dust at `pos` puts into the block lying in `direction`.
///
/// This is the world-aware half of `taxonomy::power_emitted_toward`'s
/// `RedstoneWire` arm -- the half a `BlockState` alone cannot answer, because
/// it depends on the dust's connection shape, which is a fact about the
/// surrounding world. Every caller that has a `World` must use this instead;
/// `power_emitted_toward` reports the horizontal directions as `INERT`
/// because it has no way to know, not because they are.
///
/// It is exactly the geometry of `connectivity::dust_powers_block_toward`
/// (which carries the measured rule and its table), gated on the wire
/// actually carrying a signal. Weak, always: a block powered this way still
/// cannot re-drive
/// dust, which is why `recompute_dust_strengths` is untouched by this and
/// why no dust cell's strength can move because of it.
///
/// **This does not govern what a repeater or comparator reads from dust
/// touching it.** Those read the wire's `power` field directly, whatever its
/// shape (vanilla's `DiodeBlock::getInputSignal` has an explicit fallback for
/// exactly this), which is what `signal_from`'s first path already does. A
/// dust corner does drive a repeater it turns into; it just does not power
/// the *block* beside it.
pub fn dust_power_toward(world: &World, pos: Position, direction: Facing) -> PowerOutput {
    let state = world.get(pos.x, pos.y, pos.z);
    if state.kind != BlockKind::RedstoneWire {
        return power_emitted_toward(state, direction);
    }
    let full = power_emitted_by(state);
    if full == PowerOutput::INERT {
        return PowerOutput::INERT;
    }
    if dust_powers_block_toward(world, pos, direction) {
        full
    } else {
        PowerOutput::INERT
    }
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
        // `dust_power_toward` defers to `power_emitted_toward` for
        // everything that is not dust, and answers the horizontal case
        // `power_emitted_toward` structurally cannot for the one kind that
        // is. A block learning it is powered is the only thing this changes.
        let output = dust_power_toward(world, neighbour, facing.opposite());

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

/// 從 `source` 那一格傳到 `target` 的訊號強度。
///
/// 這是所有元件讀取輸入的唯一入口，涵蓋兩條路徑：
///
/// 1. **直接相鄰的紅石粉** —— 粉會驅動它指向的元件，而粉一定會指向
///    相鄰的中繼器或比較器。這是最常見的接法。
/// 2. **被充能的方塊** —— 強充能才能再驅動下游；弱充能不行。
///
/// 少了第一條，粉接中繼器這個最基本的接法就不會動。
pub fn signal_from(world: &World, source: Position, target: Position) -> u8 {
    let source_state = world.get(source.x, source.y, source.z);

    // 路徑一：相鄰的紅石粉直接驅動
    if source_state.kind == BlockKind::RedstoneWire {
        return source_state.power;
    }

    // 路徑二：元件直接朝這個方向輸出
    if let Some(direction) = direction_from(source, target) {
        let output = power_emitted_toward(source_state, direction);
        if output.drives_dust || output.block_power != BlockPower::None {
            return output.strength;
        }
    }

    // 路徑三：被強充能的方塊
    let (kind, strength) = block_signal_at(world, source);
    if kind == BlockPower::Strong {
        return strength;
    }

    0
}

/// 從 `from` 看向 `to` 是哪個方向。兩者不相鄰時回傳 `None`。
fn direction_from(from: Position, to: Position) -> Option<Facing> {
    ALL_SIX.into_iter().find(|&facing| from.offset(facing) == to)
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

    /// 這是會抓到「有人把 O(dust count) 又改回 O(world volume) 掃描」的
    /// 測試 —— 光測正確性測不出這種回歸，因為兩種實作在小世界裡的結果
    /// 完全一樣。
    ///
    /// 世界開到 1500x6x1500（1350 萬格），但只放 5 格紅石粉跟 1 個訊號源
    /// （紅石塊），其餘全是空氣。若 `recompute_dust_strengths` 又退化成
    /// 掃過 `world.cells()` 找紅石粉，這一千三百五十萬格全部都要碰過一次
    /// ——實測這個規模的舊實作（`for (flat, palette_idx) in
    /// world.cells().iter().enumerate()` 那個版本）在 debug build 下單次
    /// 呼叫要 200ms 以上；而稀疏版本因為直接從 `World::positions_of`
    /// 拿到紅石粉的位置，成本只跟 5 格紅石粉成正比，同樣是 debug build
    /// 量級落在幾百微秒。10ms 的預算對稀疏版本是幾十倍的安全邊際，對
    /// 退化成全體積掃描的版本則遠遠不夠。
    #[test]
    fn recompute_cost_is_proportional_to_dust_not_world_volume() {
        let mut w = World::new(1500, 6, 1500);
        lay_wire(&mut w, 5);
        w.set(0, 1, 0, redstone_block());

        let start = std::time::Instant::now();
        let changed = recompute_dust_strengths(&mut w);
        let elapsed = start.elapsed();

        // 先確認結果正確，不是只圖快而算錯
        assert_eq!(changed.len(), 5, "all five dust cells should light up from the source");
        assert_eq!(w.get(1, 1, 0).power, 15, "adjacent to the source");
        assert_eq!(w.get(5, 1, 0).power, 11);

        assert!(
            elapsed < std::time::Duration::from_millis(10),
            "recompute on a 1500x6x1500 world (13.5M cells) with only 5 dust cells took \
             {elapsed:?} -- this must cost O(dust count), not O(world volume); a \
             volume-proportional scan measured well over 100ms on this exact scenario"
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

        // 比較器在石頭西邊，輸出朝東（指向石頭），輸出強度 7。
        // （Facing 是全域座標系：East 是 +x，比較器在 x=4、石頭在 x=5。
        // Wiki 對比較器 `facing` 的定義是「從輸出指向輸入的方向」，所以
        // 輸出方向是 `facing` 的反方向 —— 要輸出到 East，`facing` 必須是
        // West。見 `Position::offset` 與 `power_emitted_toward` 對中繼器／
        // 比較器的方向比對。）
        let mut comparator = named("minecraft:comparator", BlockKind::Comparator);
        comparator.lit = true;
        comparator.power = 7;
        comparator.facing = Some(Facing::West);
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

    /// A powered straight run of `len` dust cells at y=1 ending against a
    /// stone block at `x = len + 1`, all on a stone floor at y=0.
    fn run_into_a_block(len: i32) -> World {
        let mut w = World::new(len + 6, 4, 6);
        for x in 1..=len {
            w.set(x, 0, 2, stone());
            w.set(x, 1, 2, dust());
        }
        w.set(len + 1, 1, 2, stone()); // the block the run points into
        w.set(0, 1, 2, redstone_block());
        recompute_dust_strengths(&mut w);
        w
    }

    #[test]
    fn a_dust_run_weakly_powers_the_block_it_points_into() {
        // Measured, conformance probe `dust_shape_decides_which_block_it_
        // powers`. Before this rule existed the simulator reported this
        // block as completely inert, and the comment on
        // `power_emitted_toward`'s dust arm claimed this file handled it.
        let w = run_into_a_block(3);
        let (kind, strength) = block_signal_at(&w, Position::new(4, 1, 2));
        assert_eq!(kind, BlockPower::Weak, "a run's far block is weakly powered");
        assert_eq!(strength, 13, "and carries the run's own strength, not 15");
    }

    #[test]
    fn a_bent_run_leaves_that_same_block_inert() {
        let mut w = run_into_a_block(3);
        // Join the last cell from the south. Nothing about the run's power
        // changes; only its shape does.
        w.set(3, 0, 3, stone());
        w.set(3, 1, 3, dust());
        recompute_dust_strengths(&mut w);
        assert_eq!(w.get(3, 1, 2).power, 13, "the run is still powered");
        assert_eq!(
            block_signal_at(&w, Position::new(4, 1, 2)),
            (BlockPower::None, 0),
            "one perpendicular branch costs the run its direction"
        );
    }

    #[test]
    fn a_block_powered_by_a_dust_run_still_cannot_repower_dust() {
        // The whole point of the power being *weak*. If this ever became
        // strong, every routing channel in the compiler would leak through
        // its own separator blocks.
        let mut w = run_into_a_block(3);
        w.set(5, 0, 2, stone());
        w.set(5, 1, 2, dust()); // on the far side of the powered block
        recompute_dust_strengths(&mut w);
        assert_eq!(
            w.get(5, 1, 2).power,
            0,
            "weak power must not cross the block into another wire"
        );
    }

    #[test]
    fn no_dust_strength_can_move_because_of_the_directionality_rule() {
        // `recompute_dust_strengths` only ever consults *strong* block
        // power, and this rule only ever adds weak power, so it cannot
        // reach dust strengths at all. Checked rather than asserted in
        // prose, because it is the reason no compiled circuit's settle time
        // could move when the rule landed.
        let w = run_into_a_block(5);
        for x in 1..=5 {
            assert_eq!(w.get(x, 1, 2).power, 16 - x as u8, "dust at x={x}");
        }
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
