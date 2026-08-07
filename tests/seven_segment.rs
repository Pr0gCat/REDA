//! 驗收測試：把一個 BCD-to-seven-segment decoder 編譯成紅石電路。
//!
//! 這是這個專案的目標電路。三個前人的自動排線工具都死在它身上：
//! PERSHING 的 router 論文裡這一項結果欄位寫的是「still running」；
//! MinecraftHDL 產出過一個 83x442 blocks 的版本，超過 Minecraft 的區塊
//! 載入半徑，實際上跑不起來。
//!
//! `the_compiled_decoder_matches_its_truth_table` is the acceptance test for
//! that target: 84 NOR gates and 156 edges, placed and routed into a footprint
//! that fits inside a ticking area, and then checked against all 112 truth
//! table entries through the real redstone simulator.
//!
//! 網表是**產生**出來的，不是手寫的 —— 手寫 60 個閘正是抄寫錯誤的來源。
//! 用 sum-of-products：4 個輸入先各自反相一次（共用），16 個 minterm 裡
//! 只有 10 個會被用到（10-15 沒有對應的數字，直接跳過），每個 minterm
//! 在所有需要它的 segment 之間共用，最後每個 segment 是它用到的 minterm
//! 的 OR。
//!
//! 這裡唯一的閘元件是 `place_nor_gate`，硬體上最多支援 3 個輸入（西、
//! 東、南三個方向留給輸入，北留給輸出）——所以 AND4（4 個輸入的
//! minterm）跟最寬 9 個輸入的 segment OR 都不能是單一一個閘，得展開成
//! fan-in <= 3 的樹。做這件事的產生器（`NetlistBuilder`）跟
//! `build_seven_segment_netlist` 現在都在 `reda::circuits::seven_segment`
//! 裡，這個檔案只負責驗證它產生的網表。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use reda::circuits::seven_segment::{build_seven_segment_netlist, INPUT_NAMES, SEGMENT_NAMES, TRUTH_TABLE};
use reda::compile::{compile, Netlist};
use reda::formats::litematic;
use reda::redstone::simulator::Simulator;
use reda::timing::{
    game_ticks_to_redstone_ticks, game_ticks_to_seconds, observations_to_result, summarize_worst_case,
    watch_all_nets, TransitionResult,
};

/// 期望的真值表，展開成全部 16 個輸入組合（10-15 全熄滅），方便測試逐項比對。
fn expected_segments(value: u8) -> [bool; 7] {
    if (value as usize) < TRUTH_TABLE.len() {
        TRUTH_TABLE[value as usize].map(|bit| bit == 1)
    } else {
        [false; 7]
    }
}

// ---------------------------------------------------------------------
// 布林模擬：在還沒編譯成方塊之前，直接在布林值上跑一遍網表。
// ---------------------------------------------------------------------

/// 依照網表的拓樸順序，直接在布林值上算出每個訊號的值。
fn evaluate_netlist(netlist: &Netlist, input_values: &HashMap<&str, bool>) -> HashMap<String, bool> {
    let order = netlist.topological_order().expect("netlist must be acyclic");
    let mut values: HashMap<String, bool> = HashMap::new();
    for (&name, &value) in input_values {
        values.insert(name.to_string(), value);
    }
    for &gate_index in &order {
        let gate = &netlist.gates[gate_index];
        let nor_output = !gate.inputs.iter().any(|input| {
            *values
                .get(input.as_str())
                .unwrap_or_else(|| panic!("signal {input} has no value yet when evaluating {}", gate.output))
        });
        values.insert(gate.output.clone(), nor_output);
    }
    values
}

#[test]
fn the_netlist_is_logically_correct_before_placement() {
    let (netlist, segment_signal) = build_seven_segment_netlist();

    let mut mismatches = Vec::new();
    for value in 0u8..16 {
        let bits = [(value >> 3) & 1, (value >> 2) & 1, (value >> 1) & 1, value & 1];
        let input_values: HashMap<&str, bool> =
            INPUT_NAMES.iter().zip(bits.iter()).map(|(&name, &bit)| (name, bit == 1)).collect();

        let values = evaluate_netlist(&netlist, &input_values);
        let expected = expected_segments(value);

        for (segment_index, &segment_name) in SEGMENT_NAMES.iter().enumerate() {
            let signal_name = &segment_signal[segment_name];
            let actual = values[signal_name];
            if actual != expected[segment_index] {
                mismatches.push(format!(
                    "d3d2d1d0={value:04b} segment {segment_name}: expected {}, got {actual}",
                    expected[segment_index]
                ));
            }
        }
    }

    assert!(mismatches.is_empty(), "netlist logic is wrong:\n{}", mismatches.join("\n"));
}

