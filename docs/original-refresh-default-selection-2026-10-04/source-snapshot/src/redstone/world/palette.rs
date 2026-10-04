//! Palette：把重複的 BlockState 去重成整數索引。
//!
//! 紅石電路裡絕大多數方塊是空氣或少數幾種石頭，palette 讓世界的儲存
//! 從「每格一個 BlockState」變成「每格一個 u32」。

use std::collections::HashMap;

use crate::redstone::rules::taxonomy::{flags_of, BlockFlags};
use crate::redstone::world::block::BlockState;

/// BlockState ↔ u32 索引的雙向映射。
#[derive(Debug, Clone, Default)]
pub struct Palette {
    entries: Vec<BlockState>,
    /// 每個項目的分類旗標，跟 `entries` 同索引。
    ///
    /// `flags_of` 要查好幾個以方塊名字為鍵的字串集合，而這裡的項目一旦
    /// 進來就不會再改 —— 同一個 `BlockState` 問幾千次得到的都是同一個
    /// 答案。所以在 intern 當下算一次存起來，熱路徑改用索引拿。
    flags: Vec<BlockFlags>,
    lookup: HashMap<BlockState, u32>,
}

impl Palette {
    pub fn new() -> Self {
        Palette {
            entries: Vec::new(),
            flags: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    /// 取得該狀態的索引；若未出現過則新增。
    pub fn intern(&mut self, state: BlockState) -> u32 {
        if let Some(&idx) = self.lookup.get(&state) {
            return idx;
        }
        let idx = self.entries.len() as u32;
        self.flags.push(flags_of(&state));
        self.entries.push(state.clone());
        self.lookup.insert(state, idx);
        idx
    }

    pub fn get(&self, index: u32) -> Option<&BlockState> {
        self.entries.get(index as usize)
    }

    /// 該項目的分類旗標 —— 跟 `flags_of(self.get(index))` 同一個答案，
    /// 只是不用再照名字分類一次。
    pub fn flags(&self, index: u32) -> Option<BlockFlags> {
        self.flags.get(index as usize).copied()
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

    /// 快取的旗標必須跟現算的完全一致 —— 這是整個快取唯一的正確性條件。
    #[test]
    fn cached_flags_match_flags_of_for_every_entry() {
        use crate::redstone::world::block::{Facing, SlabHalf};

        let mut palette = Palette::new();
        let mut states = vec![BlockState::air()];
        for (name, kind) in [
            ("minecraft:stone", BlockKind::Solid),
            ("minecraft:glass", BlockKind::Glass),
            ("minecraft:redstone_wire", BlockKind::RedstoneWire),
            ("minecraft:redstone_torch", BlockKind::Torch),
            ("minecraft:redstone_wall_torch", BlockKind::WallTorch),
            ("minecraft:repeater", BlockKind::Repeater),
            ("minecraft:comparator", BlockKind::Comparator),
            ("minecraft:redstone_lamp", BlockKind::Lamp),
            ("minecraft:lever", BlockKind::Lever),
            ("minecraft:hopper", BlockKind::Other),
            ("minecraft:honey_block", BlockKind::Other),
            ("minecraft:oak_slab", BlockKind::Slab),
        ] {
            let mut state = BlockState::air();
            state.kind = kind;
            state.name = name.to_string();
            states.push(state.clone());
            // 同一個名字不同屬性是不同項目，旗標也可能不同。
            for half in [SlabHalf::Top, SlabHalf::Bottom, SlabHalf::Double] {
                let mut slabbed = state.clone();
                slabbed.half = Some(half);
                states.push(slabbed);
            }
            let mut facing = state.clone();
            facing.facing = Some(Facing::North);
            facing.lit = true;
            states.push(facing);
        }

        let indices: Vec<u32> = states
            .iter()
            .map(|state| palette.intern(state.clone()))
            .collect();
        for (state, index) in states.iter().zip(indices) {
            assert_eq!(
                palette.flags(index),
                Some(flags_of(state)),
                "{} 的快取旗標跟現算的不一致",
                state.name
            );
            assert_eq!(palette.get(index), Some(state));
        }
        assert_eq!(palette.flags(u32::MAX), None);
    }

    #[test]
    fn get_returns_the_interned_state() {
        let mut p = Palette::new();
        let idx = p.intern(BlockState::air());
        assert_eq!(p.get(idx).unwrap().kind, BlockKind::Air);
    }
}
