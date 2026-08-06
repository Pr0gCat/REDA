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
    Button,
    PressurePlate,
    WeightedPressurePlate,
    Observer,
    Target,
    DaylightDetector,
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

impl Facing {
    /// 相反方向。
    pub fn opposite(self) -> Facing {
        match self {
            Facing::North => Facing::South,
            Facing::South => Facing::North,
            Facing::East => Facing::West,
            Facing::West => Facing::East,
            Facing::Up => Facing::Down,
            Facing::Down => Facing::Up,
        }
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// 讀檔時沒有被讀進上面那些結構化欄位的 blockstate property。
    ///
    /// 原樣保存、原樣寫回，所以 round-trip 不會遺失任何屬性 —— 我們不需要
    /// 為每一種方塊建模它的每一個屬性。用 `BTreeMap` 是為了寫出的順序確定。
    pub extra_properties: std::collections::BTreeMap<String, String>,
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
            extra_properties: std::collections::BTreeMap::new(),
        }
    }
}

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
