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
use crate::redstone::world::block::{BlockKind, BlockState, Facing, SlabHalf};

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

/// 這個名稱的方塊是不是完整立方體 —— 也就是頂面與側面都是完整面。
///
/// **這只回答形狀，不回答導電性。** 兩者獨立：玻璃是完整立方體但不導電，
/// 靈魂沙不是完整立方體卻導電。把兩者混在一份清單裡正是先前
/// `packed_ice` 拿到零旗標的原因。
fn has_full_cube_shape(name: &str) -> bool {
    java_1_20::CONDUCTIVE_FULL_BLOCKS.contains(&name)
        || java_1_20::CONDUCTIVE_EXCEPTIONS.contains(&name)
        || java_1_20::FULL_SHAPE_NON_CONDUCTIVE.contains(&name)
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
            BlockKind::Observer | BlockKind::Target => true,
            BlockKind::Button
            | BlockKind::PressurePlate
            | BlockKind::WeightedPressurePlate
            | BlockKind::DaylightDetector => false,
            BlockKind::Other => has_full_cube_shape(name),
            _ => false,
        };

        // 側面是否為完整面 —— 這與頂面支撐是**獨立的問題**。
        // 半磚的側面是 16×8，牆上火把附不上去；只有完整立方體才有完整側面。
        let side_is_full = match state.kind {
            BlockKind::Slab => state.half == Some(SlabHalf::Double),
            BlockKind::Solid
            | BlockKind::Glass
            | BlockKind::Lamp
            | BlockKind::Observer
            | BlockKind::Target
            | BlockKind::Piston
            | BlockKind::RedstoneBlock => true,
            BlockKind::Other => has_full_cube_shape(name),
            _ => false,
        };

        if top_is_full {
            bits |= BlockFlags::SUPPORT_FULL
                | BlockFlags::SUPPORT_RIGID
                | BlockFlags::SUPPORT_CENTER;
        }
        if side_is_full {
            bits |= BlockFlags::SIDE_FULL;
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
            BlockKind::Button
            | BlockKind::PressurePlate
            | BlockKind::WeightedPressurePlate
            | BlockKind::DaylightDetector => false,
            BlockKind::Observer | BlockKind::Target => false,
            BlockKind::Other => has_full_cube_shape(name),
            _ => false,
        }
    };
    if conductive {
        bits |= BlockFlags::CONDUCTIVE;
    }

    BlockFlags(bits)
}

/// 一個方塊對相鄰方塊送出的充能種類。
///
/// 這與「會不會驅動相鄰紅石粉」是**兩個獨立的問題** —— 見 `PowerOutput`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockPower {
    None,
    /// 弱充能：被充能的方塊**不能**再驅動相鄰紅石粉，但能啟動相鄰機械、
    /// 也能驅動背對它的中繼器與比較器。只有紅石粉會產生弱充能。
    Weak,
    /// 強充能：被充能的方塊**可以**驅動相鄰紅石粉。
    /// 由電源元件、已充能的中繼器與比較器產生。
    Strong,
}

impl BlockPower {
    /// 被這種充能充到的方塊，能不能再驅動相鄰的紅石粉。
    ///
    /// 這是紅石繞線的結構性約束：只被紅石粉充能的方塊無法續傳訊號，
    /// 所以每一段線都必須以火把、中繼器或比較器收尾。
    #[inline]
    pub fn can_repower_dust(self) -> bool {
        matches!(self, BlockPower::Strong)
    }
}

/// 一個元件對外送出的訊號。
///
/// 兩個軸是獨立的，不能合併成單一的強／弱：
///
/// | 元件     | 驅動相鄰紅石粉 | 充能相鄰方塊 |
/// |----------|----------------|--------------|
/// | 紅石塊   | ✔              | **無**       |
/// | 紅石粉   | ✔（相連）      | 弱           |
/// | 拉桿     | ✔              | 強           |
/// | 中繼器   | ✔（正前方）    | 強（正前方） |
///
/// **方向性不在這個型別裡。** 火把強充能正上方、弱充能其他相鄰；中繼器
/// 只作用於正前方。這裡回傳的是該元件最強的輸出，實際套用到哪些座標
/// 由模擬器決定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerOutput {
    /// 直接讓相鄰的紅石粉亮起（不經由中間的方塊）
    pub drives_dust: bool,
    /// 對相鄰方塊送出的充能種類
    pub block_power: BlockPower,
    /// 訊號強度 0..=15
    pub strength: u8,
}

impl PowerOutput {
    /// 什麼都不送出。
    pub const INERT: PowerOutput = PowerOutput {
        drives_dust: false,
        block_power: BlockPower::None,
        strength: 0,
    };
}

