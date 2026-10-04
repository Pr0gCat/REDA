//! 訊號強度傳播與方塊充能。
//!
//! 從所有訊號源開始 BFS，每經過一格紅石粉強度 -1，強度 0 就停止。
//!
//! 用 BFS 依**功率流方向**展開，而不是原版那種對方塊放置順序敏感的遞迴 ——
//! 所以同一個電路擺在任何座標結果都相同。這是 Alternate Current 的思路。

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use serde::Serialize;

use crate::redstone::rules::taxonomy::{
    power_emitted_by, power_emitted_toward, BlockPower, PowerOutput,
};
use crate::redstone::simulator::connectivity::{dust_connections, dust_powers_block_toward};
use crate::redstone::simulator::position::{Position, ALL_SIX, HORIZONTAL};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::block::Facing;
use crate::redstone::world::storage::World;

/// A hasher for this module's flat-index keys.
///
/// The maps below are keyed by `World` flat indices -- one `usize` per cell --
/// and nothing about them needs hashing to resist anything: they are local,
/// they live for one recomputation, and no caller ever sees them. The default
/// `RandomState` charges SipHash for every lookup in the hottest loop in the
/// simulator to defend a map no adversary can reach.
///
/// One multiply by an odd constant is a bijection over `u64`, so distinct
/// indices stay distinct, and it moves the entropy of small sequential indices
/// into the high bits the table reads for its tags; the shift-xor folds some of
/// it back down for the low bits the bucket index uses. Seedless, so a run is
/// reproducible, and `Default` is the only way to build one -- which is what
/// `BuildHasherDefault` needs.
#[derive(Default)]
struct FlatHasher(u64);

/// 2^64 / φ -- the odd multiplier Fibonacci hashing uses.
const FLAT_HASH_MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;

impl Hasher for FlatHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    /// Overwrites the state rather than folding into it, which is only sound
    /// because the aliases below hard-code `usize` keys: one key is one call,
    /// so there is never a previous value to carry.
    fn write_usize(&mut self, value: usize) {
        let mixed = (value as u64).wrapping_mul(FLAT_HASH_MULTIPLIER);
        self.0 = mixed ^ (mixed >> 32);
    }

    /// Never used -- every key here is a `usize` -- but a `Hasher` has to have
    /// it, and it must not silently hash nothing if one ever is not.
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let mixed = (self.0 ^ u64::from(byte)).wrapping_mul(FLAT_HASH_MULTIPLIER);
            self.0 = mixed ^ (mixed >> 32);
        }
    }
}

type FlatBuildHasher = BuildHasherDefault<FlatHasher>;
type FlatSet = HashSet<usize, FlatBuildHasher>;
type FlatMap<V> = HashMap<usize, V, FlatBuildHasher>;

/// 紅石訊號的最大強度。
pub const MAX_SIGNAL_STRENGTH: u8 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PropagationPolicy {
    DirectedDustComponent,
}

impl PropagationPolicy {
    fn recompute(self, world: &mut World) -> Vec<Position> {
        match self {
            PropagationPolicy::DirectedDustComponent => recompute_directed_dust_component(world),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PropagationSemantics {
    pub policy: PropagationPolicy,
    pub semantic_version: u64,
}

pub const PROPAGATION_SEMANTICS: PropagationSemantics = PropagationSemantics {
    policy: PropagationPolicy::DirectedDustComponent,
    semantic_version: 1,
};

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
    PROPAGATION_SEMANTICS.policy.recompute(world)
}

fn recompute_directed_dust_component(world: &mut World) -> Vec<Position> {
    let dirty = world.take_dirty();
    if dirty.is_empty() {
        return Vec::new();
    }

    let active_dust = active_dust_networks(world, &dirty);

    // `target` 以扁平索引為鍵；佇列只帶位置，因為它要做的就是拿位置去問
    // `dust_connections`。
    let mut queue: VecDeque<(Position, u8)> = VecDeque::new();
    let mut target: FlatMap<u8> =
        FlatMap::with_capacity_and_hasher(active_dust.len(), FlatBuildHasher::default());

    // 每格紅石粉的初始強度：來自相鄰的非紅石粉訊號源
    for &(flat, pos) in &active_dust {
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
            target.insert(flat, best);
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
                // 同樣地，連接目標必定是世界裡擺著紅石粉的格子。
                let neighbour_flat = world
                    .index(neighbour.x, neighbour.y, neighbour.z)
                    .expect("a dust connection target is a cell in the world");
                let current = target.get(&neighbour_flat).copied().unwrap_or(0);
                if next_strength > current {
                    target.insert(neighbour_flat, next_strength);
                    queue.push_back((neighbour, next_strength));
                }
            }
        }
    }

