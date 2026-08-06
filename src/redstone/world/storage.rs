//! World：3D 方塊儲存。
//!
//! 用扁平的 `Vec<u32>` 存 palette 索引，索引順序是 **YZX**
//! （`y * size_x * size_z + z * size_x + x`），與 litematic 和
//! Sponge schematic 一致，讀寫檔案時不需要重排。
//!
//! 扁平陣列而非巢狀 Vec 是刻意的：日後 router 的熱路徑需要
//! cache-friendly 的線性存取，巢狀 Vec 會造成三次指標追蹤。

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::redstone::world::block::{BlockKind, BlockState};
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
    /// 每種方塊種類目前佔用的扁平索引，由 `set` 增量維護。
    ///
    /// 這是給熱路徑用的稀疏索引：模擬器每個 game tick 都要問「所有紅石粉
    /// 在哪」「所有火把／中繼器／比較器在哪」，若靠掃過 `cells()` 全部
    /// 37M+ 格去過濾，成本跟世界體積成正比；實際擺放的方塊往往只有其中
    /// 一小部分（一個真實電路可能只有幾十萬格非空氣）。改成增量維護的
    /// 索引後，這些查詢的成本只跟「擺了幾個那種方塊」成正比。
    ///
    /// **不追蹤空氣**：空氣是預設填充值，世界裡幾乎所有格子都是空氣，
    /// 追蹤它會讓這個索引跟體積一樣大，完全失去意義；反正沒有任何呼叫端
    /// 需要「所有空氣格在哪」。
    ///
    /// 用 `BTreeSet` 而非 `HashSet` 是為了讓走訪順序固定（依扁平索引遞增，
    /// 也就是 YZX 掃描順序）——跟舊版全體積線性掃描的走訪順序一致，行為
    /// 不會因為換了資料結構而改變。
    positions_by_kind: HashMap<BlockKind, BTreeSet<usize>>,
    /// 扁平索引：自從上次被 `take_dirty` 取走以來，被 `set` 動過的格子。
    ///
    /// 這是給 `recompute_dust_strengths` 做增量重算用的：稀疏索引把成本
    /// 從「跟世界體積成正比」降到「跟紅石粉數量成正比」，但一個像
    /// 七段顯示器解碼器那樣的真實電路，紅石粉本身就有十幾萬格 ——
    /// 每個 game tick 都把全部紅石粉重算一次還是太貴，而事實上一個 tick
    /// 通常只有少數幾個元件真的變了狀態。這份清單記錄「這次 tick 有哪些
    /// 格子被寫過」，`recompute_dust_strengths` 只需要以這些格子為起點，
    /// 展開到它們波及得到的紅石粉網路去重算，其餘完全沒被碰到的網路
    /// 保持原樣不用重算。
    ///
    /// 沒有清單示什麼都沒變，`recompute_dust_strengths` 可以直接跳過整個
    /// tick 的計算 —— 這在等待中繼器延遲、佇列暫時空著的 tick 尤其重要。
    dirty: HashSet<usize>,
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
            positions_by_kind: HashMap::new(),
            dirty: HashSet::new(),
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
    ///
    /// 除了更新 palette 索引之外，也順手維護兩份給熱路徑用的簿記：
    ///
    /// - `positions_by_kind`：只有在種類真的改變時才動它（同一格改變其他
    ///   屬性、種類不變的話什麼都不用做），所以這一步是攤銷 O(1)，不會讓
    ///   `set` 變貴。
    /// - `dirty`：不管種類有沒有變都無條件記一筆 —— 就算只是同一種方塊
    ///   換了 `power`／`lit`／`facing` 之類的屬性，也可能改變它對外送出
    ///   的訊號，`recompute_dust_strengths` 需要知道「這格動過」才能決定
    ///   要不要重算它附近的紅石粉網路。寧可多記（成本頂多是重算一個其實
    ///   沒變的網路），也不能漏記（漏記會讓某個網路該重算卻沒重算，變成
    ///   過期狀態）。
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        if let Some(i) = self.index(x, y, z) {
            let old_kind = self
                .palette
                .get(self.cells[i])
                .expect("palette index out of range")
                .kind;
            let new_kind = state.kind;

            let idx = self.palette.intern(state);
            self.cells[i] = idx;
            self.dirty.insert(i);

            if old_kind != new_kind {
                if old_kind != BlockKind::Air {
                    if let Some(positions) = self.positions_by_kind.get_mut(&old_kind) {
                        positions.remove(&i);
                    }
                }
                if new_kind != BlockKind::Air {
                    self.positions_by_kind.entry(new_kind).or_default().insert(i);
                }
            }
        }
    }

    /// 取走並清空目前累積的髒格清單。
    ///
    /// 「取走」是一次性的：呼叫之後這份清單就空了，下一次呼叫只會看到
    /// **這之後**新發生的變化。`recompute_dust_strengths` 用這個決定這次
    /// 要重算哪些紅石粉網路；沒有其他呼叫端需要知道「哪些格子被動過」，
    /// 所以這裡不提供唯讀版本。
    pub fn take_dirty(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.dirty).into_iter().collect()
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    /// 原始的 palette 索引陣列，寫出檔案時需要。
    pub fn cells(&self) -> &[u32] {
        &self.cells
    }

    /// 目前種類為 `kind` 的所有格子，依扁平索引遞增（= YZX 掃描順序）
    /// 回傳。
    ///
    /// 成本跟「這種方塊擺了幾格」成正比，不是世界體積 —— 這是給模擬器
    /// 熱路徑用的：每個 game tick 都要問一次「紅石粉／火把／中繼器／
    /// 比較器在哪」，不能每次都掃過整個世界。
    ///
    /// 空氣一律回傳空迭代器（見 `positions_by_kind` 的說明：空氣沒有被
    /// 追蹤）。
    pub fn positions_of(&self, kind: BlockKind) -> impl Iterator<Item = usize> + '_ {
        self.positions_by_kind.get(&kind).into_iter().flatten().copied()
    }

    /// 把扁平索引還原成座標。跟 `index` 互為反函數。
    pub fn decode(&self, flat: usize) -> (i32, i32, i32) {
        let layer = (self.size_x as usize) * (self.size_z as usize);
        let y = (flat / layer) as i32;
        let rem = flat % layer;
        let z = (rem / self.size_x as usize) as i32;
        let x = (rem % self.size_x as usize) as i32;
        (x, y, z)
    }

    /// 從既有的 palette 與索引陣列建立世界，讀取檔案時需要。
    pub fn from_parts(
        size_x: i32,
        size_y: i32,
        size_z: i32,
        mut palette: Palette,
        cells: Vec<u32>,
    ) -> Self {
        let expected = (size_x as usize) * (size_y as usize) * (size_z as usize);
        assert_eq!(cells.len(), expected, "cell count must match world size");
        // 空氣必須在 palette 裡，否則越界讀取會拿到錯誤的方塊。
        // `intern` 是冪等的：已存在就回傳既有索引。
        let air_index = palette.intern(BlockState::air());

        // 掃一次現成的 cells 建出 positions_by_kind —— 讀檔本來就得碰過
        // 每一格才能建出 `cells` 本身，這裡不會多引入新的漸進成本，之後
        // 每個 game tick 的查詢才是真正省下全體積掃描的地方。
        //
        // 同一輪順便把每個非空氣格子標成髒的：這些格子從來沒經過 `set`，
        // `dirty` 不會自然知道它們的存在，但讀進來的紅石粉 `power` 欄位
        // 完全可能跟檔案裡其他方塊的狀態不一致（沒人保證存檔當下是自洽
        // 的）——第一次 `recompute_dust_strengths` 必須把它們全部當成
        // 「可能受影響」，等同於一次全量重算，之後才切換成增量模式。
        let mut positions_by_kind: HashMap<BlockKind, BTreeSet<usize>> = HashMap::new();
        let mut dirty: HashSet<usize> = HashSet::new();
        for (flat, &palette_idx) in cells.iter().enumerate() {
            let kind = palette
                .get(palette_idx)
                .expect("palette index referenced by a cell must exist")
                .kind;
            if kind != BlockKind::Air {
                positions_by_kind.entry(kind).or_default().insert(flat);
                dirty.insert(flat);
            }
        }

        World {
            size_x,
            size_y,
            size_z,
            cells,
            palette,
            air_index,
            positions_by_kind,
            dirty,
        }
    }
}

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

    #[test]
    fn positions_of_tracks_placed_blocks_by_kind() {
        let mut w = World::new(4, 4, 4);
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        w.set(1, 0, 0, stone.clone());
        w.set(2, 0, 0, stone);

        let flats: Vec<usize> = w.positions_of(BlockKind::Solid).collect();
        assert_eq!(flats.len(), 2, "both placed stones must be tracked");
        for flat in flats {
            let (x, y, z) = w.decode(flat);
            assert_eq!(w.get(x, y, z).kind, BlockKind::Solid);
        }
    }

    #[test]
    fn positions_of_never_reports_air() {
        let w = World::new(4, 4, 4);
        assert_eq!(w.positions_of(BlockKind::Air).count(), 0, "air is never tracked");
    }

    #[test]
    fn overwriting_with_a_different_kind_moves_the_position_between_buckets() {
        let mut w = World::new(4, 4, 4);
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        w.set(1, 0, 0, stone);
        assert_eq!(w.positions_of(BlockKind::Solid).count(), 1);

        let mut wire = BlockState::air();
        wire.kind = BlockKind::RedstoneWire;
        wire.name = "minecraft:redstone_wire".to_string();
        w.set(1, 0, 0, wire);

        assert_eq!(
            w.positions_of(BlockKind::Solid).count(),
            0,
            "the old kind's bucket must lose the position once the block there changes kind"
        );
        assert_eq!(w.positions_of(BlockKind::RedstoneWire).count(), 1);
    }

    #[test]
    fn setting_air_removes_the_position_from_its_bucket() {
        let mut w = World::new(4, 4, 4);
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        w.set(1, 0, 0, stone);
        assert_eq!(w.positions_of(BlockKind::Solid).count(), 1);

        w.set(1, 0, 0, BlockState::air());
        assert_eq!(w.positions_of(BlockKind::Solid).count(), 0);
    }

    #[test]
    fn changing_a_property_without_changing_kind_does_not_duplicate_the_position() {
        let mut w = World::new(4, 4, 4);
        let mut wire = BlockState::air();
        wire.kind = BlockKind::RedstoneWire;
        wire.name = "minecraft:redstone_wire".to_string();
        wire.power = 0;
        w.set(1, 0, 0, wire.clone());

        wire.power = 12;
        w.set(1, 0, 0, wire);

        assert_eq!(
            w.positions_of(BlockKind::RedstoneWire).count(),
            1,
            "re-setting the same kind with a different property must not add a duplicate entry"
        );
    }

    #[test]
    fn from_parts_guarantees_air_for_out_of_bounds_reads() {
        // litematic 檔案的 palette 不保證含空氣
        let mut palette = Palette::new();
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        let stone_idx = palette.intern(stone);

        let w = World::from_parts(2, 1, 1, palette, vec![stone_idx, stone_idx]);

        assert_eq!(w.get(0, 0, 0).kind, BlockKind::Solid, "in-bounds is stone");
        assert_eq!(
            w.get(99, 0, 0).kind,
            BlockKind::Air,
            "out-of-bounds must be air even when the palette had none"
        );
    }

    #[test]
    fn from_parts_builds_the_position_index_from_the_given_cells() {
        let mut palette = Palette::new();
        let mut stone = BlockState::air();
        stone.kind = BlockKind::Solid;
        stone.name = "minecraft:stone".to_string();
        let stone_idx = palette.intern(stone);
        let air_idx = palette.intern(BlockState::air());

        let w = World::from_parts(2, 1, 1, palette, vec![stone_idx, air_idx]);

        let flats: Vec<usize> = w.positions_of(BlockKind::Solid).collect();
        assert_eq!(flats, vec![0], "only the stone cell should be tracked, not the air one");
    }
}