/// 這個方塊對外送出什麼訊號。
pub fn power_emitted_by(state: &BlockState) -> PowerOutput {
    match state.kind {
        // 紅石粉：與相鄰的粉相連，並**弱**充能腳下與它指向的方塊。
        // 弱充能的方塊無法再驅動紅石粉 —— 這就是每段線都要以主動元件收尾的原因。
        BlockKind::RedstoneWire if state.power > 0 => PowerOutput {
            drives_dust: true,
            block_power: BlockPower::Weak,
            strength: state.power,
        },

        // 中繼器：只對正前方輸出，強充能，固定 15。
        BlockKind::Repeater if state.lit => PowerOutput {
            drives_dust: true,
            block_power: BlockPower::Strong,
            strength: 15,
        },

        // 比較器：只對正前方輸出，強充能，但強度是類比的（0..15），
        // 存在 `state.power`。這是比較器存在的意義 —— 硬編成 15
        // 等於把它變成一個開關。
        BlockKind::Comparator if state.lit => PowerOutput {
            drives_dust: true,
            block_power: BlockPower::Strong,
            strength: state.power,
        },

        // 火把：強充能正上方，弱充能其他相鄰（但**不含**它所附著的方塊）。
        // 方向性由模擬器處理，這裡給最強的那種。
        BlockKind::Torch | BlockKind::WallTorch if state.lit => PowerOutput {
            drives_dust: true,
            block_power: BlockPower::Strong,
            strength: 15,
        },

        // 拉桿、按鈕、壓力板：強充能所附著／下方的方塊。
        BlockKind::Lever | BlockKind::Button | BlockKind::PressurePlate if state.lit => {
            PowerOutput {
                drives_dust: true,
                block_power: BlockPower::Strong,
                strength: 15,
            }
        }

        // 觀察者：只從背面送出，強充能。
        BlockKind::Observer if state.lit => PowerOutput {
            drives_dust: true,
            block_power: BlockPower::Strong,
            strength: 15,
        },

        // 紅石塊：**驅動相鄰紅石粉，但不充能任何方塊**。
        // 這正是舊的三值 PowerLevel 表達不了、因而回傳 None 的情形。
        BlockKind::RedstoneBlock => PowerOutput {
            drives_dust: true,
            block_power: BlockPower::None,
            strength: 15,
        },

        // 標靶、日光感測器與重量壓力板：驅動相鄰紅石粉，不充能方塊，強度是類比的。
        BlockKind::Target | BlockKind::DaylightDetector | BlockKind::WeightedPressurePlate => {
            PowerOutput {
                drives_dust: state.power > 0,
                block_power: BlockPower::None,
                strength: state.power,
            }
        }

        _ => PowerOutput::INERT,
    }
}

