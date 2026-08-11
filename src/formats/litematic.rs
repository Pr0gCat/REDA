//! `.litematic` 格式的讀寫。
//!
//! 結構是 gzip 過的 NBT，根節點包含 `MinecraftDataVersion`、`Version`、
//! `SubVersion`、`Metadata`、`Regions`。每個 region 有 `Position`、`Size`
//! （**可以是負的**）、`BlockStatePalette`、`BlockStates`（LongArray）、
//! `TileEntities`、`Entities`。
//!
//! 寫出版本是 **7 / sub-version 1**（Litematica 26.2）；讀取時也接受舊的
//! 6。兩者的方塊資料編碼相同，主要差別在較新版本的 metadata / block
//! entity 表達；REDA 目前不寫 block entity。
//!
//! **沒有官方規格** —— 這個實作依據的是 Litematica 原始碼與社群逆向文件。

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::formats::bitpack::{bits_per_entry, pack, required_longs, unpack};
use crate::formats::nbt::{read_gzip_nbt, write_gzip_nbt, FormatError};
use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing, SlabHalf};
use crate::redstone::world::palette::Palette;
use crate::redstone::world::storage::World;

/// Schema written by the installed Litematica 26.2 release.
///
/// Litematica 26.2 declares schema version 7, sub-version 1.  REDA's target
/// is Minecraft 26.2, so emitting the older 1.20.1 schema would mislabel the
/// download even though this project's palette contains no block entities.
pub const SCHEMATIC_VERSION: i32 = 7;
pub const SCHEMATIC_SUB_VERSION: i32 = 1;

/// 讀取時接受的最高版本；26.2 Litematica writes 7.
pub const MAX_SUPPORTED_VERSION: i32 = 7;

/// 讀取時接受的最低版本。
pub const MIN_SUPPORTED_VERSION: i32 = 6;

/// Minecraft Java 26.2's data version, confirmed from the target server's
/// `version` command (`data = 4903`).
pub const DATA_VERSION_26_2: i32 = 4903;

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
    /// NBT preserves map insertion order, so use a sorted map for stable
    /// external bytes regardless of `HashMap`'s randomized iteration order.
    pub properties: BTreeMap<String, String>,
}

