//! Minecraft Java 1.20 的方塊屬性資料表。
//!
//! 這裡只有資料，判定邏輯在 `taxonomy.rs`。1.21 只需新增一個同構的檔案。
//!
//! **重要**：導電性與「是不是完整方塊」無關，必須查表。
//! 完整方塊但不導電的例子：玻璃、發光石、TNT、冰、紅石塊、觀察者、活塞。
//! 不是完整方塊但導電的例子：靈魂沙。

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
    "minecraft:oak_leaves",
    "minecraft:farmland",
    "minecraft:dirt_path",
    "minecraft:honey_block",
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

/// 完全不能承載任何東西的方塊。
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
];