// ---------------------------------------------------------------------
// 端到端：編譯成方塊，用模擬器跑，逐項比對真值表。
// ---------------------------------------------------------------------

const MAX_TICKS: u64 = 2000;

fn set_lever(simulator: &mut Simulator, position: (i32, i32, i32), on: bool) {
    let mut state = simulator.world().get(position.0, position.1, position.2).clone();
    state.lit = on;
    simulator.world_mut().set(position.0, position.1, position.2, state);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle after changing an input");
}

/// Same as `set_lever`, but also records the transition's timing -- the
/// simulator must already have an observer attached (see `watch_all_nets`).
fn set_lever_and_record(
    simulator: &mut Simulator,
    position: (i32, i32, i32),
    on: bool,
    transitions: &mut Vec<TransitionResult>,
) {
    simulator.reset_observer();
    let start_tick = simulator.current_tick();
    set_lever(simulator, position, on);
    let settle_game_ticks = simulator.current_tick() - start_tick;
    transitions.push(observations_to_result(simulator.observations(), start_tick, settle_game_ticks));
}

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator.world().get(position.0, position.1, position.2).lit
}

/// Print the worst case across an instrumented sweep: settle time (game
/// ticks / redstone ticks / seconds), the logic-depth lower bound, the ratio
/// between them, the critical path, and how many input vectors glitched
/// which outputs. This is dynamic timing analysis (`reda::timing`) applied
/// to the same sweep the correctness check above already ran.
fn report_timing(label: &str, netlist: &Netlist, outputs: &[String], transitions: &[TransitionResult]) {
    let summary = summarize_worst_case(netlist, outputs, transitions);
    eprintln!(
        "{label} timing: worst-case settle = {} game ticks ({:.1} redstone ticks, {:.3}s)",
        summary.worst_settle_game_ticks,
        game_ticks_to_redstone_ticks(summary.worst_settle_game_ticks),
        game_ticks_to_seconds(summary.worst_settle_game_ticks),
    );
    eprintln!(
        "{label} timing: logic-depth bound = {} gates -> {} game ticks; ratio (measured/bound) = {:.2}x",
        summary.logic_depth, summary.logic_depth_bound_game_ticks, summary.ratio,
    );
    eprintln!(
        "{label} timing: critical path to worst output `{}`: {}",
        summary.critical_output,
        summary.critical_path.join(" -> ")
    );
    eprintln!(
        "{label} timing: glitches by output (number of input vectors that glitched it): {:?}",
        summary.glitch_counts
    );
}

/// Java Edition's maximum simulation distance is 32 chunks, so at most about
/// 1040 blocks across ever tick at once. A circuit wider than that has parts
/// sitting in unloaded chunks where redstone simply does not run, which is
/// what sank MinecraftHDL's 83x442 version of this same decoder. 512 is a
/// deliberately conservative fraction of the ticking area: it leaves room for
/// the player, for the circuit to sit off-centre, and for a smaller
/// simulation-distance setting than the maximum.
const MAX_HORIZONTAL_EXTENT: i32 = 512;