    // 寫回，收集改變的位置
    let mut changed = Vec::new();
    for &(flat, pos) in &active_dust {
        let want = target.get(&flat).copied().unwrap_or(0);
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

/// 一個髒格的 2 跳鄰域，去重後的 25 個位移。
///
/// 原本是每個髒格都跑一次巢狀展開：origin、6 個鄰居、再 36 個鄰居的鄰居
/// —— 43 筆輸出、四次配置，而其中只有 25 個位置是相異的（L1 半徑 2 的
/// 球）。順序就是原本那 43 筆裡每個位置**第一次**出現的順序，所以種子被
/// 探到的先後、進而 `active` 的順序，跟展開版一模一樣：重複的那 18 筆在
/// 舊版裡本來就只有第一次會有作用（是粉的話第二次被 `visited` 擋掉，不是
/// 粉的話兩次都不會有結果）。
///
/// `two_hop_offsets_are_the_old_expansion_deduped` 就是拿舊的展開重跑一次
/// 再去重，逐筆比對這張表。
const TWO_HOP_OFFSETS: [(i32, i32, i32); 25] = [
    (0, 0, 0),
    (0, 0, -1),
    (0, 0, 1),
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, -2),
    (1, 0, -1),
    (-1, 0, -1),
    (0, 1, -1),
    (0, -1, -1),
    (0, 0, 2),
    (1, 0, 1),
    (-1, 0, 1),
    (0, 1, 1),
    (0, -1, 1),
    (2, 0, 0),
    (1, 1, 0),
    (1, -1, 0),
    (-2, 0, 0),
    (-1, 1, 0),
    (-1, -1, 0),
    (0, 2, 0),
    (0, -2, 0),
];

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
/// 2. **從種子洪水填滿整個網路**：找到種子之後，沿著連接關係走訪整個
///    連通的紅石粉網路（跟下面 BFS 傳播強度用的是同一套 `dust_connections`
///    規則），因為網路裡任何一格的強度都可能因為上游的改變而跟著變 ——
///    只重算種子本身、不管它所在的整條線，會漏掉沿線往後傳的變化。
///
/// 沒被任何髒格波及的網路完全不會出現在回傳值裡，維持原樣不用重算。
///
/// The flood in step 2 walks the connection graph **in both directions**:
/// outgoing `dust_connections` edges, and incoming ones -- cells whose own
/// `dust_connections` reach the cell being expanded. The two are not the same
/// set, because dust edges can be one-way: a descent can be legal while the
/// climb back is refused (the cell the climb would step on cannot carry dust
/// -- wire doubling back under its own staircase), and a climb over glass can
/// be legal while the descent back is blocked (glass supports a dust step).
/// Walking outgoing edges only, a network re-dirtied on the *downstream* side
/// of such an edge never adds its only feeder; the initial-strength loop
/// above deliberately ignores dust neighbours, so the recompute sees no
/// source at all, and the write-back zeroes a run that the feeder -- by
/// `dust_connections`' own answer, in that very world -- still feeds.
/// Measured exactly so on the negotiated `full_adder`'s g11 isolation world:
/// `(56, 1, 99)` settled at 0 while `(57, 2, 99)` read 10 and fed it
/// (`redstone::simulator::differential` is the instrument;
/// `differential::tests::a_one_way_descent_edge_feeds_the_lower_run` and
/// `tests::a_one_way_climb_edge_feeds_the_upper_run` below are the two
/// five-block shapes). Closing the flood over incoming edges makes the active
/// set closed under "is fed by" as well as "feeds", which is what the
/// write-back loop's unconditional zeroing assumes.
fn active_dust_networks(world: &World, dirty: &[usize]) -> Vec<(usize, Position)> {
    let mut visited: FlatSet = FlatSet::default();
    let mut active: Vec<(usize, Position)> = Vec::new();

    for &flat in dirty {
        let (x, y, z) = world.decode(flat);
        let origin = Position::new(x, y, z);

        // 2 跳鄰域：origin 本身、它的 6 個鄰居、以及鄰居的鄰居。
        for (dx, dy, dz) in TWO_HOP_OFFSETS {
            let seed = Position::new(origin.x + dx, origin.y + dy, origin.z + dz);
            // 展開到世界外面的位置在這裡直接跳過。這不是新的判斷：
            // `World::get` 對範圍外一律回傳空氣，空氣永遠不是紅石粉，
            // 所以底下那個 `RedstoneWire` 檢查本來就會把它們濾掉 ——
            // 只是改用扁平索引之後，範圍外的位置根本沒有索引可用。
            let Some(seed_flat) = world.index(seed.x, seed.y, seed.z) else {
                continue;
            };
            if visited.contains(&seed_flat) {
                continue;
            }
            if world.get(seed.x, seed.y, seed.z).kind != BlockKind::RedstoneWire {
                continue;
            }

            // 種子找到了，沿著連接關係洪水填滿整個網路
            visited.insert(seed_flat);
            active.push((seed_flat, seed));
            let mut stack = vec![seed];
            while let Some(pos) = stack.pop() {
                for facing in HORIZONTAL {
                    for neighbour in dust_connections(world, pos, facing).iter() {
                        // `dust_connections` 只回傳真的擺著紅石粉的格子，
                        // 所以一定在範圍內。
                        let flat = world
                            .index(neighbour.x, neighbour.y, neighbour.z)
                            .expect("a dust connection target is a cell in the world");
                        if visited.insert(flat) {
                            active.push((flat, neighbour));
                            stack.push(neighbour);
                        }
                    }

                    // Incoming edges too: `dust_connections` is directed, and
                    // a one-way edge (see the doc comment) would otherwise
                    // leave this cell's only feeder outside the active set --
                    // whose write-back then zeroes a run its feeder still
                    // feeds. Same-layer edges are symmetric by construction
                    // (each end merely requires the other to hold dust), so
                    // only the two diagonal candidates can carry an edge the
                    // outgoing walk does not mirror -- and the edge itself is
                    // still asked of `dust_connections`, from the candidate's
                    // side, so the connection rules stay defined in exactly
                    // one place.
                    let sideways = pos.offset(facing);
                    for feeder in [sideways.up(), sideways.down()] {
                        if world.get(feeder.x, feeder.y, feeder.z).kind != BlockKind::RedstoneWire {
                            continue;
                        }
                        let flat = world
                            .index(feeder.x, feeder.y, feeder.z)
                            .expect("a feeder was just read as dust, so it is in the world");
                        if dust_connections(world, feeder, facing.opposite())
                            .iter()
                            .any(|target| target == pos)
                            && visited.insert(flat)
                        {
                            active.push((flat, feeder));
                            stack.push(feeder);
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
    if !world.flags_at(pos.x, pos.y, pos.z).is_conductive() {
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

/// 從中繼器或比較器後方的方塊讀取其可被二極體看見的充能。
///
/// 直接相鄰的粉／元件仍由呼叫端透過 `signal_from` 讀取；這個函式補上
/// 二極體獨有的後方導電方塊讀取，包含弱充能。弱充能方塊不能把訊號重新
/// 灌回另一格粉，但二極體會直接讀它。普通 `signal_from` 保持 strong only，
/// 避免把這個特例擴散到其他元件。
pub fn diode_rear_signal(world: &World, rear: Position) -> u8 {
    let (kind, strength) = block_signal_at(world, rear);
    if kind != BlockPower::None {
        strength
    } else {
        0
    }
}

/// 從 `from` 看向 `to` 是哪個方向。兩者不相鄰時回傳 `None`。
fn direction_from(from: Position, to: Position) -> Option<Facing> {
    ALL_SIX
        .into_iter()
        .find(|&facing| from.offset(facing) == to)
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
            assert_eq!(
                w.get(x, 1, 0).power,
                0,
                "dust at x={x} after source removal"
            );
        }
    }

    /// The climb-direction twin of
    /// `differential::tests::a_one_way_descent_edge_feeds_the_lower_run`:
    /// a dust edge that exists **upward only**, because glass sits directly
    /// above the lower cell -- glass is not conductive, so the climb is
    /// allowed, but its top face supports a dust step, so the descent back is
    /// refused (`connectivity`'s two rules read opposite polarities of
    /// different cells, which is exactly how an edge gets to be one-way).
    ///
    /// ```text
    ///   y=2   G     X  lever   G = glass over W; X = dust on the step
    ///   y=1   W  #             W = dust; # = the stone step under X
    ///   y=0   #                stone floor under W
    /// ```
    ///
    /// Flipping the lever off dirties only the lever's cell; the seeds reach
    /// X but W is three hops away, and no outgoing edge of X reaches W. A
    /// flood over outgoing `dust_connections` alone recomputes {X} with no
    /// source and zeroes it, while W still reads 15 and still feeds X at 14
    /// by `dust_connections`' own answer -- the same bookkeeping gap measured
    /// on the negotiated full_adder's g11 isolation world, in the other
    /// vertical direction. The flood walks incoming edges too, so X must
    /// follow its feeder down to 14.
    ///
    /// The rig drains the dirty set before the flip (the write-back marks
    /// its own writes dirty, so the recompute right after a change still has
    /// last round's cells as seeds -- a settled world does not). Without the
    /// drain this test passes even under the outgoing-only flood, measured:
    /// W stays a seed of its own and hides the gap.
    #[test]
    fn a_one_way_climb_edge_feeds_the_upper_run() {
        let mut w = World::new(8, 4, 3);
        w.set(0, 1, 0, redstone_block()); // steady source beside W
        w.set(1, 0, 0, stone());
        w.set(1, 1, 0, dust()); // W
        w.set(1, 2, 0, named("minecraft:glass", BlockKind::Glass)); // the one-way maker
        w.set(2, 1, 0, stone()); // the step
        w.set(2, 2, 0, dust()); // X
        let mut lever = named("minecraft:lever", BlockKind::Lever);
        lever.lit = true;
        w.set(3, 2, 0, lever); // toggling source beside X

        // The premise, asked of the connection rules themselves: W -> X
        // exists, X -> W does not.
        let w_pos = Position::new(1, 1, 0);
        let x_pos = Position::new(2, 2, 0);
        assert!(
            dust_connections(&w, w_pos, Facing::East)
                .iter()
                .any(|p| p == x_pos),
            "the climb W -> X must exist for this test to test anything"
        );
        for facing in HORIZONTAL {
            assert!(
                !dust_connections(&w, x_pos, facing)
                    .iter()
                    .any(|p| p == w_pos),
                "X -> W must not exist -- the edge must be one-way"
            );
        }

        // Settle: recompute until a pass changes nothing, then once more to
        // drain the write-back's own dirty marks.
        while !recompute_dust_strengths(&mut w).is_empty() {}
        assert_eq!(w.get(1, 1, 0).power, 15, "W beside the redstone block");
        assert_eq!(w.get(2, 2, 0).power, 15, "X beside the lit lever");

        // Flip the lever off: its own cell is now the only dirty one.
        let mut off = w.get(3, 2, 0).clone();
        off.lit = false;
        w.set(3, 2, 0, off);
        recompute_dust_strengths(&mut w);

        assert_eq!(w.get(1, 1, 0).power, 15, "W is untouched");
        assert_eq!(
            w.get(2, 2, 0).power,
            14,
            "X's only remaining source is W, one climb step away -- a flood \
             over outgoing edges alone leaves this stale at 0"
        );
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

    /// The const table is the old nested expansion, deduped on first
    /// occurrence -- rebuilt here rather than trusted.
    #[test]
    fn two_hop_offsets_are_the_old_expansion_deduped() {
        let origin = Position::new(0, 0, 0);
        let mut frontier = vec![origin];
        let mut emitted = vec![origin];
        for _ in 0..2 {
            let mut next = Vec::new();
            for &p in &frontier {
                for facing in ALL_SIX {
                    next.push(p.offset(facing));
                }
            }
            emitted.extend_from_slice(&next);
            frontier = next;
        }
        assert_eq!(emitted.len(), 43, "1 + 6 + 36 emissions");

        let mut seen = HashSet::new();
        let first_occurrences: Vec<Position> =
            emitted.into_iter().filter(|p| seen.insert(*p)).collect();
        let table: Vec<Position> = TWO_HOP_OFFSETS
            .iter()
            .map(|(dx, dy, dz)| Position::new(*dx, *dy, *dz))
            .collect();
        assert_eq!(first_occurrences, table);
    }

    /// The same edits in any order must leave the same world and change the
    /// same cells.
    ///
    /// `World::take_dirty` hands the recomputation its dirty cells out of a
    /// `HashSet`, so their order is already whatever the allocator and the
    /// random seed made it that run: nothing downstream may depend on it. The
    /// maps inside are keyed by flat index and hashed by a seedless hasher, and
    /// neither is ever iterated -- this is what says so out loud, against
    /// permutations chosen here rather than whichever one the set happened to
    /// hand over.
    #[test]
    fn the_order_the_dirty_cells_arrive_in_changes_nothing() {
        // Two runs, a branch joining them and a source.
        let edits: Vec<(i32, i32, i32, BlockState)> = {
            let mut edits = Vec::new();
            for x in 1..=6 {
                edits.push((x, 0, 0, stone()));
                edits.push((x, 1, 0, dust()));
                edits.push((x, 0, 2, stone()));
                edits.push((x, 1, 2, dust()));
            }
            for z in 0..=2 {
                edits.push((6, 0, z, stone()));
                edits.push((6, 1, z, dust()));
            }
            edits.push((0, 1, 0, redstone_block()));
            edits
        };
        let built = || {
            let mut world = World::new(20, 3, 5);
            for (x, y, z, state) in &edits {
                world.set(*x, *y, *z, state.clone());
            }
            world
        };

        // The active set is a property of the world and the dirty cells, not of
        // the order they are handed over in. Permutations picked here, so the
        // test does not depend on a `HashSet`'s mood.
        let mut world = built();
        let mut dirty = world.take_dirty();
        dirty.sort_unstable();
        let active_of = |world: &World, dirty: &[usize]| {
            let mut flats: Vec<usize> = active_dust_networks(world, dirty)
                .into_iter()
                .map(|(flat, _)| flat)
                .collect();
            flats.sort_unstable();
            flats
        };
        let ascending = active_of(&world, &dirty);
        assert!(!ascending.is_empty(), "the fixture must have live dust");
        dirty.reverse();
        assert_eq!(ascending, active_of(&world, &dirty), "reversed");
        let third = dirty.len() / 3;
        dirty.rotate_left(third);
        assert_eq!(ascending, active_of(&world, &dirty), "rotated");

        // And the fixpoint itself, through the whole recomputation, with the
        // edits themselves applied in both orders.
        let settle = |order: &[(i32, i32, i32, BlockState)]| {
            let mut world = World::new(20, 3, 5);
            for (x, y, z, state) in order {
                world.set(*x, *y, *z, state.clone());
            }
            let mut changed = recompute_dust_strengths(&mut world);
            changed.sort_by_key(|at| (at.y, at.z, at.x));
            let powers: Vec<u8> = (0..5)
                .flat_map(|z| (0..20).map(move |x| (x, z)))
                .map(|(x, z)| world.get(x, 1, z).power)
                .collect();
            (changed, powers)
        };
        let forward = settle(&edits);
        let mut reversed = edits.clone();
        reversed.reverse();
        let backward = settle(&reversed);

        assert!(
            forward.1.iter().any(|power| *power > 0),
            "the fixture must actually carry a signal"
        );
        assert_eq!(forward.0, backward.0, "a different set of cells changed");
        assert_eq!(forward.1, backward.1, "a different fixpoint was reached");

        // Running it again from the settled state is still a fixpoint.
        let mut world = built();
        recompute_dust_strengths(&mut world);
        assert!(recompute_dust_strengths(&mut world).is_empty());
    }

    #[test]
    fn recompute_reports_how_many_cells_changed() {
        let mut w = World::new(20, 3, 3);
        lay_wire(&mut w, 5);
        w.set(0, 1, 0, redstone_block());

        let changed = recompute_dust_strengths(&mut w);
        assert_eq!(
            changed.len(),
            5,
            "all five dust cells went from 0 to non-zero"
        );

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
        assert_eq!(
            changed.len(),
            5,
            "all five dust cells should light up from the source"
        );
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
        assert_eq!(
            kind,
            BlockPower::Strong,
            "the comparator strongly powers the stone"
        );
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
        assert_eq!(
            kind,
            BlockPower::Weak,
            "a run's far block is weakly powered"
        );
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
    fn ordinary_signal_from_rejects_a_weakly_powered_block_as_a_dust_source() {
        let mut w = run_into_a_block(3);
        let weak_block = Position::new(4, 1, 2);
        let other_face = Position::new(4, 1, 3);

        w.set(4, 0, 3, stone());
        w.set(4, 1, 3, dust());
        recompute_dust_strengths(&mut w);

        assert_eq!(
            signal_from(&w, weak_block, other_face),
            0,
            "ordinary block-to-dust propagation must remain strong-only"
        );
        assert_eq!(
            w.get(other_face.x, other_face.y, other_face.z).power,
            0,
            "weak power on one face must not re-drive dust on another face"
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
