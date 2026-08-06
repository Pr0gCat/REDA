//! 端到端測試：建構世界 → 存檔 → 讀回 → 逐格比對。
//!
//! 這抓的是「palette 索引映射錯誤」和「位元打包慣例錯誤」——
//! 兩者都不會讓程式報錯，只會靜默讀出垃圾。

use std::collections::HashMap;

use reda::formats::litematic::{load, parse_block_name, save};
use reda::redstone::world::block::{BlockKind, BlockState, SlabHalf};
use reda::redstone::world::storage::World;

fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn empty_world_roundtrips() {
    let path = temp_path("reda_rt_empty.litematic");
    let world = World::new(4, 3, 2);

    save(&path, &world, "empty").expect("save must succeed");
    let loaded = load(&path).expect("load must succeed");

    assert_eq!(loaded.size(), (4, 3, 2));
    for y in 0..3 {
        for z in 0..2 {
            for x in 0..4 {
                assert_eq!(loaded.get(x, y, z).kind, BlockKind::Air);
            }
        }
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn redstone_circuit_roundtrips_with_all_properties_intact() {
    let path = temp_path("reda_rt_circuit.litematic");
    let mut world = World::new(8, 4, 8);

    // 一段紅石粉，強度遞減
    for x in 0..5 {
        world.set(x, 1, 0, parse_block_name("minecraft:stone", &props(&[])));
        let mut dust = parse_block_name(
            "minecraft:redstone_wire",
            &props(&[("power", &(15 - x).to_string())]),
        );
        dust.power = (15 - x) as u8;
        world.set(x, 2, 0, dust);
    }

    // 一個朝北、延遲 3、亮著的中繼器
    world.set(
        5,
        2,
        0,
        parse_block_name(
            "minecraft:repeater",
            &props(&[("facing", "north"), ("delay", "3"), ("powered", "true")]),
        ),
    );

    // 一個上半磚
    world.set(
        6,
        2,
        0,
        parse_block_name("minecraft:smooth_stone_slab", &props(&[("type", "top")])),
    );

    save(&path, &world, "circuit").expect("save must succeed");
    let loaded = load(&path).expect("load must succeed");

    assert_eq!(loaded.size(), (8, 4, 8));

    for x in 0..5 {
        let dust = loaded.get(x, 2, 0);
        assert_eq!(dust.kind, BlockKind::RedstoneWire, "dust at x={x}");
        assert_eq!(dust.power, (15 - x) as u8, "dust power at x={x}");
    }

    let rep = loaded.get(5, 2, 0);
    assert_eq!(rep.kind, BlockKind::Repeater);
    assert_eq!(rep.delay, 3);
    assert!(rep.lit);

    let slab = loaded.get(6, 2, 0);
    assert_eq!(slab.kind, BlockKind::Slab);
    assert_eq!(slab.half, Some(SlabHalf::Top));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn large_palette_forces_wider_bit_packing_and_still_roundtrips() {
    // 超過 16 種方塊 → bits_per_entry 從 4 變 5，逼出跨 long 的情況
    let path = temp_path("reda_rt_wide.litematic");
    let mut world = World::new(20, 1, 1);

    for x in 0..20 {
        let mut b = BlockState::air();
        b.kind = BlockKind::Other;
        b.name = format!("minecraft:test_block_{x}");
        world.set(x, 0, 0, b);
    }

    save(&path, &world, "wide").expect("save must succeed");
    let loaded = load(&path).expect("load must succeed");

    for x in 0..20 {
        assert_eq!(
            loaded.get(x, 0, 0).name,
            format!("minecraft:test_block_{x}"),
            "block at x={x}"
        );
    }

    let _ = std::fs::remove_file(&path);
}
