# 紅石模擬器實作計畫（階段 A2.1）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 讓 REDA 能載入一個紅石電路、施加輸入、推進時間、讀出輸出，且行為與 Minecraft Java 1.20 一致 —— 這是「能證明電路會動」的前提。

**Architecture:** 事件驅動的逐 tick 模擬器。紅石粉的訊號傳播用 BFS 依功率流方向展開（參考 Alternate Current 的做法，非 locational），元件的狀態改變透過 scheduled tick 佇列排程並依 Minecraft 的四級優先權執行。世界狀態就是 A1 的 `World`，模擬器不另建表示。

**Tech Stack:** Rust 2021，無新依賴。

## 執行環境（每個任務都適用）

Rust 工具鏈**不在預設 PATH 上**。所有 `cargo` 指令必須在 bash 裡先設 PATH：

```bash
export PATH="/c/Users/LTY/.cargo/bin:$PATH"
cargo test
```

PowerShell 找不到 cargo，一律用 bash。工具鏈版本：cargo 1.97.1 / rustc 1.97.1。

## Global Constraints

- 目標版本：**Minecraft Java Edition 1.20**。
- **1 redstone tick = 2 game ticks。** 模擬器內部以 **game tick** 計時，對外的延遲數字以 redstone tick 表示。中繼器延遲 1~4 redstone tick = 2~8 game tick。
- **本階段不實作 quasi-connectivity、活塞、觀察者、locational 行為。** v1 的 cell library 只用紅石粉、中繼器、比較器、火把、拉桿、燈與實心方塊；QC 只影響活塞／發射器／投擲器，因此在這個範圍內不會發生。碰到不支援的元件必須**明確報錯**，不得靜默當成空氣。
- **發散是一種可回報的結果，不是要消除的現象。** 振盪電路（例如未收斂的 latch 回授）必須被偵測並回報，不得靠偏離遊戲語意的手段（例如讓火把燒毀永不恢復）換取收斂。
- **強充能與弱充能必須分開。** 只被紅石粉充能的方塊無法再驅動相鄰紅石粉 —— 這是繞線的結構性約束，模擬器搞錯會讓所有驗證失去意義。
- 火把 burnout：60 game tick 內被迫關閉超過 8 次即燒毀，**且會恢復**。
- 命名：不用縮寫、不與 Rust 生態撞名、讀了就知道在幹嘛。
- `cargo clippy --all-targets -- -D warnings` 必須保持乾淨。

## 既有 API（A1 已合併，簽名不得更動）

```rust
// src/redstone/world/block.rs
pub enum BlockKind { Air, Solid, Glass, Slab, RedstoneWire, Repeater, Comparator,
                     Torch, WallTorch, Lever, RedstoneBlock, Lamp, Piston,
                     Button, PressurePlate, WeightedPressurePlate, Observer,
                     Target, DaylightDetector, Other }
pub enum Facing { North, South, East, West, Up, Down }
pub enum SlabHalf { Top, Bottom, Double }
pub struct BlockState {
    pub kind: BlockKind, pub facing: Option<Facing>, pub power: u8,
    pub delay: u8, pub lit: bool, pub half: Option<SlabHalf>,
    pub name: String,
    pub extra_properties: std::collections::BTreeMap<String, String>,
}
impl BlockState { pub fn air() -> Self }

// src/redstone/world/storage.rs
impl World {
    pub fn new(size_x: i32, size_y: i32, size_z: i32) -> Self
    pub fn get(&self, x: i32, y: i32, z: i32) -> &BlockState   // 界外回傳空氣
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState)
    pub fn size(&self) -> (i32, i32, i32)
    pub fn index(&self, x: i32, y: i32, z: i32) -> Option<usize>
}

// src/redstone/rules/taxonomy.rs
pub struct BlockFlags(pub u16);
impl BlockFlags {
    pub fn can_carry_dust(self) -> bool
    pub fn can_carry_repeater(self) -> bool
    pub fn can_carry_torch(self) -> bool
    pub fn can_attach_wall_torch(self) -> bool
    pub fn is_conductive(self) -> bool
}
pub fn flags_of(state: &BlockState) -> BlockFlags

pub enum BlockPower { None, Weak, Strong }
impl BlockPower { pub fn can_repower_dust(self) -> bool }
pub struct PowerOutput { pub drives_dust: bool, pub block_power: BlockPower, pub strength: u8 }
impl PowerOutput { pub const INERT: PowerOutput }
pub fn power_emitted_by(state: &BlockState) -> PowerOutput
```

