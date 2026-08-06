# 紅石世界模型與格式 I/O 實作計畫

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 建立紅石電路的記憶體表示、方塊行為規則查詢、以及 `.litematic` 檔案的讀寫，使後續的模擬器有可靠的地基。

**Architecture:** 單一 crate `reda`，內部以 module 劃分。`redstone/world` 用 palette + 扁平陣列儲存 3D 方塊，`redstone/rules` 提供純查表的方塊行為判定（三個彼此獨立的屬性），`formats` 處理 gzip NBT 與 litematic 的位元打包。所有規則查詢都是無分支的位元運算或查表，不寫 if-else 鏈。

**Tech Stack:** Rust 2021、`fastnbt`（NBT 序列化）、`flate2`（gzip）、`thiserror`（錯誤型別）

## 執行環境（每個任務都適用）

Rust 工具鏈**不在預設 PATH 上**。所有 `cargo` 指令必須在 bash 裡先設 PATH：

```bash
export PATH="/c/Users/LTY/.cargo/bin:$PATH"
cargo test
```

PowerShell 找不到 cargo，一律用 bash。工具鏈版本：cargo 1.97.1 / rustc 1.97.1。

## Global Constraints

- 目標版本：**Minecraft Java Edition 1.20**。1.21 的元件（銅燈）不在此計畫範圍。
- litematic 版本：**`SCHEMATIC_VERSION = 7`**（對應 MC 1.20.5+）。讀取時也要接受 6。
- litematic 的位元打包是**跨 long 的舊式慣例**（pre-1.16），**不是** 1.16+ 的非跨界慣例。用錯會讀出垃圾。
- litematic 的 `bits_per_entry` 最小值是 **2**，即使 palette 只有 1 個項目。
- litematic 的索引順序是 **YZX**：`index = y * (sizeX * sizeZ) + z * sizeX + x`
- 方塊行為由**三個彼此獨立**的屬性決定：頂面支撐型別、導電性、不透明度（不透明度與紅石無關，不建模）。**絕不用單一 `is_solid` 判斷任何事。**
- 模組命名：不用縮寫、不與 Rust 生態撞名（禁用 `lib/`、`core/`）、讀了就知道在幹嘛。
- 規則查詢的結果以**位元旗標**表示，判定邏輯**集中在單一函式**並由資料表驅動。呼叫端一律呼叫該函式，**不得自行以方塊種類做 if-else 判斷** —— 為日後的 SIMD/GPU 與規則版本化保留空間。（判定函式內部使用 `match` 與條件式是正常且必要的，此約束針對的是「判定邏輯散布到各處」。）

---

## File Structure

```
Cargo.toml
src/
├── lib.rs                      crate 根，re-export 公開型別
├── redstone/
│   ├── mod.rs
│   ├── world/
│   │   ├── mod.rs
│   │   ├── block.rs            BlockState：方塊種類與其狀態欄位
│   │   ├── palette.rs          Palette：BlockState ↔ u32 索引
│   │   └── storage.rs          World：扁平陣列 + 尺寸 + palette
│   └── rules/
│       ├── mod.rs
│       ├── taxonomy.rs         SupportType、Conductivity 的判定
│       └── java_1_20.rs        1.20 的方塊屬性資料表
└── formats/
    ├── mod.rs
    ├── nbt.rs                  gzip NBT 的讀寫封裝
    ├── bitpack.rs              litematica 的跨 long 位元打包
    └── litematic.rs            .litematic 的結構與轉換
tests/
├── bitpack_vectors.rs          位元打包的已知向量測試
└── litematic_roundtrip.rs      讀→寫→讀 的一致性測試
```

各檔案職責：

| 檔案 | 職責 | 為什麼獨立 |
|---|---|---|
| `block.rs` | 方塊種類與狀態欄位的型別定義 | 被所有其他模組依賴，必須零依賴 |
| `palette.rs` | 去重與索引映射 | 純資料結構，可獨立測試 |
| `storage.rs` | 3D 空間存取 | 效能熱點，日後要換扁平化/SIMD |
| `taxonomy.rs` | 方塊行為的**判定邏輯** | 邏輯與資料分離，資料可換版本 |
| `java_1_20.rs` | 方塊行為的**資料表** | 1.21 只需新增一個同構檔案 |
| `bitpack.rs` | 位元打包 | 這是最容易寫錯的地方，必須能單獨用已知向量驗證 |
| `litematic.rs` | 檔案結構 ↔ World | 格式細節集中處 |

---

### Task 1: 專案骨架

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/redstone/mod.rs`
- Create: `src/formats/mod.rs`

**Interfaces:**
- Consumes: 無（第一個任務）
- Produces: crate `reda` 可編譯、`cargo test` 可執行

- [ ] **Step 1: 建立 Cargo.toml**

```toml
[package]
name = "reda"
version = "0.1.0"
edition = "2021"
description = "Redstone EDA - compile HDL into latency-minimised Minecraft redstone"
license = "MIT"

[dependencies]
fastnbt = "2.6"
flate2 = "1.0"
serde = { version = "1.0", features = ["derive"] }
thiserror = "2.0"

[dev-dependencies]
```

- [ ] **Step 2: 建立模組骨架**

`src/lib.rs`：

```rust
//! REDA — 把 HDL 編譯成延遲最小化的 Minecraft 紅石電路。

pub mod formats;
pub mod redstone;
```

`src/redstone/mod.rs`：

```rust
//! 紅石本身：方塊規則、世界模型、模擬器。

pub mod rules;
pub mod world;
```

`src/formats/mod.rs`：

```rust
//! 檔案讀寫：NBT、litematic。

pub mod bitpack;
pub mod litematic;
pub mod nbt;
```

同時建立空的佔位檔，讓它編得過：

`src/redstone/world/mod.rs`：
```rust
pub mod block;
pub mod palette;
pub mod storage;
```

`src/redstone/rules/mod.rs`：
```rust
pub mod java_1_20;
pub mod taxonomy;
```

其餘八個檔案先各放一行佔位，讓 module 宣告編得過。這些檔案會在 Task 2–9 逐一被實作內容取代：

- `src/redstone/world/block.rs`（Task 2）
- `src/redstone/world/palette.rs`（Task 3）
- `src/redstone/world/storage.rs`（Task 3）
- `src/redstone/rules/taxonomy.rs`（Task 4、5）
- `src/redstone/rules/java_1_20.rs`（Task 4）
- `src/formats/nbt.rs`（Task 7）
- `src/formats/bitpack.rs`（Task 6）
- `src/formats/litematic.rs`（Task 8、9）

每個檔案的內容就一行：

```rust
// 佔位，於後續任務實作
```

- [ ] **Step 3: 驗證編譯**

Run: `cargo build`
Expected: `Finished dev [unoptimized + debuginfo] target(s)`，無錯誤

- [ ] **Step 4: 驗證測試框架可執行**

Run: `cargo test`
Expected: `test result: ok. 0 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/
git commit -m "chore: scaffold reda crate with module skeleton"
```

---

### Task 2: 方塊狀態表示

**Files:**
- Create: `src/redstone/world/block.rs`

**Interfaces:**
- Consumes: 無
- Produces:
  - `pub enum BlockKind`（`Air`, `Solid`, `Glass`, `Slab`, `RedstoneWire`, `Repeater`, `Comparator`, `Torch`, `WallTorch`, `Lever`, `RedstoneBlock`, `Lamp`, `Piston`, `Other`）
  - `pub enum Facing { North, South, East, West, Up, Down }`
  - `pub enum SlabHalf { Top, Bottom, Double }`
  - `pub struct BlockState { pub kind: BlockKind, pub facing: Option<Facing>, pub power: u8, pub delay: u8, pub lit: bool, pub half: Option<SlabHalf>, pub name: String }`
  - `impl BlockState { pub fn air() -> Self }`

- [ ] **Step 1: 寫失敗的測試**

在 `src/redstone/world/block.rs` 底部：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_has_no_power_and_no_facing() {
        let b = BlockState::air();
        assert_eq!(b.kind, BlockKind::Air);
        assert_eq!(b.power, 0);
        assert_eq!(b.facing, None);
    }

    #[test]
    fn redstone_wire_carries_power_level() {
        let mut b = BlockState::air();
        b.kind = BlockKind::RedstoneWire;
        b.power = 15;
        assert_eq!(b.power, 15);
    }

    #[test]
    fn repeater_records_facing_and_delay() {
        let mut b = BlockState::air();
        b.kind = BlockKind::Repeater;
        b.facing = Some(Facing::North);
        b.delay = 3;
        assert_eq!(b.facing, Some(Facing::North));
        assert_eq!(b.delay, 3);
    }
}
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib block`
Expected: 編譯錯誤 `cannot find type BlockState in this scope`

