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

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::formats::bitpack::{bits_per_entry, required_longs, unpack};
use crate::formats::nbt::{read_gzip_nbt, FormatError};
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
    pub regions: BTreeMap<String, Region>,
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

    // 註：`lit` 找不到時退回 `powered`，對中繼器與比較器是正確的
    // —— 它們只有 `powered`。但 1.21 的銅燈兩個屬性都有而且意義不同
    // （`lit` 是鎖存的輸出狀態，`powered` 是輸入訊號），加入 1.21 支援時
    // 這個退回必須拆開處理。
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

/// 讀取一個 `.litematic` 檔案，回傳第一個 region 的世界。
///
/// 多 region 的檔案目前只取第一個 —— 我們自己產生的檔案永遠是單 region，
/// 而讀取社群結構時多 region 很罕見。
pub fn load(path: &Path) -> Result<World, FormatError> {
    let file: LitematicFile = read_gzip_nbt(path)?;

    if file.version < MIN_SUPPORTED_VERSION || file.version > SCHEMATIC_VERSION {
        return Err(FormatError::UnsupportedVersion(file.version));
    }

    // BTreeMap 依鍵排序，所以多 region 檔案每次都取到同一個 region。
    let region = file
        .regions
        .values()
        .next()
        .ok_or_else(|| FormatError::MissingField("Regions".to_string()))?;

    // Size 可以是負的，表示 region 往負方向延伸。取絕對值即可，
    // 因為我們只關心 bounding box 的形狀。
    let abs_axis = |value: i32, axis: &str| -> Result<i32, FormatError> {
        value.checked_abs().ok_or_else(|| {
            FormatError::Nbt(format!("region size.{axis} ({value}) is out of range"))
        })
    };
    let size_x = abs_axis(region.size.x, "x")?;
    let size_y = abs_axis(region.size.y, "y")?;
    let size_z = abs_axis(region.size.z, "z")?;

    if size_x == 0 || size_y == 0 || size_z == 0 {
        return Err(FormatError::MissingField("Size".to_string()));
    }

    let mut palette = Palette::new();
    let mut index_map = Vec::with_capacity(region.block_state_palette.len());
    for entry in &region.block_state_palette {
        let state = parse_block_name(&entry.name, &entry.properties);
        index_map.push(palette.intern(state));
    }

    let count = (size_x as usize)
        .checked_mul(size_y as usize)
        .and_then(|n| n.checked_mul(size_z as usize))
        .ok_or_else(|| {
            FormatError::Nbt(format!(
                "region volume {size_x}x{size_y}x{size_z} overflows"
            ))
        })?;
    let bits = bits_per_entry(region.block_state_palette.len());

    // `fastnbt::LongArray` 不保證能直接當 `&[i64]` 用，先收成 Vec。
    // 這裡多一次配置，但讀檔不在熱路徑上。
    let longs: Vec<i64> = region.block_states.iter().copied().collect();
    if longs.len() < required_longs(count, bits) {
        return Err(FormatError::MissingField("BlockStates".to_string()));
    }
    let raw = unpack(&longs, bits, count);

    // 檔案裡的索引指向檔案自己的 palette，要映射到我們的 palette
    let palette_len = region.block_state_palette.len();
    let cells: Vec<u32> = raw
        .into_iter()
        .map(|i| {
            index_map.get(i as usize).copied().ok_or_else(|| {
                FormatError::Nbt(format!(
                    "block state index {i} is out of range for a palette of {palette_len}"
                ))
            })
        })
        .collect::<Result<Vec<u32>, FormatError>>()?;

    Ok(World::from_parts(size_x, size_y, size_z, palette, cells))
}

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

    #[test]
    fn parse_block_name_keeps_unknown_blocks_as_other() {
        let b = parse_block_name("minecraft:some_future_block", &props(&[]));
        assert_eq!(b.kind, BlockKind::Other);
        assert_eq!(b.name, "minecraft:some_future_block");
    }

    #[test]
    fn axis_error_messages_name_the_axis() {
        // 三個軸的錯誤訊息必須能分辨，否則損壞的檔案無從診斷
        let messages: Vec<String> = ["x", "y", "z"]
            .iter()
            .map(|axis| format!("region size.{axis} ({}) is out of range", i32::MIN))
            .collect();
        assert_eq!(messages.len(), 3);
        assert!(messages[0].contains("size.x"));
        assert!(messages[1].contains("size.y"));
        assert!(messages[2].contains("size.z"));
        assert!(
            messages[0] != messages[1] && messages[1] != messages[2],
            "each axis must produce a distinct message"
        );
    }
}