/// 這個方塊往 `direction` 方向送出什麼訊號。
///
/// **方向性是紅石的核心語意，不是細節。** 兩個決定性的例子：
///
/// - **火把不充能它所附著的方塊。** 那正是火把能當反相器的原因 —— 若少了這條，
///   火把塔會自己餵自己，反相器變成鎖存器。
/// - **中繼器與比較器只對正前方輸出。** 否則它會變成四向分接器，並倒灌回自己的輸入。
///
/// `direction` 是**從這個方塊看出去**的方向。
pub fn power_emitted_toward(state: &BlockState, direction: Facing) -> PowerOutput {
    let full = power_emitted_by(state);
    if full == PowerOutput::INERT {
        return PowerOutput::INERT;
    }

    match state.kind {
        // 中繼器與比較器：只有正前方（輸出端）。Minecraft Wiki 對兩者
        // `facing` 的定義都是「從輸出指向輸入的方向」，所以輸出方向是
        // `facing` 的反方向，不是 `facing` 本身。
        BlockKind::Repeater | BlockKind::Comparator => {
            if state.facing == Some(direction.opposite()) {
                full
            } else {
                PowerOutput::INERT
            }
        }

        // 立式火把：強充能正上方，弱充能其他方向，但**完全不碰下方**
        // （下方就是它所附著的方塊）。
        BlockKind::Torch => match direction {
            Facing::Down => PowerOutput::INERT,
            Facing::Up => full,
            _ => PowerOutput {
                block_power: BlockPower::Weak,
                ..full
            },
        },

        // 牆上火把：同理，但所附著的方塊在 `facing` 的**反方向**。
        // （`facing` 記錄的是火把頭朝外的方向。）
        BlockKind::WallTorch => {
            let attached_to = state.facing.map(Facing::opposite);
            if Some(direction) == attached_to {
                PowerOutput::INERT
            } else if direction == Facing::Up {
                full
            } else {
                PowerOutput {
                    block_power: BlockPower::Weak,
                    ..full
                }
            }
        }

        // 紅石粉：**弱**充能腳下的方塊。水平方向的充能取決於粉的連接形狀，
        // 由 `propagate` 依連接關係處理，這裡只回答垂直的部分。
        BlockKind::RedstoneWire => match direction {
            Facing::Down => full,
            _ => PowerOutput::INERT,
        },

        // 紅石塊：驅動相鄰紅石粉但不充能任何方塊 —— `power_emitted_by` 已經
        // 把 block_power 設成 None，所以各方向一致。
        BlockKind::RedstoneBlock => full,

        // 其餘（拉桿、按鈕、壓力板、觀察者、標靶、日光感測器）目前一律各向同性。
        // 它們的方向性等各自的元件實作時再收斂。
        _ => full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, BlockState, Facing, SlabHalf};

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

    #[test]
    fn weak_power_cannot_repower_dust() {
        assert!(!BlockPower::Weak.can_repower_dust());
        assert!(BlockPower::Strong.can_repower_dust());
        assert!(!BlockPower::None.can_repower_dust());
    }

    #[test]
    fn dust_only_weakly_powers_blocks() {
        let mut dust = named(BlockKind::RedstoneWire, "minecraft:redstone_wire");
        dust.power = 15;
        let out = power_emitted_by(&dust);
        assert_eq!(out.block_power, BlockPower::Weak);
        assert!(out.drives_dust);
        assert_eq!(out.strength, 15);
    }

    #[test]
    fn powered_repeater_strongly_powers_blocks() {
        let mut rep = named(BlockKind::Repeater, "minecraft:repeater");
        rep.lit = true;
        let out = power_emitted_by(&rep);
        assert_eq!(out.block_power, BlockPower::Strong);
        assert_eq!(out.strength, 15);

        let mut off = named(BlockKind::Repeater, "minecraft:repeater");
        off.lit = false;
        assert_eq!(power_emitted_by(&off), PowerOutput::INERT);
    }

    #[test]
    fn unpowered_dust_emits_nothing() {
        let mut dust = named(BlockKind::RedstoneWire, "minecraft:redstone_wire");
        dust.power = 0;
        assert_eq!(power_emitted_by(&dust), PowerOutput::INERT);
    }

    #[test]
    fn redstone_block_drives_dust_but_powers_no_block() {
        // 這是舊的三值 PowerLevel 表達不了、因而被寫成「不發電」的情形
        let rb = named(BlockKind::RedstoneBlock, "minecraft:redstone_block");
        let out = power_emitted_by(&rb);
        assert!(out.drives_dust, "a redstone block drives adjacent dust");
        assert_eq!(
            out.block_power,
            BlockPower::None,
            "a redstone block powers no block"
        );
        assert_eq!(out.strength, 15);
    }

    #[test]
    fn dust_carries_its_own_signal_strength() {
        for level in [1u8, 7, 14, 15] {
            let mut dust = named(BlockKind::RedstoneWire, "minecraft:redstone_wire");
            dust.power = level;
            assert_eq!(power_emitted_by(&dust).strength, level);
        }
    }

    #[test]
    fn wall_torches_cannot_attach_to_slab_sides() {
        // 半磚的側面是 16×8，不是完整面
        assert!(
            !flags_of(&slab(SlabHalf::Top)).can_attach_wall_torch(),
            "a top slab has no full side face"
        );
        assert!(
            !flags_of(&slab(SlabHalf::Bottom)).can_attach_wall_torch(),
            "a bottom slab has no full side face"
        );
        assert!(
            flags_of(&slab(SlabHalf::Double)).can_attach_wall_torch(),
            "a double slab is a full cube"
        );
    }

    #[test]
    fn wall_torches_attach_to_full_cubes() {
        assert!(flags_of(&named(BlockKind::Solid, "minecraft:stone")).can_attach_wall_torch());
        assert!(flags_of(&named(BlockKind::Glass, "minecraft:glass")).can_attach_wall_torch());
    }

    #[test]
    fn top_support_and_side_support_are_independent() {
        // 上半磚頂面放得了東西，側面卻附不了火把 —— 兩個軸各自獨立
        let f = flags_of(&slab(SlabHalf::Top));
        assert!(f.can_carry_dust(), "a top slab holds dust");
        assert!(!f.can_attach_wall_torch(), "but has no full side");
    }

    #[test]
    fn conducting_exception_blocks_have_coherent_flags() {
        // 導電卻什麼都放不上去是矛盾的旗標組合
        for (kind, name) in [
            (BlockKind::Other, "minecraft:soul_sand"),
            (BlockKind::Target, "minecraft:target"),
        ] {
            let b = named(kind, name);
            let f = flags_of(&b);
            assert!(f.is_conductive(), "{name} conducts");
            assert!(f.can_carry_dust(), "{name} must also hold dust");
            assert!(f.can_carry_repeater(), "{name} must also hold a repeater");
        }
    }

    #[test]
    fn input_components_are_not_inert() {
        // 少了這些，模擬器載入真實電路時會看不到任何輸入源
        for (kind, name) in [
            (BlockKind::Button, "minecraft:stone_button"),
            (BlockKind::PressurePlate, "minecraft:stone_pressure_plate"),
            (BlockKind::Observer, "minecraft:observer"),
        ] {
            let mut b = named(kind, name);
            b.lit = true;
            let out = power_emitted_by(&b);
            assert!(out.drives_dust, "{name} must drive dust when active");
            assert_eq!(out.strength, 15, "{name} strength");
        }
    }

    #[test]
    fn full_cubes_support_things_even_when_they_do_not_conduct() {
        // 支撐與導電是獨立的兩個問題 —— 冰的家族全都是完整立方體
        for name in [
            "minecraft:packed_ice",
            "minecraft:blue_ice",
            "minecraft:glowstone",
            "minecraft:tnt",
        ] {
            let b = named(BlockKind::Other, name);
            let f = flags_of(&b);
            assert!(f.can_carry_dust(), "{name} is a full cube and holds dust");
            assert!(f.can_attach_wall_torch(), "{name} has full sides");
            assert!(!f.is_conductive(), "{name} does not conduct");
        }
    }

    #[test]
    fn blocks_that_support_nothing_still_support_nothing() {
        for name in ["minecraft:farmland", "minecraft:dirt_path", "minecraft:composter"] {
            let b = named(BlockKind::Other, name);
            let f = flags_of(&b);
            assert!(!f.can_carry_dust(), "{name} must not hold dust");
            assert!(!f.is_conductive(), "{name} must not conduct");
        }
    }

    #[test]
    fn a_torch_does_not_power_the_block_it_stands_on() {
        // 這是火把能當反相器的全部原因 —— 少了它，火把塔會自己餵自己
        let mut torch = named(BlockKind::Torch, "minecraft:redstone_torch");
        torch.lit = true;

        assert_eq!(
            power_emitted_toward(&torch, Facing::Down),
            PowerOutput::INERT,
            "a torch must not power its own support block"
        );
        assert_eq!(
            power_emitted_toward(&torch, Facing::Up).block_power,
            BlockPower::Strong,
            "a torch strongly powers the block above it"
        );
        assert_eq!(
            power_emitted_toward(&torch, Facing::North).block_power,
            BlockPower::Weak,
            "and weakly powers its other neighbours"
        );
    }

    #[test]
    fn a_wall_torch_does_not_power_the_wall_it_hangs_on() {
        let mut torch = named(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
        torch.lit = true;
        torch.facing = Some(Facing::East); // 頭朝東，所以附著在西邊的方塊上

        assert_eq!(
            power_emitted_toward(&torch, Facing::West),
            PowerOutput::INERT,
            "a wall torch must not power the block it is attached to"
        );
        assert_eq!(
            power_emitted_toward(&torch, Facing::Up).block_power,
            BlockPower::Strong
        );
    }

    #[test]
    fn a_repeater_only_outputs_forward() {
        // Wiki: `facing` is "the direction from the output side to the input
        // side", so a repeater facing West outputs East, its opposite.
        let mut rep = named(BlockKind::Repeater, "minecraft:repeater");
        rep.lit = true;
        rep.facing = Some(Facing::West);

        assert_ne!(power_emitted_toward(&rep, Facing::East), PowerOutput::INERT);
        for other in [Facing::North, Facing::South, Facing::West, Facing::Up, Facing::Down] {
            assert_eq!(
                power_emitted_toward(&rep, other),
                PowerOutput::INERT,
                "a repeater must not output toward {other:?} -- it would become a splitter"
            );
        }
    }

    #[test]
    fn an_unlit_component_emits_nothing_in_every_direction() {
        let rep = named(BlockKind::Repeater, "minecraft:repeater");
        for d in [Facing::North, Facing::South, Facing::East, Facing::West, Facing::Up, Facing::Down] {
            assert_eq!(power_emitted_toward(&rep, d), PowerOutput::INERT);
        }
    }

    #[test]
    fn weighted_plates_emit_their_analog_strength() {
        let mut plate = named(
            BlockKind::WeightedPressurePlate,
            "minecraft:light_weighted_pressure_plate",
        );
        plate.power = 9;
        let out = power_emitted_by(&plate);
        assert!(out.drives_dust);
        assert_eq!(out.strength, 9);
        assert_eq!(out.block_power, BlockPower::None);
    }
}