---

## File Structure

```
src/redstone/simulator/
├── mod.rs           Simulator：公開介面、tick 迴圈、發散偵測
├── position.rs      Position 與方向的向量運算
├── connectivity.rs  紅石粉的連接判定（哪些格子屬於同一條線）
├── propagate.rs     訊號強度傳播與方塊充能
├── component.rs     各元件收到更新時的行為
└── schedule.rs      scheduled tick 佇列與四級優先權
tests/
└── simulator_circuits.rs   端到端電路行為測試
```

| 檔案 | 職責 | 為什麼獨立 |
|---|---|---|
| `position.rs` | 座標與方向 | 被所有其他模組用，必須零依賴 |
| `connectivity.rs` | 「這兩格粉相連嗎」 | 規則最囉唆、最容易錯，要能單獨測 |
| `propagate.rs` | 強度怎麼擴散、方塊怎麼被充能 | 效能熱點，日後要換 SIMD |
| `component.rs` | 每個元件的狀態轉移 | 加新元件只動這裡 |
| `schedule.rs` | 什麼時候輪到誰 | Minecraft 的 tick 語意集中處 |
| `mod.rs` | 對外的 API 與主迴圈 | 使用者只看得到這個 |

---

### Task 1: 座標與方向

**Files:**
- Create: `src/redstone/simulator/position.rs`
- Create: `src/redstone/simulator/mod.rs`
- Modify: `src/redstone/mod.rs`（加 `pub mod simulator;`）

**Interfaces:**
- Consumes: `Facing`（A1）
- Produces:
  - `pub struct Position { pub x: i32, pub y: i32, pub z: i32 }`
  - `impl Position`：`new(x,y,z)`、`offset(self, Facing) -> Position`、`up(self)`、`down(self)`
  - `pub const HORIZONTAL: [Facing; 4]`（北、南、東、西 —— 固定順序）
  - `pub const ALL_SIX: [Facing; 6]`
  - `pub fn opposite(f: Facing) -> Facing`

- [ ] **Step 1: 寫失敗的測試**

`src/redstone/simulator/position.rs`：

```rust
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
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib simulator::position`
Expected: 編譯錯誤 `cannot find type Position in this scope`

- [ ] **Step 3: 實作**

`src/redstone/simulator/position.rs` 頂部：

```rust
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
    match facing {
        Facing::North => Facing::South,
        Facing::South => Facing::North,
        Facing::East => Facing::West,
        Facing::West => Facing::East,
        Facing::Up => Facing::Down,
        Facing::Down => Facing::Up,
    }
}
```

`src/redstone/simulator/mod.rs`：

```rust
//! 紅石模擬器。
//!
//! 逐 game tick 推進，訊號傳播依功率流方向做 BFS（非 locational）。
//!
//! **本階段不支援** quasi-connectivity、活塞、觀察者。碰到這些元件會明確
//! 報錯，不會靜默忽略。

pub mod position;
```

`src/redstone/mod.rs` 加一行：

```rust
pub mod simulator;
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib simulator::position`
Expected: `test result: ok. 4 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/redstone/simulator/ src/redstone/mod.rs
git commit -m "feat: add simulator position and direction primitives"
```

---

### Task 2: 紅石粉的連接判定

**Files:**
- Create: `src/redstone/simulator/connectivity.rs`
- Modify: `src/redstone/simulator/mod.rs`（加 `pub mod connectivity;`）

**Interfaces:**
- Consumes: `Position`、`HORIZONTAL`（Task 1）、`World`、`BlockKind`、`flags_of`（A1）
- Produces:
  - `pub fn dust_connects(world: &World, from: Position, direction: Facing) -> Option<Position>`

**這個任務為什麼困難：** 紅石粉的連接不只是「隔壁有沒有粉」。它會沿方塊爬上爬下，而爬升的條件取決於中間那格是不是導體。搞錯的話電路會在垂直方向斷掉或誤連。

規則（Java 1.20）：

從位置 `L` 往水平方向 `d` 看，鄰居是 `N = L.offset(d)`：