- [ ] **Step 3: 實作**

在 `src/redstone/world/block.rs` 頂部（測試模組之前）：

```rust
//! 方塊狀態的型別定義。這是整個 crate 的底層資料，不依賴任何其他模組。

/// 方塊的種類。只列出紅石相關的；其餘一律 `Other`，靠 `name` 區分。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    Air,
    /// 一般的完整方塊（石頭、泥土、羊毛…）
    Solid,
    /// 完整方塊但不導電（玻璃、發光石、TNT、冰…）
    Glass,
    Slab,
    RedstoneWire,
    Repeater,
    Comparator,
    /// 立在地上的紅石火把
    Torch,
    /// 附在牆上的紅石火把
    WallTorch,
    Lever,
    RedstoneBlock,
    Lamp,
    Piston,
    /// 其他方塊，行為由 `BlockState::name` 查表決定
    Other,
}

/// 方塊朝向。中繼器、比較器、牆上火把、活塞都需要。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facing {
    North,
    South,
    East,
    West,
    Up,
    Down,
}

/// 半磚位於方塊格的哪一半。這決定它的頂面能不能承載東西。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlabHalf {
    /// 上半磚：頂面是完整實心面
    Top,
    /// 下半磚：頂面不是完整面
    Bottom,
    /// 雙層半磚：等同完整方塊，而且導電
    Double,
}

/// 一個方塊的完整狀態。
///
/// `name` 保留原始的 Minecraft 方塊 ID（例如 `minecraft:smooth_stone`），
/// 因為方塊分類（§2.2）必須查表，不能從 `kind` 推導。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockState {
    pub kind: BlockKind,
    pub facing: Option<Facing>,
    /// 紅石粉的訊號強度 0..=15；其他方塊為 0
    pub power: u8,
    /// 中繼器的延遲 1..=4；其他方塊為 0
    pub delay: u8,
    /// 火把、燈是否亮著
    pub lit: bool,
    pub half: Option<SlabHalf>,
    /// 原始 Minecraft 方塊 ID
    pub name: String,
}

impl BlockState {
    /// 空氣。這是世界的預設填充值。
    pub fn air() -> Self {
        BlockState {
            kind: BlockKind::Air,
            facing: None,
            power: 0,
            delay: 0,
            lit: false,
            half: None,
            name: "minecraft:air".to_string(),
        }
    }
}
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib block`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/redstone/world/block.rs
git commit -m "feat: add block state representation"
```

---

### Task 3: Palette 與 3D 儲存

**Files:**
- Create: `src/redstone/world/palette.rs`
- Create: `src/redstone/world/storage.rs`
- Modify: `src/redstone/world/block.rs`（`BlockState` 的 derive 要加 `Hash`，見 Step 3）

**Interfaces:**
- Consumes: `BlockState`、`BlockKind`（Task 2）
- Produces:
  - `pub struct Palette`，方法 `new()`、`intern(&mut self, BlockState) -> u32`、`get(&self, u32) -> Option<&BlockState>`、`len(&self) -> usize`
  - `pub struct World`，方法 `new(size_x, size_y, size_z) -> Self`、`get(&self, x, y, z) -> &BlockState`、`set(&mut self, x, y, z, BlockState)`、`size(&self) -> (i32, i32, i32)`、`index(&self, x, y, z) -> Option<usize>`
  - `World` 內部使用 `Vec<u32>` 扁平陣列，索引順序 **YZX**

- [ ] **Step 1: 寫 Palette 的失敗測試**

`src/redstone/world/palette.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState};

    #[test]
    fn interning_the_same_state_twice_returns_the_same_index() {
        let mut p = Palette::new();
        let a = p.intern(BlockState::air());
        let b = p.intern(BlockState::air());
        assert_eq!(a, b);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn different_states_get_different_indices() {
        let mut p = Palette::new();
        let air = p.intern(BlockState::air());
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        let stone_idx = p.intern(stone);
        assert_ne!(air, stone_idx);
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn get_returns_the_interned_state() {
        let mut p = Palette::new();
        let idx = p.intern(BlockState::air());
        assert_eq!(p.get(idx).unwrap().kind, BlockKind::Air);
    }
}
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib palette`
Expected: 編譯錯誤 `cannot find type Palette in this scope`

- [ ] **Step 3: 實作 Palette**

`src/redstone/world/palette.rs` 頂部：

```rust
//! Palette：把重複的 BlockState 去重成整數索引。
//!
//! 紅石電路裡絕大多數方塊是空氣或少數幾種石頭，palette 讓世界的儲存
//! 從「每格一個 BlockState」變成「每格一個 u32」。

use std::collections::HashMap;

use crate::redstone::world::block::BlockState;

/// BlockState ↔ u32 索引的雙向映射。
#[derive(Debug, Clone, Default)]
pub struct Palette {
    entries: Vec<BlockState>,
    lookup: HashMap<BlockState, u32>,
}

impl Palette {
    pub fn new() -> Self {
        Palette {
            entries: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    /// 取得該狀態的索引；若未出現過則新增。
    pub fn intern(&mut self, state: BlockState) -> u32 {
        if let Some(&idx) = self.lookup.get(&state) {
            return idx;
        }
        let idx = self.entries.len() as u32;
        self.entries.push(state.clone());
        self.lookup.insert(state, idx);
        idx
    }

    pub fn get(&self, index: u32) -> Option<&BlockState> {
        self.entries.get(index as usize)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 依索引順序取得所有項目，寫出檔案時需要。
    pub fn entries(&self) -> &[BlockState] {
        &self.entries
    }
}
```

`BlockState` 需要 `Hash` 才能當 HashMap 的 key。修改 `src/redstone/world/block.rs` 的 derive：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlockState {
```

- [ ] **Step 4: 執行 Palette 測試確認通過**

Run: `cargo test --lib palette`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 5: 寫 World 的失敗測試**

`src/redstone/world/storage.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState};

    #[test]
    fn new_world_is_all_air() {
        let w = World::new(4, 4, 4);
        assert_eq!(w.get(0, 0, 0).kind, BlockKind::Air);
        assert_eq!(w.get(3, 3, 3).kind, BlockKind::Air);
    }

    #[test]
    fn set_then_get_returns_the_same_block() {
        let mut w = World::new(4, 4, 4);
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        w.set(1, 2, 3, stone);
        assert_eq!(w.get(1, 2, 3).kind, BlockKind::Solid);
        assert_eq!(w.get(0, 0, 0).kind, BlockKind::Air);
    }

    #[test]
    fn index_uses_yzx_order() {
        let w = World::new(2, 3, 5);
        // index = y * (size_x * size_z) + z * size_x + x
        assert_eq!(w.index(0, 0, 0), Some(0));
        assert_eq!(w.index(1, 0, 0), Some(1));
        assert_eq!(w.index(0, 0, 1), Some(2));
        assert_eq!(w.index(0, 1, 0), Some(10));
    }

    #[test]
    fn out_of_bounds_index_is_none() {
        let w = World::new(2, 2, 2);
        assert_eq!(w.index(2, 0, 0), None);
        assert_eq!(w.index(-1, 0, 0), None);
    }
}
```

- [ ] **Step 6: 執行測試確認失敗**

Run: `cargo test --lib storage`
Expected: 編譯錯誤 `cannot find type World in this scope`

- [ ] **Step 7: 實作 World**

`src/redstone/world/storage.rs` 頂部：

```rust
//! World：3D 方塊儲存。
//!
//! 用扁平的 `Vec<u32>` 存 palette 索引，索引順序是 **YZX**
//! （`y * size_x * size_z + z * size_x + x`），與 litematic 和
//! Sponge schematic 一致，讀寫檔案時不需要重排。
//!
//! 扁平陣列而非巢狀 Vec 是刻意的：日後 router 的熱路徑需要
//! cache-friendly 的線性存取，巢狀 Vec 會造成三次指標追蹤。

use crate::redstone::world::block::BlockState;
use crate::redstone::world::palette::Palette;

#[derive(Debug, Clone)]
pub struct World {
    size_x: i32,
    size_y: i32,
    size_z: i32,
    /// 每格一個 palette 索引，長度 = size_x * size_y * size_z
    cells: Vec<u32>,
    palette: Palette,
    /// 空氣在 palette 裡的索引，快取起來避免重複查找
    air_index: u32,
}

impl World {
    /// 建立全空氣的世界。
    pub fn new(size_x: i32, size_y: i32, size_z: i32) -> Self {
        assert!(size_x > 0 && size_y > 0 && size_z > 0, "world size must be positive");
        let mut palette = Palette::new();
        let air_index = palette.intern(BlockState::air());
        let count = (size_x as usize) * (size_y as usize) * (size_z as usize);
        World {
            size_x,
            size_y,
            size_z,
            cells: vec![air_index; count],
            palette,
            air_index,
        }
    }

    pub fn size(&self) -> (i32, i32, i32) {
        (self.size_x, self.size_y, self.size_z)
    }

    /// 把座標轉成扁平索引。超出範圍回傳 `None`。
    pub fn index(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        if x < 0 || y < 0 || z < 0 || x >= self.size_x || y >= self.size_y || z >= self.size_z {
            return None;
        }
        let i = (y as usize) * (self.size_x as usize) * (self.size_z as usize)
            + (z as usize) * (self.size_x as usize)
            + (x as usize);
        Some(i)
    }

    /// 讀取一格。超出範圍時回傳空氣（router 的邊界處理靠這個簡化）。
    pub fn get(&self, x: i32, y: i32, z: i32) -> &BlockState {
        match self.index(x, y, z) {
            Some(i) => self
                .palette
                .get(self.cells[i])
                .expect("palette index out of range"),
            None => self
                .palette
                .get(self.air_index)
                .expect("air must be in palette"),
        }
    }

    /// 寫入一格。超出範圍時靜默忽略。
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        if let Some(i) = self.index(x, y, z) {
            let idx = self.palette.intern(state);
            self.cells[i] = idx;
        }
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// 原始的 palette 索引陣列，寫出檔案時需要。
    pub fn cells(&self) -> &[u32] {
        &self.cells
    }

    /// 從既有的 palette 與索引陣列建立世界，讀取檔案時需要。
    pub fn from_parts(
        size_x: i32,
        size_y: i32,
        size_z: i32,
        palette: Palette,
        cells: Vec<u32>,
    ) -> Self {
        let expected = (size_x as usize) * (size_y as usize) * (size_z as usize);
        assert_eq!(cells.len(), expected, "cell count must match world size");
        let air_index = palette
            .entries()
            .iter()
            .position(|b| b.name == "minecraft:air")
            .unwrap_or(0) as u32;
        World {
            size_x,
            size_y,
            size_z,
            cells,
            palette,
            air_index,
        }
    }
}
```

- [ ] **Step 8: 執行測試確認通過**

Run: `cargo test --lib storage`
Expected: `test result: ok. 4 passed; 0 failed`

- [ ] **Step 9: Commit**

```bash
git add src/redstone/world/palette.rs src/redstone/world/storage.rs src/redstone/world/block.rs
git commit -m "feat: add palette and flat 3D world storage"
```

---

### Task 4: 方塊分類

**Files:**
- Create: `src/redstone/rules/taxonomy.rs`
- Create: `src/redstone/rules/java_1_20.rs`

**Interfaces:**
- Consumes: `BlockState`、`BlockKind`、`SlabHalf`（Task 2）
- Produces:
  - `pub struct BlockFlags(u16)`，常數 `SUPPORT_FULL`、`SUPPORT_RIGID`、`SUPPORT_CENTER`、`CONDUCTIVE`、`SIDE_FULL`
  - `pub fn flags_of(state: &BlockState) -> BlockFlags`
  - `impl BlockFlags`：`can_carry_dust()`、`can_carry_repeater()`、`can_carry_torch()`、`is_conductive()`、`can_attach_wall_torch()`

- [ ] **Step 1: 寫失敗的測試**

`src/redstone/rules/taxonomy.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState, SlabHalf};

    fn named(kind: BlockKind, name: &str) -> BlockState {
        let mut b = BlockState::air();
        b.kind = kind;
        b.name = name.to_string();
        b
    }

    fn slab(half: SlabHalf) -> BlockState {
        let mut b = named(BlockKind::Slab, "minecraft:smooth_stone_slab");
        b.half = Some(half);
        b
    }

    #[test]
    fn stone_carries_everything_and_conducts() {
        let f = flags_of(&named(BlockKind::Solid, "minecraft:stone"));
        assert!(f.can_carry_dust());
        assert!(f.can_carry_repeater());
        assert!(f.can_carry_torch());
        assert!(f.is_conductive());
    }

    #[test]
    fn glass_carries_but_does_not_conduct() {
        let f = flags_of(&named(BlockKind::Glass, "minecraft:glass"));
        assert!(f.can_carry_dust());
        assert!(f.can_carry_repeater());
        assert!(!f.is_conductive(), "glass must not conduct redstone");
    }

    #[test]
    fn top_slab_carries_but_bottom_slab_does_not() {
        assert!(flags_of(&slab(SlabHalf::Top)).can_carry_dust());
        assert!(!flags_of(&slab(SlabHalf::Bottom)).can_carry_dust());
    }

    #[test]
    fn single_slab_never_conducts_but_double_slab_does() {
        assert!(!flags_of(&slab(SlabHalf::Top)).is_conductive());
        assert!(!flags_of(&slab(SlabHalf::Bottom)).is_conductive());
        assert!(flags_of(&slab(SlabHalf::Double)).is_conductive());
    }

    #[test]
    fn repeater_support_is_weaker_than_dust_support() {
        // 漏斗頂面是 hollow square：中繼器放得上去，紅石粉放不上去。
        let hopper = named(BlockKind::Other, "minecraft:hopper");
        let f = flags_of(&hopper);
        assert!(f.can_carry_repeater(), "hopper supports repeaters");
        assert!(
            f.can_carry_dust(),
            "hopper is the hardcoded exception that also supports dust"
        );

        // 柵欄只提供 small square：火把可以，中繼器不行。
        let fence = named(BlockKind::Other, "minecraft:oak_fence");
        let g = flags_of(&fence);
        assert!(g.can_carry_torch(), "fence supports a standing torch");
        assert!(!g.can_carry_repeater(), "fence does not support repeaters");
        assert!(!g.can_carry_dust(), "fence does not support dust");
    }

    #[test]
    fn air_supports_nothing() {
        let f = flags_of(&BlockState::air());
        assert!(!f.can_carry_dust());
        assert!(!f.can_carry_repeater());
        assert!(!f.can_carry_torch());
        assert!(!f.is_conductive());
    }
}
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib taxonomy`
Expected: 編譯錯誤 `cannot find function flags_of in this scope`

- [ ] **Step 3: 實作資料表**

`src/redstone/rules/java_1_20.rs`：

```rust
//! Minecraft Java 1.20 的方塊屬性資料表。
//!
//! 這裡只有資料，判定邏輯在 `taxonomy.rs`。1.21 只需新增一個同構的檔案。
//!
//! **重要**：導電性與「是不是完整方塊」無關，必須查表。
//! 完整方塊但不導電的例子：玻璃、發光石、TNT、冰、紅石塊、觀察者、活塞。
//! 不是完整方塊但導電的例子：靈魂沙。

/// 明確不導電的方塊 ID。
///
/// 來源：minecraft.wiki 的 Conductivity 頁。這個清單是白名單式的例外，
/// 其餘的完整方塊預設導電。
pub const NON_CONDUCTIVE: &[&str] = &[
    "minecraft:glass",
    "minecraft:tinted_glass",
    "minecraft:glowstone",
    "minecraft:sea_lantern",
    "minecraft:ice",
    "minecraft:packed_ice",
    "minecraft:blue_ice",
    "minecraft:tnt",
    "minecraft:redstone_block",
    "minecraft:observer",
    "minecraft:piston",
    "minecraft:sticky_piston",
    "minecraft:hopper",
    "minecraft:oak_leaves",
    "minecraft:farmland",
    "minecraft:dirt_path",
    "minecraft:honey_block",
    "minecraft:composter",
    "minecraft:decorated_pot",
    "minecraft:enchanting_table",
];

/// 明確導電、但不是「一般完整方塊」的方塊 ID。
pub const CONDUCTIVE_EXCEPTIONS: &[&str] = &[
    "minecraft:soul_sand",
    "minecraft:slime_block",
    "minecraft:mud",
    "minecraft:target",
    "minecraft:redstone_lamp",
    "minecraft:barrier",
];

/// 頂面提供 hollow square 支撐的方塊：中繼器與比較器放得上去，紅石粉放不上去。
pub const RIGID_ONLY: &[&str] = &["minecraft:composter"];

/// 頂面只提供 small square 支撐的方塊：立式火把放得上去，中繼器與紅石粉都不行。
pub const CENTER_ONLY: &[&str] = &[
    "minecraft:oak_fence",
    "minecraft:spruce_fence",
    "minecraft:birch_fence",
    "minecraft:cobblestone_wall",
    "minecraft:iron_bars",
];

/// 紅石粉的硬編碼特例：漏斗頂面是 hollow square，但遊戲特別允許放紅石粉。
pub const DUST_EXCEPTIONS: &[&str] = &["minecraft:hopper"];

/// 完全不能承載任何東西的方塊。
pub const SUPPORTS_NOTHING: &[&str] = &[
    "minecraft:air",
    "minecraft:cave_air",
    "minecraft:water",
    "minecraft:lava",
    "minecraft:carpet",
    "minecraft:white_carpet",
    "minecraft:campfire",
    "minecraft:oak_leaves",
    "minecraft:chest",
    "minecraft:flower_pot",
];
```

- [ ] **Step 4: 實作判定邏輯**

`src/redstone/rules/taxonomy.rs` 頂部：

```rust
//! 方塊行為的判定。
//!
//! Minecraft **沒有**單一的 `is_solid` 屬性。紅石行為由三個彼此獨立的
//! 屬性決定：頂面支撐型別、導電性、不透明度（不透明度只管光照，與紅石無關，
//! 因此不建模）。
//!
//! 支撐型別對應遊戲程式碼裡的 `SupportType` 三值列舉：
//!
//! | 元件         | 需要的支撐              |
//! |--------------|-------------------------|
//! | 紅石粉       | `FULL`（+ 漏斗特例）    |
//! | 中繼器/比較器 | `RIGID`                 |
//! | 立式火把     | `CENTER`                |
//! | 牆上火把     | 側面 `FULL`             |
//!
//! **三者不是包含關係。** 中繼器的放置條件比紅石粉**寬鬆** —— 漏斗頂面
//! 中繼器放得上去，紅石粉靠特例才行。
//!
//! 所有判定都是位元運算，沒有條件分支鏈 —— 為日後的 SIMD 與 GPU 保留空間。

use crate::redstone::rules::java_1_20;
use crate::redstone::world::block::{BlockKind, BlockState, SlabHalf};

/// 方塊行為的位元旗標。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockFlags(pub u16);

impl BlockFlags {
    /// 頂面是完整實心方形面：紅石粉、拉桿、按鈕放得上去
    pub const SUPPORT_FULL: u16 = 1 << 0;
    /// 頂面是 hollow square 以上：中繼器、比較器、鐵軌放得上去
    pub const SUPPORT_RIGID: u16 = 1 << 1;
    /// 頂面是 small square 以上：立式火把放得上去
    pub const SUPPORT_CENTER: u16 = 1 << 2;
    /// 可被充能並把訊號傳出去
    pub const CONDUCTIVE: u16 = 1 << 3;
    /// 側面是完整面：牆上火把附得上去
    pub const SIDE_FULL: u16 = 1 << 4;

    pub const NONE: BlockFlags = BlockFlags(0);

    #[inline]
    pub fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    #[inline]
    pub fn can_carry_dust(self) -> bool {
        self.has(Self::SUPPORT_FULL)
    }

    #[inline]
    pub fn can_carry_repeater(self) -> bool {
        self.has(Self::SUPPORT_RIGID)
    }

    #[inline]
    pub fn can_carry_torch(self) -> bool {
        self.has(Self::SUPPORT_CENTER)
    }

    #[inline]
    pub fn can_attach_wall_torch(self) -> bool {
        self.has(Self::SIDE_FULL)
    }

    #[inline]
    pub fn is_conductive(self) -> bool {
        self.has(Self::CONDUCTIVE)
    }
}

/// 查出一個方塊的行為旗標。
pub fn flags_of(state: &BlockState) -> BlockFlags {
    let name = state.name.as_str();

    if java_1_20::SUPPORTS_NOTHING.contains(&name) || state.kind == BlockKind::Air {
        return BlockFlags::NONE;
    }

    let mut bits: u16 = 0;

    // ── 支撐型別 ──────────────────────────────────────────────
    if java_1_20::CENTER_ONLY.contains(&name) {
        bits |= BlockFlags::SUPPORT_CENTER;
    } else if java_1_20::RIGID_ONLY.contains(&name) {
        bits |= BlockFlags::SUPPORT_RIGID | BlockFlags::SUPPORT_CENTER;
    } else if java_1_20::DUST_EXCEPTIONS.contains(&name) {
        // 漏斗：hollow square，但遊戲特別允許放紅石粉
        bits |= BlockFlags::SUPPORT_FULL | BlockFlags::SUPPORT_RIGID;
    } else {
        let top_is_full = match state.kind {
            BlockKind::Slab => matches!(state.half, Some(SlabHalf::Top) | Some(SlabHalf::Double)),
            BlockKind::Solid | BlockKind::Glass | BlockKind::Lamp => true,
            BlockKind::Piston | BlockKind::RedstoneBlock => true,
            BlockKind::Other => true,
            _ => false,
        };
        if top_is_full {
            bits |= BlockFlags::SUPPORT_FULL
                | BlockFlags::SUPPORT_RIGID
                | BlockFlags::SUPPORT_CENTER
                | BlockFlags::SIDE_FULL;
        }
    }

    // ── 導電性（與支撐型別完全獨立）────────────────────────────
    let conductive = if java_1_20::NON_CONDUCTIVE.contains(&name) {
        false
    } else if java_1_20::CONDUCTIVE_EXCEPTIONS.contains(&name) {
        true
    } else {
        match state.kind {
            // 單層半磚永不導電；雙層半磚等同完整方塊
            BlockKind::Slab => state.half == Some(SlabHalf::Double),
            BlockKind::Solid | BlockKind::Lamp | BlockKind::Other => true,
            _ => false,
        }
    };
    if conductive {
        bits |= BlockFlags::CONDUCTIVE;
    }

    BlockFlags(bits)
}
```

- [ ] **Step 5: 執行測試確認通過**

Run: `cargo test --lib taxonomy`
Expected: `test result: ok. 6 passed; 0 failed`

- [ ] **Step 6: Commit**

```bash
git add src/redstone/rules/
git commit -m "feat: add block taxonomy with three independent properties"
```

---

### Task 5: 充能規則

**Files:**
- Modify: `src/redstone/rules/taxonomy.rs`（追加）

**Interfaces:**
- Consumes: `BlockState`、`BlockKind`（Task 2）。**不依賴 Task 4 的 `BlockFlags`** —— 充能來源只看方塊種類與其狀態，不看支撐型別。
- Produces:
  - `pub enum PowerLevel { None, Weak, Strong }`
  - `pub fn can_power_adjacent_dust(level: PowerLevel) -> bool`
  - `pub fn power_emitted_by(state: &BlockState) -> PowerLevel`

- [ ] **Step 1: 寫失敗的測試**

追加到 `src/redstone/rules/taxonomy.rs` 的 `mod tests` 內：

```rust
    #[test]
    fn weak_power_cannot_drive_adjacent_dust() {
        assert!(!can_power_adjacent_dust(PowerLevel::Weak));
        assert!(can_power_adjacent_dust(PowerLevel::Strong));
        assert!(!can_power_adjacent_dust(PowerLevel::None));
    }

    #[test]
    fn dust_only_emits_weak_power() {
        let mut dust = named(BlockKind::RedstoneWire, "minecraft:redstone_wire");
        dust.power = 15;
        assert_eq!(power_emitted_by(&dust), PowerLevel::Weak);
    }

    #[test]
    fn powered_repeater_emits_strong_power() {
        let mut rep = named(BlockKind::Repeater, "minecraft:repeater");
        rep.lit = true;
        assert_eq!(power_emitted_by(&rep), PowerLevel::Strong);

        let mut off = named(BlockKind::Repeater, "minecraft:repeater");
        off.lit = false;
        assert_eq!(power_emitted_by(&off), PowerLevel::None);
    }

    #[test]
    fn unpowered_dust_emits_nothing() {
        let mut dust = named(BlockKind::RedstoneWire, "minecraft:redstone_wire");
        dust.power = 0;
        assert_eq!(power_emitted_by(&dust), PowerLevel::None);
    }
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib taxonomy`
Expected: 編譯錯誤 `cannot find type PowerLevel in this scope`

- [ ] **Step 3: 實作**

追加到 `src/redstone/rules/taxonomy.rs`（測試模組之前）：

```rust
/// 充能的強度類別。
///
/// 這**不是**訊號強度 0..15，而是「這個充能能驅動什麼」的分類：
///
/// - **強充能**：可驅動相鄰紅石粉（含上下方），也可啟動機械。
///   來源：紅石電源元件、已充能的中繼器、已充能的比較器。
/// - **弱充能**：**不能**驅動相鄰紅石粉，但可啟動相鄰機械、
///   可驅動朝外的中繼器與比較器。
///   來源：**只有紅石粉**。
///
/// 這個區分是紅石繞線的結構性約束 —— 只被紅石粉充能的方塊無法續傳訊號，
/// 所以每一段線都必須以火把、中繼器或比較器收尾。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerLevel {
    None,
    Weak,
    Strong,
}

/// 這種充能能不能驅動相鄰的紅石粉。
#[inline]
pub fn can_power_adjacent_dust(level: PowerLevel) -> bool {
    matches!(level, PowerLevel::Strong)
}

/// 這個方塊對外送出哪一種充能。
pub fn power_emitted_by(state: &BlockState) -> PowerLevel {
    match state.kind {
        // 紅石粉只送出弱充能，而且只給腳下與它指向的方塊
        BlockKind::RedstoneWire => {
            if state.power > 0 {
                PowerLevel::Weak
            } else {
                PowerLevel::None
            }
        }
        // 中繼器與比較器：亮著時對正前方送強充能
        BlockKind::Repeater | BlockKind::Comparator => {
            if state.lit {
                PowerLevel::Strong
            } else {
                PowerLevel::None
            }
        }
        // 火把：強充能正上方，弱充能其他相鄰（排除所附著的方塊）。
        // 這裡回傳的是「最強的那一種」，方向性由呼叫端處理。
        BlockKind::Torch | BlockKind::WallTorch => {
            if state.lit {
                PowerLevel::Strong
            } else {
                PowerLevel::None
            }
        }
        BlockKind::Lever => {
            if state.lit {
                PowerLevel::Strong
            } else {
                PowerLevel::None
            }
        }
        // 紅石塊：不充能任何方塊，只驅動相鄰紅石粉與朝外的中繼器/比較器
        BlockKind::RedstoneBlock => PowerLevel::None,
        _ => PowerLevel::None,
    }
}
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib taxonomy`
Expected: `test result: ok. 10 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/redstone/rules/taxonomy.rs
git commit -m "feat: add strong/weak power distinction"
```

---

### Task 6: litematic 位元打包

**Files:**
- Create: `src/formats/bitpack.rs`
- Create: `tests/bitpack_vectors.rs`

**Interfaces:**
- Consumes: 無
- Produces:
  - `pub fn bits_per_entry(palette_len: usize) -> u32`
  - `pub fn unpack(longs: &[i64], bits: u32, count: usize) -> Vec<u32>`
  - `pub fn pack(values: &[u32], bits: u32) -> Vec<i64>`

- [ ] **Step 1: 寫失敗的測試**

`src/formats/bitpack.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_per_entry_has_a_floor_of_two() {
        assert_eq!(bits_per_entry(1), 2);
        assert_eq!(bits_per_entry(2), 2);
        assert_eq!(bits_per_entry(4), 2);
        assert_eq!(bits_per_entry(5), 3);
        assert_eq!(bits_per_entry(8), 3);
        assert_eq!(bits_per_entry(9), 4);
        assert_eq!(bits_per_entry(256), 8);
        assert_eq!(bits_per_entry(257), 9);
    }

    #[test]
    fn pack_then_unpack_roundtrips() {
        for bits in 2..=16u32 {
            let max = (1u32 << bits) - 1;
            let values: Vec<u32> = (0..100).map(|i| (i * 7) % (max + 1)).collect();
            let packed = pack(&values, bits);
            let unpacked = unpack(&packed, bits, values.len());
            assert_eq!(unpacked, values, "roundtrip failed at bits={bits}");
        }
    }

    #[test]
    fn entries_span_across_long_boundaries() {
        // 3 bits/entry：第 21 個項目（索引 21）跨越第一個和第二個 long。
        // 21 * 3 = 63，所以它只有 1 bit 在第一個 long 裡，2 bits 在第二個。
        // 這正是舊式（pre-1.16）打包慣例，與 1.16+ 的非跨界慣例不同。
        let values: Vec<u32> = (0..25).map(|i| i % 8).collect();
        let packed = pack(&values, 3);
        let unpacked = unpack(&packed, 3, values.len());
        assert_eq!(unpacked[21], values[21], "entry 21 must span two longs");
        assert_eq!(unpacked, values);
    }

    #[test]
    fn unpack_stops_at_requested_count() {
        let values: Vec<u32> = vec![1, 2, 3];
        let packed = pack(&values, 2);
        let unpacked = unpack(&packed, 2, 3);
        assert_eq!(unpacked.len(), 3);
    }
}
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib bitpack`
Expected: 編譯錯誤 `cannot find function bits_per_entry in this scope`

- [ ] **Step 3: 實作**

`src/formats/bitpack.rs` 頂部：

```rust
//! litematica 的位元打包。
//!
//! **這是整個格式最容易寫錯的地方。**
//!
//! litematica 使用**跨 long 的舊式慣例**（pre-1.16）：一個項目的位元
//! 可以橫跨兩個 `i64` 的邊界。這與 Minecraft 1.16+ 區塊儲存所用的
//! 「非跨界」慣例**不同** —— 後者會在每個 long 末尾留下未使用的位元。
//!
//! 用錯慣例不會報錯，只會讀出垃圾，所以本模組有獨立的向量測試。
//!
//! 另外 `bits_per_entry` 的**最小值是 2**，即使 palette 只有一個項目。

/// 給定 palette 大小，算出每個項目佔幾個 bit。最小值是 2。
pub fn bits_per_entry(palette_len: usize) -> u32 {
    let needed = if palette_len <= 1 {
        1
    } else {
        usize::BITS - (palette_len - 1).leading_zeros()
    };
    needed.max(2)
}

/// 從 long array 解出 `count` 個項目。
///
/// `longs` 以有號 `i64` 儲存（NBT 的 LongArray 是有號的），但位元操作
/// 一律當成無號處理。
pub fn unpack(longs: &[i64], bits: u32, count: usize) -> Vec<u32> {
    assert!(bits >= 1 && bits <= 32, "bits must be in 1..=32");
    let mask: u64 = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let mut out = Vec::with_capacity(count);

    for i in 0..count {
        let bit_offset = (i as u64) * (bits as u64);
        let start_long = (bit_offset / 64) as usize;
        let start_bit = (bit_offset % 64) as u32;
        let end_bit = start_bit + bits;

        if start_long >= longs.len() {
            out.push(0);
            continue;
        }

        let value = if end_bit <= 64 {
            // 完全落在一個 long 裡
            ((longs[start_long] as u64) >> start_bit) & mask
        } else {
            // 跨越兩個 long —— 這正是舊式慣例
            let end_long = start_long + 1;
            if end_long >= longs.len() {
                ((longs[start_long] as u64) >> start_bit) & mask
            } else {
                let low_bits = 64 - start_bit;
                let low = (longs[start_long] as u64) >> start_bit;
                let high = (longs[end_long] as u64) << low_bits;
                (low | high) & mask
            }
        };

        out.push(value as u32);
    }

    out
}

/// 把項目打包成 long array，使用與 `unpack` 相同的跨界慣例。
pub fn pack(values: &[u32], bits: u32) -> Vec<i64> {
    assert!(bits >= 1 && bits <= 32, "bits must be in 1..=32");
    if values.is_empty() {
        return Vec::new();
    }
    let total_bits = (values.len() as u64) * (bits as u64);
    let long_count = ((total_bits + 63) / 64) as usize;
    let mut longs = vec![0u64; long_count];
    let mask: u64 = (1u64 << bits) - 1;

    for (i, &v) in values.iter().enumerate() {
        let value = (v as u64) & mask;
        let bit_offset = (i as u64) * (bits as u64);
        let start_long = (bit_offset / 64) as usize;
        let start_bit = (bit_offset % 64) as u32;
        let end_bit = start_bit + bits;

        longs[start_long] |= value << start_bit;

        if end_bit > 64 {
            let low_bits = 64 - start_bit;
            longs[start_long + 1] |= value >> low_bits;
        }
    }

    longs.into_iter().map(|x| x as i64).collect()
}
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib bitpack`
Expected: `test result: ok. 4 passed; 0 failed`

- [ ] **Step 5: 加一個跨實作的已知向量測試**

`tests/bitpack_vectors.rs`：

```rust
//! 已知向量測試：這些數字是手工推導的，用來釘住跨 long 的打包慣例。
//!
//! 如果有人不小心把實作改成 1.16+ 的非跨界慣例，這些測試會失敗。

use reda::formats::bitpack::{pack, unpack};

#[test]
fn two_bit_entries_pack_32_per_long() {
    // 2 bits × 32 = 64 bits = 剛好一個 long
    let values: Vec<u32> = (0..32).map(|i| i % 4).collect();
    let packed = pack(&values, 2);
    assert_eq!(packed.len(), 1, "32 two-bit entries fit in exactly one long");
    assert_eq!(unpack(&packed, 2, 32), values);
}

#[test]
fn five_bit_entries_straddle_boundaries() {
    // 5 bits/entry：第 12 個項目起點在 bit 60，跨越到下一個 long
    let values: Vec<u32> = (0..20).map(|i| (i * 3) % 32).collect();
    let packed = pack(&values, 5);
    let unpacked = unpack(&packed, 5, 20);
    assert_eq!(unpacked, values);
    assert_eq!(
        unpacked[12], values[12],
        "entry 12 starts at bit 60 and must span two longs"
    );
}

#[test]
fn known_vector_three_bits() {
    // 手工推導：values = [1, 2, 3, 4]，3 bits each
    // bit layout (LSB first): 001 010 011 100
    // long[0] = 0b100_011_010_001 = 0x8D1 = 2257
    let values = vec![1u32, 2, 3, 4];
    let packed = pack(&values, 3);
    assert_eq!(packed[0], 2257, "hand-computed packing must match");
    assert_eq!(unpack(&packed, 3, 4), values);
}
```

- [ ] **Step 6: 執行整合測試確認通過**

Run: `cargo test --test bitpack_vectors`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 7: Commit**

```bash
git add src/formats/bitpack.rs tests/bitpack_vectors.rs
git commit -m "feat: add litematica cross-long bit packing with known vectors"
```

---

### Task 7: gzip NBT 讀寫

**Files:**
- Create: `src/formats/nbt.rs`

**Interfaces:**
- Consumes: 無
- Produces:
  - `pub enum FormatError`（`Io`, `Nbt`, `UnsupportedVersion`, `MissingField`）
  - `pub fn read_gzip_nbt<T: DeserializeOwned>(path: &Path) -> Result<T, FormatError>`
  - `pub fn write_gzip_nbt<T: Serialize>(path: &Path, value: &T) -> Result<(), FormatError>`

- [ ] **Step 1: 寫失敗的測試**

`src/formats/nbt.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        name: String,
        count: i32,
    }

    #[test]
    fn gzip_nbt_roundtrips_through_a_temp_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("reda_nbt_roundtrip_test.nbt");

        let original = Sample {
            name: "test".to_string(),
            count: 42,
        };
        write_gzip_nbt(&path, &original).expect("write must succeed");

        let loaded: Sample = read_gzip_nbt(&path).expect("read must succeed");
        assert_eq!(loaded, original);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reading_a_missing_file_is_an_io_error() {
        let path = std::path::Path::new("/definitely/does/not/exist.nbt");
        let result: Result<Sample, _> = read_gzip_nbt(path);
        assert!(matches!(result, Err(FormatError::Io(_))));
    }
}
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib nbt`
Expected: 編譯錯誤 `cannot find function read_gzip_nbt in this scope`

- [ ] **Step 3: 實作**

`src/formats/nbt.rs` 頂部：

```rust
//! gzip 壓縮的 NBT 讀寫。
//!
//! `.litematic` 與 Sponge `.schem` 都是 gzip 過的 NBT。`fastnbt` 本身
//! **不處理 gzip**，所以壓縮層在這裡自己接。

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::de::DeserializeOwned;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("nbt error: {0}")]
    Nbt(String),

    #[error("unsupported schematic version: {0}")]
    UnsupportedVersion(i32),

    #[error("missing required field: {0}")]
    MissingField(String),
}

/// 讀取一個 gzip 壓縮的 NBT 檔案並反序列化。
pub fn read_gzip_nbt<T: DeserializeOwned>(path: &Path) -> Result<T, FormatError> {
    let file = File::open(path)?;
    let mut decoder = GzDecoder::new(file);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes)?;
    fastnbt::from_bytes(&bytes).map_err(|e| FormatError::Nbt(e.to_string()))
}

/// 序列化並寫出成 gzip 壓縮的 NBT 檔案。
pub fn write_gzip_nbt<T: Serialize>(path: &Path, value: &T) -> Result<(), FormatError> {
    let bytes = fastnbt::to_bytes(value).map_err(|e| FormatError::Nbt(e.to_string()))?;
    let file = File::create(path)?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder.write_all(&bytes)?;
    encoder.finish()?;
    Ok(())
}
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib nbt`
Expected: `test result: ok. 2 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/formats/nbt.rs
git commit -m "feat: add gzip NBT read/write"
```

---

### Task 8: litematic 結構定義與讀取

**Files:**
- Create: `src/formats/litematic.rs`

**Interfaces:**
- Consumes: `World`、`Palette`（Task 3）、`BlockState`、`BlockKind`、`Facing`、`SlabHalf`（Task 2）、`bits_per_entry`、`unpack`（Task 6）、`FormatError`、`read_gzip_nbt`（Task 7）
- Produces:
  - `pub fn load(path: &Path) -> Result<World, FormatError>`
  - `pub fn parse_block_name(name: &str, properties: &HashMap<String, String>) -> BlockState`

- [ ] **Step 1: 寫失敗的測試**

`src/formats/litematic.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, Facing, SlabHalf};

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_plain_stone() {
        let b = parse_block_name("minecraft:stone", &props(&[]));
        assert_eq!(b.kind, BlockKind::Solid);
        assert_eq!(b.name, "minecraft:stone");
    }

    #[test]
    fn parses_redstone_wire_power() {
        let b = parse_block_name("minecraft:redstone_wire", &props(&[("power", "11")]));
        assert_eq!(b.kind, BlockKind::RedstoneWire);
        assert_eq!(b.power, 11);
    }

    #[test]
    fn parses_repeater_facing_delay_and_state() {
        let b = parse_block_name(
            "minecraft:repeater",
            &props(&[("facing", "north"), ("delay", "3"), ("powered", "true")]),
        );
        assert_eq!(b.kind, BlockKind::Repeater);
        assert_eq!(b.facing, Some(Facing::North));
        assert_eq!(b.delay, 3);
        assert!(b.lit);
    }

    #[test]
    fn parses_slab_half() {
        let top = parse_block_name(
            "minecraft:smooth_stone_slab",
            &props(&[("type", "top")]),
        );
        assert_eq!(top.half, Some(SlabHalf::Top));

        let double = parse_block_name(
            "minecraft:smooth_stone_slab",
            &props(&[("type", "double")]),
        );
        assert_eq!(double.half, Some(SlabHalf::Double));
    }

    #[test]
    fn parses_glass_as_non_conductive_kind() {
        let b = parse_block_name("minecraft:glass", &props(&[]));
        assert_eq!(b.kind, BlockKind::Glass);
    }

    #[test]
    fn parses_wall_torch_with_facing() {
        let b = parse_block_name(
            "minecraft:redstone_wall_torch",
            &props(&[("facing", "east"), ("lit", "true")]),
        );
        assert_eq!(b.kind, BlockKind::WallTorch);
        assert_eq!(b.facing, Some(Facing::East));
        assert!(b.lit);
    }
}
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib litematic`
Expected: 編譯錯誤 `cannot find function parse_block_name in this scope`

- [ ] **Step 3: 實作結構與方塊名稱解析**

`src/formats/litematic.rs` 頂部：

```rust
//! `.litematic` 格式的讀寫。
//!
//! 結構是 gzip 過的 NBT，根節點包含 `MinecraftDataVersion`、`Version`、
//! `SubVersion`、`Metadata`、`Regions`。每個 region 有 `Position`、`Size`
//! （**可以是負的**）、`BlockStatePalette`、`BlockStates`（LongArray）、
//! `TileEntities`、`Entities`。
//!
//! 目前版本是 **7**（對應 MC 1.20.5+）；讀取時也接受 6，因為兩者的
//! 方塊資料編碼完全相同，差別只在 TileEntity 內的 item stack NBT。
//!
//! **沒有官方規格** —— 這個實作依據的是 Litematica 原始碼與社群逆向文件。

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::formats::bitpack::{bits_per_entry, pack, unpack};
use crate::formats::nbt::{read_gzip_nbt, write_gzip_nbt, FormatError};
use crate::redstone::world::block::{BlockKind, BlockState, Facing, SlabHalf};
use crate::redstone::world::palette::Palette;
use crate::redstone::world::storage::World;

/// 目前寫出的 schematic 版本。
pub const SCHEMATIC_VERSION: i32 = 7;
/// 讀取時接受的最低版本。
pub const MIN_SUPPORTED_VERSION: i32 = 6;
/// MC 1.20.1 的 data version。
pub const DATA_VERSION_1_20: i32 = 3465;

#[derive(Serialize, Deserialize, Debug)]
pub struct LitematicFile {
    #[serde(rename = "MinecraftDataVersion")]
    pub minecraft_data_version: i32,
    #[serde(rename = "Version")]
    pub version: i32,
    #[serde(rename = "SubVersion", default)]
    pub sub_version: i32,
    #[serde(rename = "Metadata")]
    pub metadata: Metadata,
    #[serde(rename = "Regions")]
    pub regions: HashMap<String, Region>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Metadata {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Author", default)]
    pub author: String,
    #[serde(rename = "Description", default)]
    pub description: String,
    #[serde(rename = "RegionCount", default)]
    pub region_count: i32,
    #[serde(rename = "TotalVolume", default)]
    pub total_volume: i32,
    #[serde(rename = "TotalBlocks", default)]
    pub total_blocks: i32,
    #[serde(rename = "TimeCreated", default)]
    pub time_created: i64,
    #[serde(rename = "TimeModified", default)]
    pub time_modified: i64,
    #[serde(rename = "EnclosingSize", default)]
    pub enclosing_size: Vec3,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone, Copy)]
pub struct Vec3 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Region {
    #[serde(rename = "Position")]
    pub position: Vec3,
    #[serde(rename = "Size")]
    pub size: Vec3,
    #[serde(rename = "BlockStatePalette")]
    pub block_state_palette: Vec<PaletteEntry>,
    #[serde(rename = "BlockStates")]
    pub block_states: fastnbt::LongArray,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PaletteEntry {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Properties", default)]
    pub properties: HashMap<String, String>,
}

/// 把 Minecraft 方塊 ID 與其 blockstate properties 轉成我們的 `BlockState`。
///
/// 未知的方塊一律歸類為 `BlockKind::Other`，行為靠 `name` 查表決定
/// （見 `redstone::rules::taxonomy`）。
pub fn parse_block_name(name: &str, properties: &HashMap<String, String>) -> BlockState {
    let kind = match name {
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air" => BlockKind::Air,
        "minecraft:redstone_wire" => BlockKind::RedstoneWire,
        "minecraft:repeater" => BlockKind::Repeater,
        "minecraft:comparator" => BlockKind::Comparator,
        "minecraft:redstone_torch" => BlockKind::Torch,
        "minecraft:redstone_wall_torch" => BlockKind::WallTorch,
        "minecraft:lever" => BlockKind::Lever,
        "minecraft:redstone_block" => BlockKind::RedstoneBlock,
        "minecraft:redstone_lamp" => BlockKind::Lamp,
        "minecraft:piston" | "minecraft:sticky_piston" => BlockKind::Piston,
        "minecraft:glass" | "minecraft:tinted_glass" | "minecraft:glowstone"
        | "minecraft:sea_lantern" | "minecraft:tnt" | "minecraft:ice" => BlockKind::Glass,
        n if n.ends_with("_slab") => BlockKind::Slab,
        "minecraft:stone" | "minecraft:smooth_stone" | "minecraft:cobblestone"
        | "minecraft:dirt" | "minecraft:oak_planks" => BlockKind::Solid,
        _ => BlockKind::Other,
    };

    let facing = properties.get("facing").and_then(|f| match f.as_str() {
        "north" => Some(Facing::North),
        "south" => Some(Facing::South),
        "east" => Some(Facing::East),
        "west" => Some(Facing::West),
        "up" => Some(Facing::Up),
        "down" => Some(Facing::Down),
        _ => None,
    });

    let half = if kind == BlockKind::Slab {
        properties.get("type").and_then(|t| match t.as_str() {
            "top" => Some(SlabHalf::Top),
            "bottom" => Some(SlabHalf::Bottom),
            "double" => Some(SlabHalf::Double),
            _ => None,
        })
    } else {
        None
    };

    let power = properties
        .get("power")
        .and_then(|p| p.parse::<u8>().ok())
        .unwrap_or(0);

    let delay = properties
        .get("delay")
        .and_then(|d| d.parse::<u8>().ok())
        .unwrap_or(0);

    let lit = properties
        .get("lit")
        .or_else(|| properties.get("powered"))
        .map(|v| v == "true")
        .unwrap_or(false);

    BlockState {
        kind,
        facing,
        power,
        delay,
        lit,
        half,
        name: name.to_string(),
    }
}
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib litematic`
Expected: `test result: ok. 6 passed; 0 failed`

- [ ] **Step 5: 實作 load**

追加到 `src/formats/litematic.rs`（測試模組之前）：

```rust
/// 讀取一個 `.litematic` 檔案，回傳第一個 region 的世界。
///
/// 多 region 的檔案目前只取第一個 —— 我們自己產生的檔案永遠是單 region，
/// 而讀取社群結構時多 region 很罕見。
pub fn load(path: &Path) -> Result<World, FormatError> {
    let file: LitematicFile = read_gzip_nbt(path)?;

    if file.version < MIN_SUPPORTED_VERSION {
        return Err(FormatError::UnsupportedVersion(file.version));
    }

    let region = file
        .regions
        .values()
        .next()
        .ok_or_else(|| FormatError::MissingField("Regions".to_string()))?;

    // Size 可以是負的，表示 region 往負方向延伸。取絕對值即可，
    // 因為我們只關心 bounding box 的形狀。
    let size_x = region.size.x.abs();
    let size_y = region.size.y.abs();
    let size_z = region.size.z.abs();

    if size_x == 0 || size_y == 0 || size_z == 0 {
        return Err(FormatError::MissingField("Size".to_string()));
    }

    let mut palette = Palette::new();
    let mut index_map = Vec::with_capacity(region.block_state_palette.len());
    for entry in &region.block_state_palette {
        let state = parse_block_name(&entry.name, &entry.properties);
        index_map.push(palette.intern(state));
    }

    let count = (size_x as usize) * (size_y as usize) * (size_z as usize);
    let bits = bits_per_entry(region.block_state_palette.len());

    // `fastnbt::LongArray` 不保證能直接當 `&[i64]` 用，先收成 Vec。
    // 這裡多一次配置，但讀檔不在熱路徑上。
    let longs: Vec<i64> = region.block_states.iter().copied().collect();
    let raw = unpack(&longs, bits, count);

    // 檔案裡的索引指向檔案自己的 palette，要映射到我們的 palette
    let cells: Vec<u32> = raw
        .into_iter()
        .map(|i| index_map.get(i as usize).copied().unwrap_or(0))
        .collect();

    Ok(World::from_parts(size_x, size_y, size_z, palette, cells))
}
```

- [ ] **Step 6: 驗證編譯**

Run: `cargo build`
Expected: 編譯成功，無錯誤

- [ ] **Step 7: Commit**

```bash
git add src/formats/litematic.rs
git commit -m "feat: add litematic loading"
```

---

### Task 9: litematic 寫出

**Files:**
- Modify: `src/formats/litematic.rs`（追加 `save`）

**Interfaces:**
- Consumes: Task 8 的所有型別、`pack`（Task 6）、`write_gzip_nbt`（Task 7）
- Produces:
  - `pub fn save(path: &Path, world: &World, name: &str) -> Result<(), FormatError>`
  - `pub fn block_state_to_entry(state: &BlockState) -> PaletteEntry`

- [ ] **Step 1: 寫失敗的測試**

追加到 `src/formats/litematic.rs` 的 `mod tests` 內：

```rust
    #[test]
    fn block_state_to_entry_preserves_properties() {
        let b = parse_block_name(
            "minecraft:repeater",
            &props(&[("facing", "north"), ("delay", "3"), ("powered", "true")]),
        );
        let entry = block_state_to_entry(&b);
        assert_eq!(entry.name, "minecraft:repeater");
        assert_eq!(entry.properties.get("facing").map(String::as_str), Some("north"));
        assert_eq!(entry.properties.get("delay").map(String::as_str), Some("3"));
        assert_eq!(entry.properties.get("powered").map(String::as_str), Some("true"));
    }

    #[test]
    fn block_state_to_entry_emits_no_properties_for_plain_blocks() {
        let b = parse_block_name("minecraft:stone", &props(&[]));
        let entry = block_state_to_entry(&b);
        assert!(entry.properties.is_empty());
    }
```

- [ ] **Step 2: 執行測試確認失敗**

Run: `cargo test --lib litematic`
Expected: 編譯錯誤 `cannot find function block_state_to_entry in this scope`

- [ ] **Step 3: 實作**

追加到 `src/formats/litematic.rs`（測試模組之前）：

```rust
/// 把我們的 `BlockState` 轉回 litematic 的 palette 項目。
///
/// 只寫出該方塊真正擁有的 property —— 多寫會讓 Minecraft 拒絕載入。
pub fn block_state_to_entry(state: &BlockState) -> PaletteEntry {
    let mut properties = HashMap::new();

    if let Some(f) = state.facing {
        let s = match f {
            Facing::North => "north",
            Facing::South => "south",
            Facing::East => "east",
            Facing::West => "west",
            Facing::Up => "up",
            Facing::Down => "down",
        };
        properties.insert("facing".to_string(), s.to_string());
    }

    if let Some(h) = state.half {
        let s = match h {
            SlabHalf::Top => "top",
            SlabHalf::Bottom => "bottom",
            SlabHalf::Double => "double",
        };
        properties.insert("type".to_string(), s.to_string());
    }

    match state.kind {
        BlockKind::RedstoneWire => {
            properties.insert("power".to_string(), state.power.to_string());
        }
        BlockKind::Repeater => {
            properties.insert("delay".to_string(), state.delay.to_string());
            properties.insert("powered".to_string(), state.lit.to_string());
        }
        BlockKind::Comparator => {
            properties.insert("powered".to_string(), state.lit.to_string());
        }
        BlockKind::Torch | BlockKind::WallTorch | BlockKind::Lamp => {
            properties.insert("lit".to_string(), state.lit.to_string());
        }
        BlockKind::Lever => {
            properties.insert("powered".to_string(), state.lit.to_string());
        }
        _ => {}
    }

    PaletteEntry {
        name: state.name.clone(),
        properties,
    }
}

/// 把一個世界寫出成 `.litematic`。
pub fn save(path: &Path, world: &World, name: &str) -> Result<(), FormatError> {
    let (size_x, size_y, size_z) = world.size();
    let volume = (size_x as usize) * (size_y as usize) * (size_z as usize);

    let palette_entries: Vec<PaletteEntry> = world
        .palette()
        .entries()
        .iter()
        .map(block_state_to_entry)
        .collect();

    let bits = bits_per_entry(palette_entries.len());
    let packed = pack(world.cells(), bits);

    let non_air = world
        .cells()
        .iter()
        .filter(|&&idx| {
            world
                .palette()
                .get(idx)
                .map(|b| b.kind != BlockKind::Air)
                .unwrap_or(false)
        })
        .count() as i32;

    let region = Region {
        position: Vec3 { x: 0, y: 0, z: 0 },
        size: Vec3 {
            x: size_x,
            y: size_y,
            z: size_z,
        },
        block_state_palette: palette_entries,
        block_states: fastnbt::LongArray::new(packed),
    };

    let mut regions = HashMap::new();
    regions.insert(name.to_string(), region);

    let file = LitematicFile {
        minecraft_data_version: DATA_VERSION_1_20,
        version: SCHEMATIC_VERSION,
        sub_version: 1,
        metadata: Metadata {
            name: name.to_string(),
            author: "REDA".to_string(),
            description: "Generated by REDA".to_string(),
            region_count: 1,
            total_volume: volume as i32,
            total_blocks: non_air,
            time_created: 0,
            time_modified: 0,
            enclosing_size: Vec3 {
                x: size_x,
                y: size_y,
                z: size_z,
            },
        },
        regions,
    };

    write_gzip_nbt(path, &file)
}
```

- [ ] **Step 4: 執行測試確認通過**

Run: `cargo test --lib litematic`
Expected: `test result: ok. 8 passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add src/formats/litematic.rs
git commit -m "feat: add litematic saving"
```

---

### Task 10: 端到端 round-trip 測試

**Files:**
- Create: `tests/litematic_roundtrip.rs`

**Interfaces:**
- Consumes: `World`、`BlockState`、`litematic::load`、`litematic::save`（Tasks 3, 8, 9）
- Produces: 無新型別，這是驗收測試

- [ ] **Step 1: 寫失敗的測試**

`tests/litematic_roundtrip.rs`：

```rust
//! 端到端測試：建構世界 → 存檔 → 讀回 → 逐格比對。
//!
//! 這抓的是「palette 索引映射錯誤」和「位元打包慣例錯誤」——
//! 兩者都不會讓程式報錯，只會靜默讀出垃圾。

use std::collections::HashMap;

use reda::formats::litematic::{load, parse_block_name, save};
use reda::redstone::world::block::{BlockKind, BlockState, SlabHalf};
use reda::redstone::world::storage::World;

fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn empty_world_roundtrips() {
    let path = temp_path("reda_rt_empty.litematic");
    let world = World::new(4, 3, 2);

    save(&path, &world, "empty").expect("save must succeed");
    let loaded = load(&path).expect("load must succeed");

    assert_eq!(loaded.size(), (4, 3, 2));
    for y in 0..3 {
        for z in 0..2 {
            for x in 0..4 {
                assert_eq!(loaded.get(x, y, z).kind, BlockKind::Air);
            }
        }
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn redstone_circuit_roundtrips_with_all_properties_intact() {
    let path = temp_path("reda_rt_circuit.litematic");
    let mut world = World::new(8, 4, 8);

    // 一段紅石粉，強度遞減
    for x in 0..5 {
        world.set(x, 1, 0, parse_block_name("minecraft:stone", &props(&[])));
        let mut dust = parse_block_name(
            "minecraft:redstone_wire",
            &props(&[("power", &(15 - x).to_string())]),
        );
        dust.power = (15 - x) as u8;
        world.set(x, 2, 0, dust);
    }

    // 一個朝北、延遲 3、亮著的中繼器
    world.set(
        5,
        2,
        0,
        parse_block_name(
            "minecraft:repeater",
            &props(&[("facing", "north"), ("delay", "3"), ("powered", "true")]),
        ),
    );

    // 一個上半磚
    world.set(
        6,
        2,
        0,
        parse_block_name("minecraft:smooth_stone_slab", &props(&[("type", "top")])),
    );

    save(&path, &world, "circuit").expect("save must succeed");
    let loaded = load(&path).expect("load must succeed");

    assert_eq!(loaded.size(), (8, 4, 8));

    for x in 0..5 {
        let dust = loaded.get(x, 2, 0);
        assert_eq!(dust.kind, BlockKind::RedstoneWire, "dust at x={x}");
        assert_eq!(dust.power, (15 - x) as u8, "dust power at x={x}");
    }

    let rep = loaded.get(5, 2, 0);
    assert_eq!(rep.kind, BlockKind::Repeater);
    assert_eq!(rep.delay, 3);
    assert!(rep.lit);

    let slab = loaded.get(6, 2, 0);
    assert_eq!(slab.kind, BlockKind::Slab);
    assert_eq!(slab.half, Some(SlabHalf::Top));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn large_palette_forces_wider_bit_packing_and_still_roundtrips() {
    // 超過 16 種方塊 → bits_per_entry 從 4 變 5，逼出跨 long 的情況
    let path = temp_path("reda_rt_wide.litematic");
    let mut world = World::new(20, 1, 1);

    for x in 0..20 {
        let mut b = BlockState::air();
        b.kind = BlockKind::Other;
        b.name = format!("minecraft:test_block_{x}");
        world.set(x, 0, 0, b);
    }

    save(&path, &world, "wide").expect("save must succeed");
    let loaded = load(&path).expect("load must succeed");

    for x in 0..20 {
        assert_eq!(
            loaded.get(x, 0, 0).name,
            format!("minecraft:test_block_{x}"),
            "block at x={x}"
        );
    }

    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 2: 執行測試確認失敗或通過**

Run: `cargo test --test litematic_roundtrip`
Expected: 三個測試全部通過。若失敗，最可能的原因是 Task 6 的位元打包慣例寫錯，或 Task 8 的 palette 索引映射錯誤。

- [ ] **Step 3: 執行完整測試套件**

Run: `cargo test`
Expected: 所有測試通過，`0 failed`

- [ ] **Step 4: 檢查 clippy 乾淨**

Run: `cargo clippy -- -D warnings`
Expected: 無警告

- [ ] **Step 5: Commit**

```bash
git add tests/litematic_roundtrip.rs
git commit -m "test: add end-to-end litematic roundtrip verification"
```

---

## 完成後的狀態

這個計畫完成後，`reda` crate 能夠：

- 用 palette + 扁平 YZX 陣列表示任意大小的紅石電路
- 查詢任何方塊的三個獨立屬性（支撐型別、導電性、側面支撐），全部是位元運算
- 區分強充能與弱充能，這是繞線的結構性約束
- 讀寫 `.litematic`，位元打包慣例經已知向量測試釘住

**還不能做的**（下一份計畫 A2 的範圍）：模擬紅石行為。目前只有靜態的世界表示與規則查詢。

## 給 A2 的介面承諾

A2（模擬器）會依賴以下介面，實作 A1 時不得更動簽名：

```rust
// 世界存取
World::get(&self, x: i32, y: i32, z: i32) -> &BlockState
World::set(&mut self, x: i32, y: i32, z: i32, state: BlockState)
World::size(&self) -> (i32, i32, i32)
World::index(&self, x: i32, y: i32, z: i32) -> Option<usize>

// 規則查詢
flags_of(state: &BlockState) -> BlockFlags
BlockFlags::can_carry_dust(self) -> bool
BlockFlags::is_conductive(self) -> bool
power_emitted_by(state: &BlockState) -> PowerLevel
can_power_adjacent_dust(level: PowerLevel) -> bool

// 檔案
litematic::load(path: &Path) -> Result<World, FormatError>
litematic::save(path: &Path, world: &World, name: &str) -> Result<(), FormatError>
```
