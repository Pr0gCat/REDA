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