1. **同層**：`N` 是紅石粉 → 相連
2. **往上**：`N.up()` 是紅石粉，**且 `L.up()` 不是導體**（導體會擋住）→ 相連
3. **往下**：`N.down()` 是紅石粉，**且 `N` 不是導體**（導體會擋住）→ 相連

- [ ] **Step 1: 寫失敗的測試**

`src/redstone/simulator/connectivity.rs`：

```rust
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
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib simulator::connectivity`
Expected: 編譯錯誤 `cannot find function dust_connects in this scope`

- [ ] **Step 3: 實作**

`src/redstone/simulator/connectivity.rs` 頂部：

```rust
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
```

`src/redstone/simulator/mod.rs` 加一行：

```rust
pub mod connectivity;
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib simulator::connectivity`
Expected: `test result: ok. 7 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/redstone/simulator/
git commit -m "feat: add redstone dust connectivity rules"
```

---

### Task 3: 訊號強度傳播

**Files:**
- Create: `src/redstone/simulator/propagate.rs`
- Modify: `src/redstone/simulator/mod.rs`

**Interfaces:**
- Consumes: `Position`、`HORIZONTAL`（Task 1）、`dust_connects`（Task 2）、`World`、`power_emitted_by`、`flags_of`、`BlockPower`（A1）
- Produces:
  - `pub const MAX_SIGNAL_STRENGTH: u8 = 15`
  - `pub fn recompute_dust_strengths(world: &mut World) -> usize`（回傳改變的格數）
  - `pub fn block_power_at(world: &World, pos: Position) -> BlockPower`

**做法：** 從所有訊號源開始做 BFS，每往外一格強度 -1。這是 Alternate Current 的思路 —— 依功率流方向展開，而不是原版那種對位置敏感的遞迴，所以結果與座標無關。

- [ ] **Step 1: 寫失敗的測試**

`src/redstone/simulator/propagate.rs`：

```rust
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
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib simulator::propagate`
Expected: 編譯錯誤 `cannot find function recompute_dust_strengths in this scope`

- [ ] **Step 3: 實作**

`src/redstone/simulator/propagate.rs` 頂部：

```rust
//! 訊號強度傳播與方塊充能。
//!
//! 從所有訊號源開始 BFS，每經過一格紅石粉強度 -1，強度 0 就停止。
//!
//! 用 BFS 依**功率流方向**展開，而不是原版那種對方塊放置順序敏感的遞迴 ——
//! 所以同一個電路擺在任何座標結果都相同。這是 Alternate Current 的思路。

use std::collections::VecDeque;

use crate::redstone::rules::taxonomy::{flags_of, power_emitted_by, BlockPower};
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
        for facing in ALL_SIX {
            let neighbour = pos.offset(facing);
            let output = power_emitted_by(world.get(neighbour.x, neighbour.y, neighbour.z));
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
        let output = power_emitted_by(neighbour_state);

        match output.block_power {
            BlockPower::Strong => return BlockPower::Strong,
            BlockPower::Weak => best = BlockPower::Weak,
            BlockPower::None => {}
        }
    }

    best
}
```

`src/redstone/simulator/mod.rs` 加一行：

```rust
pub mod propagate;
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib simulator::propagate`
Expected: `test result: ok. 6 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/redstone/simulator/
git commit -m "feat: add signal strength propagation"
```

---

## 後續任務（尚未展開）

本計畫的前三個任務建立了傳播的基礎。剩餘任務會在前三個完成、且傳播行為經測試確認後展開，因為它們的介面取決於前三個實際長成的樣子：

- **Task 4: 火把行為** —— 反相、1 redstone tick 延遲、burnout 與其恢復
- **Task 5: 中繼器行為** —— 1~4 tick 延遲、側面鎖存
- **Task 6: 比較器行為** —— 比較模式與減法模式
- **Task 7: tick 排程與四級優先權**
- **Task 8: Simulator 公開介面** —— 施加輸入、推進、讀輸出、發散偵測
- **Task 9: 端到端電路測試** —— 反相器、AND、半加器的真值表

刻意不預先寫死這些任務的程式碼：Task 1–3 會揭露傳播 API 的真實形狀，而後續任務全部建在其上。等前三個綠了再展開，比現在猜完整的十任務計畫可靠。
