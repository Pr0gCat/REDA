//! Minecraft Java 1.20 的方塊屬性資料表。
//!
//! 這裡只有資料，判定邏輯在 `taxonomy.rs`。1.21 只需新增一個同構的檔案。
//!
//! **重要**：導電性與「是不是完整方塊」無關，必須查表。
//! 完整方塊但不導電的例子：玻璃、發光石、TNT、冰、紅石塊、觀察者、活塞。
//! 不是完整方塊但導電的例子：靈魂沙。
//! 渲染模型看起來是完整方塊、實際上連紅石粉都放不上去的例子：蜂蜜塊
//! （見 `SUPPORTS_NOTHING` 的說明）。

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
    "minecraft:farmland",
    "minecraft:dirt_path",
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

/// 完整立方體但不導電的方塊。
///
/// 它們可以承載紅石粉與元件、有完整側面，但無法傳遞紅石訊號。
/// 這份清單與 `NON_CONDUCTIVE` 的差別在於後者也包含農地、樹葉、堆肥桶
/// 這類「什麼都放不上去」的方塊 —— 支撐與導電是兩個獨立的問題，
/// 不能用同一份清單回答。
pub const FULL_SHAPE_NON_CONDUCTIVE: &[&str] = &[
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
];

/// 完整且導電的一般建材方塊。
///
/// `BlockKind::Other` 的方塊必須符合這裡的名單或後綴規則，才會被視為
/// 完整方塊（可承載一切）且導電。不符合的一律沒有任何能力 —— 未知方塊
/// **fail safe**，寧可誤判成不能用，也不要誤判成可用。
///
/// 註：把方塊正確分類成 `BlockKind::Solid` 是 `parse_block_name` 的責任。
/// 這份清單只是 `Other` 的安全網，不追求完整。
pub const CONDUCTIVE_FULL_BLOCKS: &[&str] = &[
    "minecraft:stone",
    "minecraft:smooth_stone",
    "minecraft:cobblestone",
    "minecraft:stone_bricks",
    "minecraft:deepslate",
    "minecraft:andesite",
    "minecraft:diorite",
    "minecraft:granite",
    "minecraft:dirt",
    "minecraft:grass_block",
    "minecraft:sand",
    "minecraft:gravel",
    "minecraft:obsidian",
    "minecraft:bedrock",
    "minecraft:netherrack",
    "minecraft:end_stone",
    "minecraft:quartz_block",
    "minecraft:purpur_block",
    "minecraft:hay_block",
    "minecraft:bone_block",
];

/// 常見的完整建材方塊後綴。多數染色變體只有前綴不同，逐一列出不切實際。
///
/// **這些後綴只在方塊未出現於 `NON_CONDUCTIVE`、`CONDUCTIVE_EXCEPTIONS`
/// 與 `SUPPORTS_NOTHING` 時才適用** —— 那幾份清單先查，所以
/// `redstone_block`、`slime_block` 不會被 `_block` 這條後綴誤判成一般完整
/// 方塊，`honey_block` 更是連「完整方塊」的資格都在 `SUPPORTS_NOTHING`
/// 那一關就被擋下，根本不會走到這條後綴規則。
pub const FULL_BLOCK_SUFFIXES: &[&str] = &[
    "_wool",
    "_concrete",
    "_planks",
    "_terracotta",
    "_bricks",
    "_log",
    "_wood",
    "_block",
];

/// 完全不能承載任何東西的方塊。
///
/// `minecraft:honey_block` 是這裡最違反直覺的一筆：它的渲染模型是完整
/// 16x16x16 立方體（跟玻璃一樣），視覺上像個完整方塊，但遊戲把它排除在
/// 「頂面完整實心方形面」之外 —— 紅石粉、拉桿、按鈕都放不上去。這與它
/// 不導電是**兩件獨立的事**：不導電只表示訊號傳不過去，這裡講的是連
/// 「站得上去」都不行。誤把它丟進 `NON_CONDUCTIVE`（如同一份完整方塊只是
/// 不導電的清單）曾經讓 `_block` 後綴規則把它判成完整立方體，因而誤判紅石粉
/// 爬得上去 —— 見 `conformance/results/1.20.1.json` 的 `conducts_honey_block`
/// 探針。
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
    "minecraft:honey_block",
];
