//! 驗收測試：把真的邏輯閘用紅石搭出來，對照它的完整真值表。
//!
//! 前面所有測試都只驗證單一元件（火把、中繼器、紅石粉…）各自的行為。
//! 這份測試證明它們組合起來能動 —— 這正是模擬器存在的理由。
//!
//! 每個電路都刻意在無關的元件之間留至少兩格空隙，避免火把或支撐塊意外
//! 碰到不該碰的方塊，製造出電路本身沒有設計的訊號路徑。
//!
//! `run_until_stable` 每一輪都會先從目前的世界狀態重新安定下來（含無
//! 條件重算紅石粉強度），所以 `set_lever` 改了拉桿之後直接呼叫它就夠，
//! 不需要另外手動 `step()` 去強制重算。

use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Facing};
use reda::redstone::world::storage::World;

fn stone() -> BlockState {
    let mut b = BlockState::air();
    b.kind = BlockKind::Solid;
    b.name = "minecraft:stone".to_string();
    b
}

fn dust() -> BlockState {
    let mut b = BlockState::air();
    b.kind = BlockKind::RedstoneWire;
    b.name = "minecraft:redstone_wire".to_string();
    b
}

fn lever(on: bool) -> BlockState {
    let mut b = BlockState::air();
    b.kind = BlockKind::Lever;
    b.name = "minecraft:lever".to_string();
    b.lit = on;
    b
}

/// 立式火把，一開始假定沒有被充能（自洽的初始狀態：亮）。
fn standing_torch() -> BlockState {
    let mut b = BlockState::air();
    b.kind = BlockKind::Torch;
    b.name = "minecraft:redstone_torch".to_string();
    b.lit = true;
    b
}

/// 牆上火把，附著方向由呼叫端決定；同樣假定初始未充能（亮）。
fn wall_torch(facing: Facing) -> BlockState {
    let mut b = BlockState::air();
    b.kind = BlockKind::WallTorch;
    b.name = "minecraft:redstone_wall_torch".to_string();
    b.facing = Some(facing);
    b.lit = true;
    b
}

const MAX_TICKS: u64 = 200;

/// 改變一顆拉桿的狀態，然後讓電路重新穩定下來。
fn set_lever(simulator: &mut Simulator, position: Position, on: bool) {
    simulator
        .world_mut()
        .set(position.x, position.y, position.z, lever(on));
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle after changing an input");
}

// ---------------------------------------------------------------------
// NOT：拉桿 -> 支撐塊 -> 立式火把。輸入充能支撐塊，火把反相。
//
//   L(lever) -- B(stone) -- T(torch, on top of B)
//
// L 在 B 西側，T 立在 B 上方。
// ---------------------------------------------------------------------
#[test]
fn a_torch_inverts() {
    let lever_pos = Position::new(1, 1, 5);
    let block_pos = Position::new(2, 1, 5);
    let torch_pos = Position::new(2, 2, 5);

    let mut world = World::new(6, 4, 10);
    world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));
    world.set(block_pos.x, block_pos.y, block_pos.z, stone());
    world.set(torch_pos.x, torch_pos.y, torch_pos.z, standing_torch());

    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle");

    // 輸入 0 -> 輸出 1
    assert!(
        simulator.world().get(torch_pos.x, torch_pos.y, torch_pos.z).lit,
        "lever off -> support unpowered -> torch lit (output 1)"
    );

    set_lever(&mut simulator, lever_pos, true);

    // 輸入 1 -> 輸出 0
    assert!(
        !simulator.world().get(torch_pos.x, torch_pos.y, torch_pos.z).lit,
        "lever on -> support powered -> torch off (output 0)"
    );
}

// ---------------------------------------------------------------------
// 共用的「多輸入 NOR」搭建器。
//
// 中心方塊 S 的四個水平鄰格最多用到：西、東、南各放一路輸入（拉桿驅動
// 支撐塊、支撐塊上方的紅石粉匯流到 S 正上方的粉），北側放輸出用的牆上
// 火把。所有輸入的粉都疊在 y+1 層，只跟 S 正上方那格粉水平相鄰，互相
// 之間、以及跟輸出火把之間都留了至少一格的間隔，不會意外碰到彼此。
//
//   俯視圖（y = 支撐塊那層）：
//
//         .   T(北, 輸出火把)   .
//     LA - SA   S   SB - LB
//         .   SC(南)   .
//                |
//               LC
//
//   （SA/SB/SC 是輸入的支撐塊，S 是中心方塊；LA/LB/LC 是拉桿，
//    各自跟自己的支撐塊面對面相鄰，離 S 有一格緩衝。）
//
//   側視圖（沿其中一路輸入）：
//
//     y+1:      DA -- D          （D 疊在 S 正上方，DA 疊在 SA 正上方）
//     y+0:  LA  SA    S
// ---------------------------------------------------------------------
struct NorGate {
    levers: Vec<Position>,
    output_torch: Position,
}