/// 這個方塊種類的哪些 blockstate property 會被讀進 `BlockState` 的結構化欄位。
///
/// 沒列在這裡的屬性一律原樣保存在 `extra_properties` 並原樣寫回，所以新增
/// 一種方塊不需要修改讀寫邏輯，也不會靜默遺失資料。
fn structured_property_keys(kind: BlockKind) -> &'static [&'static str] {
    match kind {
        BlockKind::RedstoneWire => &["power"],
        BlockKind::Repeater => &["facing", "delay", "powered"],
        BlockKind::Comparator => &["facing", "powered"],
        BlockKind::WallTorch => &["facing", "lit"],
        BlockKind::Torch | BlockKind::Lamp => &["lit"],
        BlockKind::Lever => &["face", "facing", "powered"],
        BlockKind::Piston => &["facing"],
        BlockKind::Slab => &["type"],
        BlockKind::Button => &["face", "powered"],
        BlockKind::PressurePlate => &["powered"],
        BlockKind::WeightedPressurePlate => &["power"],
        BlockKind::Observer => &["facing", "powered"],
        BlockKind::Target | BlockKind::DaylightDetector => &["power"],
        _ => &[],
    }
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
        "minecraft:glass"
        | "minecraft:tinted_glass"
        | "minecraft:glowstone"
        | "minecraft:sea_lantern"
        | "minecraft:tnt"
        | "minecraft:ice" => BlockKind::Glass,
        n if n.ends_with("_slab") => BlockKind::Slab,
        "minecraft:stone"
        | "minecraft:smooth_stone"
        | "minecraft:cobblestone"
        | "minecraft:dirt"
        | "minecraft:oak_planks" => BlockKind::Solid,
        "minecraft:observer" => BlockKind::Observer,
        "minecraft:target" => BlockKind::Target,
        "minecraft:daylight_detector" => BlockKind::DaylightDetector,
        n if n.ends_with("_button") => BlockKind::Button,
        n if n.ends_with("_weighted_pressure_plate") => BlockKind::WeightedPressurePlate,
        n if n.ends_with("_pressure_plate") => BlockKind::PressurePlate,
        _ => BlockKind::Other,
    };

    let structured = structured_property_keys(kind);
    let reads = |key: &str| -> Option<&String> {
        if structured.contains(&key) {
            properties.get(key)
        } else {
            None
        }
    };

    let facing = reads("facing").and_then(|f| match f.as_str() {
        "north" => Some(Facing::North),
        "south" => Some(Facing::South),
        "east" => Some(Facing::East),
        "west" => Some(Facing::West),
        "up" => Some(Facing::Up),
        "down" => Some(Facing::Down),
        _ => None,
    });

    let face = reads("face").and_then(|f| match f.as_str() {
        "floor" => Some(Face::Floor),
        "wall" => Some(Face::Wall),
        "ceiling" => Some(Face::Ceiling),
        _ => None,
    });

    let half = reads("type").and_then(|t| match t.as_str() {
        "top" => Some(SlabHalf::Top),
        "bottom" => Some(SlabHalf::Bottom),
        "double" => Some(SlabHalf::Double),
        _ => None,
    });

    let power = reads("power")
        .and_then(|p| p.parse::<u8>().ok())
        .unwrap_or(0);

    let delay = reads("delay")
        .and_then(|d| d.parse::<u8>().ok())
        .unwrap_or(0);

    // 註：`lit` 找不到時退回 `powered`，對中繼器與比較器是正確的
    // —— 它們只有 `powered`。但 1.21 的銅燈兩個屬性都有而且意義不同
    // （`lit` 是鎖存的輸出狀態，`powered` 是輸入訊號），加入 1.21 支援時
    // 這個退回必須拆開處理。
    let lit = reads("lit")
        .or_else(|| reads("powered"))
        .map(|v| v == "true")
        .unwrap_or(false);

    // 其餘屬性原樣保存，寫回時無損
    let extra_properties = properties
        .iter()
        .filter(|(k, _)| !structured.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    BlockState {
        kind,
        facing,
        face,
        power,
        delay,
        lit,
        half,
        name: name.to_string(),
        extra_properties,
    }
}

