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
///
/// **三個支撐旗標不是階層關係。** 不要假設 `SUPPORT_FULL` 蘊含
/// `SUPPORT_CENTER` —— 漏斗就是反例：它的頂面是 hollow square，紅石粉
/// 靠遊戲的硬編碼特例放得上去（FULL），但立式火把需要中央的實心面，
/// 放不上去（無 CENTER）。三者各自獨立查詢。
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

/// `BlockKind::Other` 的方塊是不是一般的完整建材方塊。
///
/// 只有在方塊未被兩份導電性清單涵蓋時才會問到這裡。
fn is_ordinary_full_block(name: &str) -> bool {
    java_1_20::CONDUCTIVE_FULL_BLOCKS.contains(&name)
        || java_1_20::FULL_BLOCK_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
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
            BlockKind::Other => is_ordinary_full_block(name),
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
            BlockKind::Solid | BlockKind::Lamp => true,
            BlockKind::Other => is_ordinary_full_block(name),
            _ => false,
        }
    };
    if conductive {
        bits |= BlockFlags::CONDUCTIVE;
    }

    BlockFlags(bits)
}

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

    #[test]
    fn fence_does_not_conduct() {
        let fence = named(BlockKind::Other, "minecraft:oak_fence");
        assert!(
            !flags_of(&fence).is_conductive(),
            "a fence is not a full block and must not conduct"
        );
    }

    #[test]
    fn unknown_blocks_get_no_capabilities() {
        let unknown = named(BlockKind::Other, "minecraft:some_block_we_never_heard_of");
        let f = flags_of(&unknown);
        assert!(!f.can_carry_dust(), "unknown blocks must fail safe");
        assert!(!f.can_carry_repeater());
        assert!(!f.can_carry_torch());
        assert!(!f.is_conductive());
    }

    #[test]
    fn ordinary_building_blocks_still_work_via_suffix() {
        let wool = named(BlockKind::Other, "minecraft:lime_wool");
        let f = flags_of(&wool);
        assert!(f.can_carry_dust(), "wool is an ordinary full block");
        assert!(f.is_conductive(), "wool conducts like stone");
    }

    #[test]
    fn suffix_rule_does_not_override_the_non_conductive_list() {
        // `_block` 後綴不能讓紅石塊與蜂蜜塊變成導體
        for name in ["minecraft:redstone_block", "minecraft:honey_block"] {
            let b = named(BlockKind::Other, name);
            assert!(
                !flags_of(&b).is_conductive(),
                "{name} must stay non-conductive despite the _block suffix"
            );
        }
    }
}