fn build_nor_gate(world: &mut World, center: Position, input_count: usize) -> NorGate {
    let directions = [Facing::West, Facing::East, Facing::South];
    assert!(input_count <= directions.len(), "only 3 free sides available for inputs");

    world.set(center.x, center.y, center.z, stone());

    let merge_dust = center.up();
    world.set(merge_dust.x, merge_dust.y, merge_dust.z, dust());

    let mut levers = Vec::new();
    for &direction in directions.iter().take(input_count) {
        let support = center.offset(direction);
        let support_dust = support.up();
        let lever_pos = support.offset(direction); // 再往外一格，跟 S 保持距離

        world.set(support.x, support.y, support.z, stone());
        world.set(support_dust.x, support_dust.y, support_dust.z, dust());
        world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

        levers.push(lever_pos);
    }

    let output_torch = center.offset(Facing::North);
    world.set(
        output_torch.x,
        output_torch.y,
        output_torch.z,
        wall_torch(Facing::North),
    );

    NorGate { levers, output_torch }
}

fn read_torch(simulator: &Simulator, position: Position) -> bool {
    simulator.world().get(position.x, position.y, position.z).lit
}

#[test]
fn a_two_input_nor_gate_matches_its_truth_table() {
    let mut world = World::new(12, 4, 12);
    let center = Position::new(6, 1, 6);
    let gate = build_nor_gate(&mut world, center, 2);
    let lever_a = gate.levers[0];
    let lever_b = gate.levers[1];

    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle");

    // 四列全測：00->1, 01->0, 10->0, 11->0
    let rows: [(bool, bool, bool); 4] = [
        (false, false, true),
        (false, true, false),
        (true, false, false),
        (true, true, false),
    ];

    for (a, b, expected) in rows {
        set_lever(&mut simulator, lever_a, a);
        set_lever(&mut simulator, lever_b, b);
        let output = read_torch(&simulator, gate.output_torch);
        assert_eq!(
            output, expected,
            "NOR({a}, {b}) should be {expected}, got {output}"
        );
    }
}