#[test]
fn the_compiled_decoder_matches_its_truth_table() {
    // 這是這個專案的目標電路。三個前人團隊都死在它身上：PERSHING 的 router
    // 論文裡這一項結果欄位是「still running」；MinecraftHDL 產出過一個
    // 83x442 blocks 的版本，超過 Minecraft 的區塊載入半徑，實際上跑不起來。
    let (netlist, segment_signal) = build_seven_segment_netlist();
    let gate_count = netlist.gates.len();

    let compile_start = Instant::now();
    let compiled = compile(&netlist).expect("this netlist is acyclic and fully driven");
    let compile_elapsed = compile_start.elapsed();

    let (size_x, size_y, size_z) = compiled.world.size();
    let non_air_blocks = {
        let mut count = 0usize;
        for x in 0..size_x {
            for y in 0..size_y {
                for z in 0..size_z {
                    if compiled.world.get(x, y, z).kind != reda::redstone::world::block::BlockKind::Air {
                        count += 1;
                    }
                }
            }
        }
        count
    };

    eprintln!("seven-segment decoder: {gate_count} gates");
    eprintln!("bounding box: {size_x} x {size_y} x {size_z}, {non_air_blocks} non-air blocks");
    eprintln!("compile() took {compile_elapsed:?}");

    // The circuit has to fit inside a ticking area to be a circuit at all, so
    // this is a correctness bound rather than a nice-to-have. Y is exempt:
    // Minecraft's simulation distance is horizontal, and building upwards is
    // usually the right call in redstone.
    assert!(
        size_x <= MAX_HORIZONTAL_EXTENT && size_z <= MAX_HORIZONTAL_EXTENT,
        "the decoder must fit in a loadable footprint, got {size_x} x {size_y} x {size_z}"
    );

    let watched = watch_all_nets(&compiled);

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");
    simulator.attach_observer(watched);

    let lever_positions: HashMap<&str, (i32, i32, i32)> = INPUT_NAMES
        .iter()
        .map(|&name| (name, *compiled.input_positions.get(name).unwrap()))
        .collect();
    let output_positions: HashMap<&str, (i32, i32, i32)> = SEGMENT_NAMES
        .iter()
        .map(|&name| (name, *compiled.output_positions.get(&segment_signal[name]).unwrap()))
        .collect();

    let simulate_start = Instant::now();
    let mut mismatches = Vec::new();
    let mut transitions: Vec<TransitionResult> = Vec::new();
    for value in 0u8..16 {
        let bits = [(value >> 3) & 1, (value >> 2) & 1, (value >> 1) & 1, value & 1];
        for (&name, &bit) in INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever_and_record(&mut simulator, lever_positions[name], bit == 1, &mut transitions);
        }

        let expected = expected_segments(value);
        for (segment_index, &segment_name) in SEGMENT_NAMES.iter().enumerate() {
            let actual = read_output(&simulator, output_positions[segment_name]);
            if actual != expected[segment_index] {
                mismatches.push(format!(
                    "d3d2d1d0={value:04b} segment {segment_name}: expected {}, got {actual}",
                    expected[segment_index]
                ));
            }
        }
    }
    let simulate_elapsed = simulate_start.elapsed();
    eprintln!("simulating all 16 inputs took {simulate_elapsed:?}");

    assert!(
        mismatches.is_empty(),
        "compiled decoder does not match its truth table ({}/112 wrong):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );

    let outputs: Vec<String> = SEGMENT_NAMES.iter().map(|&name| segment_signal[name].clone()).collect();
    report_timing("seven_segment", &netlist, &outputs, &transitions);
}

#[test]
fn the_compiled_decoder_saves_to_a_litematic() {
    let (netlist, _segment_signal) = build_seven_segment_netlist();
    let compiled = compile(&netlist).expect("this netlist is acyclic and fully driven");

    let (size_x, size_y, size_z) = compiled.world.size();
    eprintln!("seven-segment decoder litematic size: {size_x} x {size_y} x {size_z}");
    // MinecraftHDL's version of this same circuit was 83x442 blocks -- past
    // Minecraft's own chunk loading radius, so it could not actually be run
    // in-game. Print ours alongside it so the number is comparable.

    let mut path = PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR").unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().to_string()),
    );
    path.push("reda_seven_segment.litematic");

    litematic::save(&path, &compiled.world, "seven_segment_decoder").expect("saving must succeed");
    let loaded = litematic::load(&path).expect("loading must succeed");

    assert_eq!(loaded.size(), compiled.world.size(), "loaded world must have the same dimensions");

    let mut non_air_blocks = 0usize;
    for x in 0..size_x {
        for y in 0..size_y {
            for z in 0..size_z {
                let original = compiled.world.get(x, y, z);
                let round_tripped = loaded.get(x, y, z);
                assert_eq!(original.kind, round_tripped.kind, "block kind mismatch at ({x},{y},{z})");
                assert_eq!(original.name, round_tripped.name, "block name mismatch at ({x},{y},{z})");
                assert_eq!(original.facing, round_tripped.facing, "facing mismatch at ({x},{y},{z})");
                if original.kind != reda::redstone::world::block::BlockKind::Air {
                    non_air_blocks += 1;
                }
            }
        }
    }
    eprintln!("seven-segment decoder non-air block count: {non_air_blocks}");

    let _ = std::fs::remove_file(&path);
}
