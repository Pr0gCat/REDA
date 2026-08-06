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