#[test]
fn a_three_input_nor_gate_matches_its_truth_table() {
    let mut world = World::new(12, 4, 12);
    let center = Position::new(6, 1, 6);
    let gate = build_nor_gate(&mut world, center, 3);
    let lever_a = gate.levers[0];
    let lever_b = gate.levers[1];
    let lever_c = gate.levers[2];

    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle");

    // 八列全測 -- 紅石的 NOR 扇入不影響延遲，這驗證多輸入真的可行
    for a in [false, true] {
        for b in [false, true] {
            for c in [false, true] {
                set_lever(&mut simulator, lever_a, a);
                set_lever(&mut simulator, lever_b, b);
                set_lever(&mut simulator, lever_c, c);
                let expected = !(a || b || c);
                let output = read_torch(&simulator, gate.output_torch);
                assert_eq!(
                    output, expected,
                    "NOR({a}, {b}, {c}) should be {expected}, got {output}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------
// AND = NOR(NOT A, NOT B)。三個火把：TA、TB 各自反相一個輸入，
// 兩者的輸出直接匯流到最終 NOR 級的合併點，TF 是最終輸出。
//
// TA、TB 是立式火把，跟合併點 D 同一層（y+1），水平相鄰即可直接驅動 D
// （火把側向弱充能紅石粉一樣算「驅動」）。TF 是牆上火把，貼在中心方塊
// S 北側，跟 TA/TB 之間至少差了一格對角線，不會意外相鄰。
//
//   俯視圖（y = 火把/合併粉那層）：
//
//        TF(北側，牆上火把，貼在 S 上，跟這層無關)
//     TA(4,y+1)  D(5,y+1)  TB(6,y+1)
//
//   側視圖：
//
//     y+2:  TA        TB      （立式火把，立在 BA/BB 上）
//     y+1:  BA   S    BB      （BA/BB 是 NOT 閘的支撐塊；S 是最終 NOR 的中心塊）
//                |
//              TF(牆上，貼 S 北側，同 y+1 高度)
//     y+0:  LA        LB      （拉桿，離 BA/BB 一格）
// ---------------------------------------------------------------------
#[test]
fn an_and_gate_built_from_nor_matches_its_truth_table() {
    let mut world = World::new(12, 5, 12);

    let support_a = Position::new(4, 1, 6);
    let lever_a = Position::new(3, 1, 6);
    let torch_a = support_a.up();

    let support_b = Position::new(6, 1, 6);
    let lever_b = Position::new(7, 1, 6);
    let torch_b = support_b.up();

    let center = Position::new(5, 1, 6);
    let merge_dust = center.up();
    let output_torch = center.offset(Facing::North);

    world.set(support_a.x, support_a.y, support_a.z, stone());
    world.set(lever_a.x, lever_a.y, lever_a.z, lever(false));
    world.set(torch_a.x, torch_a.y, torch_a.z, standing_torch());

    world.set(support_b.x, support_b.y, support_b.z, stone());
    world.set(lever_b.x, lever_b.y, lever_b.z, lever(false));
    world.set(torch_b.x, torch_b.y, torch_b.z, standing_torch());

    world.set(center.x, center.y, center.z, stone());
    world.set(merge_dust.x, merge_dust.y, merge_dust.z, dust());
    world.set(
        output_torch.x,
        output_torch.y,
        output_torch.z,
        wall_torch(Facing::North),
    );

    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle");

    // 四列全測：00->0, 01->0, 10->0, 11->1
    let rows: [(bool, bool, bool); 4] = [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ];

    for (a, b, expected) in rows {
        set_lever(&mut simulator, lever_a, a);
        set_lever(&mut simulator, lever_b, b);
        let output = read_torch(&simulator, output_torch);
        assert_eq!(
            output, expected,
            "AND({a}, {b}) should be {expected}, got {output}"
        );
    }
}

// ---------------------------------------------------------------------
// OR：兩條粉線匯流成一個 NOR 級（T1 = NOR(A, B)），再用第二個火把把
// 極性反轉回來（T2 = NOT(T1) = A OR B）。
//
// T1（牆上火把，貼在 NOR 中心塊 S 北側）弱充能它北邊的石頭 X；
// T2（牆上火把，貼在 X 北側）把 X 的充能狀態再反相一次，還原成 OR。
// T1 跟 T2 之間隔著 X，彼此不直接相鄰；X 跟輸入的支撐塊、拉桿都離得
// 夠遠，不會被意外充能。
//
//   側視圖（沿南北向切）：
//
//   z=2   z=3   z=4   z=5(S 所在的那一排)
//   T2 -- X -- T1 -- S
//
//   （T2、T1 都是牆上火把，箭頭方向朝北；X 是普通石頭，被 T1 弱充能，
//    T2 反相 X 的充能狀態。S 是 NOR 的中心塊，輸入接法跟 NOR 測試相同。）
// ---------------------------------------------------------------------
#[test]
fn an_or_gate_built_from_two_inversions_matches_its_truth_table() {
    let mut world = World::new(12, 4, 12);
    let center = Position::new(6, 1, 6);
    let gate = build_nor_gate(&mut world, center, 2);
    let lever_a = gate.levers[0];
    let lever_b = gate.levers[1];

    // T1 已經由 build_nor_gate 放好，就是 gate.output_torch。
    // 再往北推兩格：中繼方塊 X，然後是最終輸出火把 T2。
    let relay_block = gate.output_torch.offset(Facing::North);
    let output_torch = relay_block.offset(Facing::North);

    world.set(relay_block.x, relay_block.y, relay_block.z, stone());
    world.set(
        output_torch.x,
        output_torch.y,
        output_torch.z,
        wall_torch(Facing::North),
    );

    let mut simulator = Simulator::new(world);
    simulator
        .run_until_stable(MAX_TICKS)
        .expect("circuit must settle");

    // 四列全測：00->0, 01->1, 10->1, 11->1
    let rows: [(bool, bool, bool); 4] = [
        (false, false, false),
        (false, true, true),
        (true, false, true),
        (true, true, true),
    ];

    for (a, b, expected) in rows {
        set_lever(&mut simulator, lever_a, a);
        set_lever(&mut simulator, lever_b, b);
        let output = read_torch(&simulator, output_torch);
        assert_eq!(
            output, expected,
            "OR({a}, {b}) should be {expected}, got {output}"
        );
    }
}

// ---------------------------------------------------------------------
// 訊號強度隨距離衰減：拉桿驅動一條 20 格長的紅石粉，每一格 -1，
// 第 15 格還有 1 格強度，第 16 格已經是 0。
// ---------------------------------------------------------------------
#[test]
fn signal_strength_falls_off_over_distance_in_a_real_circuit() {
    let mut world = World::new(25, 3, 3);

    let lever_pos = Position::new(0, 0, 0);
    world.set(lever_pos.x, lever_pos.y, lever_pos.z, lever(false));

    // 沿 +x 鋪 20 格：每格底下一塊石頭，上面一格紅石粉。
    // 拉桿緊貼著第一塊支撐石（x=1），粉線從 x=1 開始。
    for x in 1..=20 {
        world.set(x, 0, 0, stone());
        world.set(x, 1, 0, dust());
    }

    let mut simulator = Simulator::new(world);
    set_lever(&mut simulator, lever_pos, true);

    for x in 1..=15 {
        let power = simulator.world().get(x, 1, 0).power;
        assert!(power > 0, "dust at x={x} should still carry signal, got {power}");
    }
    assert_eq!(
        simulator.world().get(16, 1, 0).power,
        0,
        "dust at x=16 is the sixteenth block from the source -- signal must have died out"
    );
    for x in 17..=20 {
        assert_eq!(simulator.world().get(x, 1, 0).power, 0, "dust at x={x}");
    }
}