/// 讀取一個 `.litematic` 檔案，回傳第一個 region 的世界。
///
/// 多 region 的檔案目前只取第一個 —— 我們自己產生的檔案永遠是單 region，
/// 而讀取社群結構時多 region 很罕見。
pub fn load(path: &Path) -> Result<World, FormatError> {
    let file: LitematicFile = read_gzip_nbt(path)?;

    if file.version < MIN_SUPPORTED_VERSION || file.version > MAX_SUPPORTED_VERSION {
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
        let properties: HashMap<String, String> = entry
            .properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let state = parse_block_name(&entry.name, &properties);
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

/// 把我們的 `BlockState` 轉回 litematic 的 palette 項目。
///
/// 只寫出該方塊真正擁有的 property —— 多寫會讓 Minecraft 拒絕載入。
pub fn block_state_to_entry(state: &BlockState) -> PaletteEntry {
    // 先放回沒建模的屬性，再讓結構化欄位覆寫 —— 兩者的 key 依定義不重疊。
    let mut properties: BTreeMap<String, String> = state
        .extra_properties
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let structured = structured_property_keys(state.kind);

    if structured.contains(&"facing") {
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
    }

    if structured.contains(&"face") {
        if let Some(f) = state.face {
            let s = match f {
                Face::Floor => "floor",
                Face::Wall => "wall",
                Face::Ceiling => "ceiling",
            };
            properties.insert("face".to_string(), s.to_string());
        }
    }

    if structured.contains(&"type") {
        if let Some(h) = state.half {
            let s = match h {
                SlabHalf::Top => "top",
                SlabHalf::Bottom => "bottom",
                SlabHalf::Double => "double",
            };
            properties.insert("type".to_string(), s.to_string());
        }
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
        BlockKind::Button | BlockKind::PressurePlate | BlockKind::Observer => {
            properties.insert("powered".to_string(), state.lit.to_string());
        }
        BlockKind::WeightedPressurePlate => {
            properties.insert("power".to_string(), state.power.to_string());
        }
        BlockKind::Target | BlockKind::DaylightDetector => {
            properties.insert("power".to_string(), state.power.to_string());
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

    let mut regions = BTreeMap::new();
    regions.insert(name.to_string(), region);

    let file = LitematicFile {
        minecraft_data_version: DATA_VERSION_26_2,
        version: SCHEMATIC_VERSION,
        sub_version: SCHEMATIC_SUB_VERSION,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::world::block::{BlockKind, Face, Facing, SlabHalf};

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn saving_a_property_rich_world_is_byte_deterministic() {
        let mut world = World::new(2, 1, 1);
        world.set(
            0,
            0,
            0,
            parse_block_name(
                "minecraft:redstone_wall_torch",
                &props(&[("facing", "north"), ("lit", "true")]),
            ),
        );
        world.set(
            1,
            0,
            0,
            parse_block_name(
                "minecraft:repeater",
                &props(&[("facing", "east"), ("delay", "2"), ("powered", "false")]),
            ),
        );

        let directory =
            std::env::temp_dir().join(format!("reda-litematic-determinism-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create isolated output directory");
        let mut outputs = Vec::new();
        for index in 0..16 {
            let path = directory.join(format!("{index}.litematic"));
            save(&path, &world, "property_order").expect("save must succeed");
            outputs.push(std::fs::read(path).expect("read saved litematic"));
        }
        std::fs::remove_dir_all(&directory).expect("remove isolated output directory");

        assert!(
            outputs.windows(2).all(|pair| pair[0] == pair[1]),
            "the NBT property map must have one stable serialization order"
        );
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
        let top = parse_block_name("minecraft:smooth_stone_slab", &props(&[("type", "top")]));
        assert_eq!(top.half, Some(SlabHalf::Top));

        let double = parse_block_name("minecraft:smooth_stone_slab", &props(&[("type", "double")]));
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
    fn block_state_to_entry_preserves_properties() {
        let b = parse_block_name(
            "minecraft:repeater",
            &props(&[("facing", "north"), ("delay", "3"), ("powered", "true")]),
        );
        let entry = block_state_to_entry(&b);
        assert_eq!(entry.name, "minecraft:repeater");
        assert_eq!(
            entry.properties.get("facing").map(String::as_str),
            Some("north")
        );
        assert_eq!(entry.properties.get("delay").map(String::as_str), Some("3"));
        assert_eq!(
            entry.properties.get("powered").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn block_state_to_entry_emits_no_properties_for_plain_blocks() {
        let b = parse_block_name("minecraft:stone", &props(&[]));
        let entry = block_state_to_entry(&b);
        assert!(entry.properties.is_empty());
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

    #[test]
    fn facing_is_not_emitted_for_blocks_that_have_no_such_property() {
        // 之後的佈局程式碼可能複製模板時誤設 facing；輸出必須過濾掉，
        // 否則 Minecraft 會拒絕載入整個檔案
        let mut stone = parse_block_name("minecraft:stone", &props(&[]));
        stone.facing = Some(Facing::North);

        let entry = block_state_to_entry(&stone);
        assert!(
            !entry.properties.contains_key("facing"),
            "a plain solid block must not carry a facing property"
        );
    }

    #[test]
    fn slab_half_is_not_emitted_for_non_slabs() {
        let mut stone = parse_block_name("minecraft:stone", &props(&[]));
        stone.half = Some(SlabHalf::Top);

        let entry = block_state_to_entry(&stone);
        assert!(
            !entry.properties.contains_key("type"),
            "a non-slab must not carry a type property"
        );
    }

    #[test]
    fn facing_is_still_emitted_for_blocks_that_do_have_it() {
        let rep = parse_block_name(
            "minecraft:repeater",
            &props(&[("facing", "east"), ("delay", "2"), ("powered", "false")]),
        );
        let entry = block_state_to_entry(&rep);
        assert_eq!(
            entry.properties.get("facing").map(String::as_str),
            Some("east"),
            "a repeater must keep its facing"
        );
    }

    #[test]
    fn observer_facing_survives_the_round_trip() {
        // observer 是自己的 BlockKind::Observer，facing 是結構化欄位，
        // 走的是結構化路徑而不是 extra_properties
        let b = parse_block_name("minecraft:observer", &props(&[("facing", "north")]));
        let entry = block_state_to_entry(&b);
        assert_eq!(
            entry.properties.get("facing").map(String::as_str),
            Some("north"),
            "an observer's facing must not be lost"
        );
    }

    #[test]
    fn comparator_mode_survives_the_round_trip() {
        // mode 決定比較器是比較還是減法 —— 弄丟它等於把元件換成另一個
        let b = parse_block_name(
            "minecraft:comparator",
            &props(&[
                ("mode", "subtract"),
                ("powered", "true"),
                ("facing", "north"),
            ]),
        );
        let entry = block_state_to_entry(&b);
        assert_eq!(
            entry.properties.get("mode").map(String::as_str),
            Some("subtract"),
            "a subtract comparator must not become a compare comparator"
        );
        assert_eq!(
            entry.properties.get("facing").map(String::as_str),
            Some("north")
        );
        assert_eq!(
            entry.properties.get("powered").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn lever_face_and_repeater_locked_survive() {
        let lever = parse_block_name(
            "minecraft:lever",
            &props(&[("face", "floor"), ("facing", "north"), ("powered", "false")]),
        );
        let entry = block_state_to_entry(&lever);
        assert_eq!(
            entry.properties.get("face").map(String::as_str),
            Some("floor"),
            "a floor lever must not become a wall lever"
        );

        let rep = parse_block_name(
            "minecraft:repeater",
            &props(&[
                ("locked", "true"),
                ("delay", "1"),
                ("powered", "false"),
                ("facing", "east"),
            ]),
        );
        let rep_entry = block_state_to_entry(&rep);
        assert_eq!(
            rep_entry.properties.get("locked").map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn lever_face_is_read_into_the_structured_field_not_extra_properties() {
        // `face` must take the typed path so callers (e.g. compile()) can
        // query `state.face` instead of poking at a string bag.
        let lever = parse_block_name(
            "minecraft:lever",
            &props(&[
                ("face", "ceiling"),
                ("facing", "west"),
                ("powered", "false"),
            ]),
        );
        assert_eq!(lever.face, Some(Face::Ceiling));
        assert!(
            !lever.extra_properties.contains_key("face"),
            "face must be a structured field, not stashed in extra_properties"
        );
    }

    #[test]
    fn button_face_survives_the_round_trip() {
        let button = parse_block_name(
            "minecraft:oak_button",
            &props(&[("face", "wall"), ("facing", "north"), ("powered", "true")]),
        );
        assert_eq!(button.face, Some(Face::Wall));
        let entry = block_state_to_entry(&button);
        assert_eq!(
            entry.properties.get("face").map(String::as_str),
            Some("wall"),
            "a wall button must not become a floor button"
        );
    }

    #[test]
    fn face_is_not_emitted_for_blocks_that_have_no_such_property() {
        let mut stone = parse_block_name("minecraft:stone", &props(&[]));
        stone.face = Some(Face::Floor);

        let entry = block_state_to_entry(&stone);
        assert!(
            !entry.properties.contains_key("face"),
            "a plain solid block must not carry a face property"
        );
    }

    #[test]
    fn plain_blocks_gain_no_properties_they_did_not_have() {
        let stone = parse_block_name("minecraft:stone", &props(&[]));
        assert!(block_state_to_entry(&stone).properties.is_empty());
    }

    #[test]
    fn every_input_property_comes_back_out() {
        // 通用不變式:讀進來的 key 集合必須等於寫出去的 key 集合
        let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
            (
                "minecraft:observer",
                vec![("facing", "up"), ("powered", "false")],
            ),
            (
                "minecraft:oak_stairs",
                vec![
                    ("facing", "east"),
                    ("half", "top"),
                    ("shape", "straight"),
                    ("waterlogged", "false"),
                ],
            ),
            (
                "minecraft:redstone_wire",
                vec![("power", "9"), ("north", "side"), ("east", "up")],
            ),
            (
                "minecraft:hopper",
                vec![("facing", "down"), ("enabled", "true")],
            ),
            (
                "minecraft:lever",
                vec![("face", "wall"), ("facing", "south"), ("powered", "true")],
            ),
            (
                "minecraft:stone_button",
                vec![
                    ("face", "ceiling"),
                    ("facing", "east"),
                    ("powered", "false"),
                ],
            ),
        ];

        for (name, pairs) in cases {
            let input = props(&pairs);
            let state = parse_block_name(name, &input);
            let output = block_state_to_entry(&state).properties;

            let mut in_keys: Vec<&str> = input.keys().map(String::as_str).collect();
            let mut out_keys: Vec<&str> = output.keys().map(String::as_str).collect();
            in_keys.sort_unstable();
            out_keys.sort_unstable();
            assert_eq!(in_keys, out_keys, "key set changed for {name}");

            for (k, v) in &input {
                assert_eq!(output.get(k), Some(v), "value changed for {name}.{k}");
            }
        }
    }

    #[test]
    fn input_component_names_are_recognised() {
        assert_eq!(
            parse_block_name("minecraft:stone_button", &props(&[])).kind,
            BlockKind::Button
        );
        assert_eq!(
            parse_block_name("minecraft:oak_pressure_plate", &props(&[])).kind,
            BlockKind::PressurePlate
        );
        assert_eq!(
            parse_block_name("minecraft:observer", &props(&[])).kind,
            BlockKind::Observer
        );
    }

    #[test]
    fn weighted_pressure_plates_keep_their_analog_power() {
        let plate = parse_block_name(
            "minecraft:light_weighted_pressure_plate",
            &props(&[("power", "7")]),
        );
        assert_eq!(plate.kind, BlockKind::WeightedPressurePlate);
        assert_eq!(
            plate.power, 7,
            "analog power must reach the structured field"
        );

        let entry = block_state_to_entry(&plate);
        assert_eq!(entry.properties.get("power").map(String::as_str), Some("7"));
        assert!(
            !entry.properties.contains_key("powered"),
            "a weighted plate has no `powered` property; emitting one makes Minecraft reject the file"
        );
    }

    #[test]
    fn ordinary_pressure_plates_still_use_powered() {
        let plate = parse_block_name(
            "minecraft:stone_pressure_plate",
            &props(&[("powered", "true")]),
        );
        assert_eq!(plate.kind, BlockKind::PressurePlate);
        let entry = block_state_to_entry(&plate);
        assert_eq!(
            entry.properties.get("powered").map(String::as_str),
            Some("true")
        );
        assert!(!entry.properties.contains_key("power"));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn written_version_is_loadable_by_the_target_minecraft_version() {
        // The installed 26.2 Litematica jar declares schema version 7,
        // sub-version 1.  Its target Minecraft data version is the live
        // server's `version` response: 4903.
        assert_eq!(
            SCHEMATIC_VERSION, 7,
            "26.2 Litematica writes schema version 7"
        );
        assert_eq!(
            DATA_VERSION_26_2, 4903,
            "26.2's live server reports data version 4903"
        );
        assert!(
            MAX_SUPPORTED_VERSION >= SCHEMATIC_VERSION,
            "we must be able to read back what we write"
        );
    }
}
