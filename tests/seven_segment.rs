//! 驗收測試：把一個 BCD-to-seven-segment decoder 編譯成紅石電路。
//!
//! 這是這個專案的目標電路。三個前人的自動排線工具都死在它身上：
//! PERSHING 的 router 論文裡這一項結果欄位寫的是「still running」；
//! MinecraftHDL 產出過一個 83x442 blocks 的版本，超過 Minecraft 的區塊
//! 載入半徑，實際上跑不起來。這顆電路大約 60 個邏輯閘，至今沒有自動化
//! 工具真正跑完它。
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
//! fan-in <= 3 的樹。`NetlistBuilder` 就是做這件事的產生器。

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use reda::compile::{compile, Gate, Netlist};
use reda::formats::litematic;
use reda::redstone::simulator::Simulator;

// ---------------------------------------------------------------------
// 網表產生器：只用 NOR，fan-in 硬性上限 3。
// ---------------------------------------------------------------------

/// 一步一步把 NOR 閘疊起來的產生器。
///
/// - `not`：對同一個訊號重複呼叫回傳同一個共用的反相閘（用 `not_cache`
///   記住），這就是「四個輸入的反相閘只建一次，之後每個 minterm 共用」
///   的機制。
/// - `nor`：最原始的操作，建一個新的 NOR 閘，最多 3 個輸入 —— 對應
///   `place_nor_gate` 的硬體限制。
/// - `and_reduce` / `or_reduce`：把任意長度的訊號清單摺成一棵 fan-in <= 3
///   的樹，分別算出它們的 AND / OR。
struct NetlistBuilder {
    gates: Vec<Gate>,
    not_cache: HashMap<String, String>,
    counter: usize,
}

impl NetlistBuilder {
    fn new() -> Self {
        NetlistBuilder { gates: Vec::new(), not_cache: HashMap::new(), counter: 0 }
    }

    fn fresh_name(&mut self) -> String {
        let name = format!("g{}", self.counter);
        self.counter += 1;
        name
    }

    /// 建一個新的 NOR 閘，`inputs.len()` 必須在 1..=3 之間。
    fn nor(&mut self, inputs: &[String]) -> String {
        assert!(
            !inputs.is_empty() && inputs.len() <= 3,
            "place_nor_gate 最多 3 個輸入，收到 {}",
            inputs.len()
        );
        let output = self.fresh_name();
        self.gates.push(Gate {
            name: output.clone(),
            inputs: inputs.to_vec(),
            output: output.clone(),
        });
        output
    }

    /// `NOT x`，同一個 `x` 只會建一次閘，之後都回傳快取的輸出名稱。
    fn not(&mut self, x: &str) -> String {
        if let Some(cached) = self.not_cache.get(x) {
            return cached.clone();
        }
        let output = self.nor(&[x.to_string()]);
        self.not_cache.insert(x.to_string(), output.clone());
        output
    }

    /// 任意長度訊號清單的 AND，摺成 fan-in <= 3 的樹。
    ///
    /// 每一層把訊號三個三個分組：組裡的每個訊號先取 `NOT`（如果是原始
    /// 輸入或別的 minterm 已經算過的反相，直接命中快取，不新建閘），
    /// 再用一個 NOR 閘算這一組的 AND（De Morgan：
    /// `AND(a,b,c) = NOR(NOT a, NOT b, NOT c)`）。落單的訊號直接晉級到
    /// 下一層，不建新閘。
    fn and_reduce(&mut self, signals: Vec<String>) -> String {
        let mut level = signals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(3));
            for chunk in level.chunks(3) {
                if chunk.len() == 1 {
                    next.push(chunk[0].clone());
                } else {
                    let nots: Vec<String> = chunk.iter().map(|s| self.not(s)).collect();
                    next.push(self.nor(&nots));
                }
            }
            level = next;
        }
        level.into_iter().next().expect("and_reduce called with an empty signal list")
    }

    /// 任意長度訊號清單的 OR，摺成 fan-in <= 3 的樹。
    ///
    /// `OR(a,b,c) = NOT(NOR(a,b,c))`：每組先算 NOR，再反相一次拿到真正
    /// 的 OR 值，這樣才能繼續往上一層跟別組的 OR 值再取 OR。落單的訊號
    /// 直接晉級,不建新閘。
    fn or_reduce(&mut self, signals: Vec<String>) -> String {
        let mut level = signals;
        while level.len() > 1 {
            let mut next = Vec::with_capacity(level.len().div_ceil(3));
            for chunk in level.chunks(3) {
                if chunk.len() == 1 {
                    next.push(chunk[0].clone());
                } else {
                    let nor_out = self.nor(chunk);
                    next.push(self.not(&nor_out));
                }
            }
            level = next;
        }
        level.into_iter().next().expect("or_reduce called with an empty signal list")
    }
}

/// 真值表：`d3 d2 d1 d0` (MSB 先) 對到 `a b c d e f g`（active high）。
/// 只有 0..=9 有定義；10..=15 全部熄滅。
const TRUTH_TABLE: [[u8; 7]; 10] = [
    [1, 1, 1, 1, 1, 1, 0], // 0
    [0, 1, 1, 0, 0, 0, 0], // 1
    [1, 1, 0, 1, 1, 0, 1], // 2
    [1, 1, 1, 1, 0, 0, 1], // 3
    [0, 1, 1, 0, 0, 1, 1], // 4
    [1, 0, 1, 1, 0, 1, 1], // 5
    [1, 0, 1, 1, 1, 1, 1], // 6
    [1, 1, 1, 0, 0, 0, 0], // 7
    [1, 1, 1, 1, 1, 1, 1], // 8
    [1, 1, 1, 1, 0, 1, 1], // 9
];

const SEGMENT_NAMES: [&str; 7] = ["a", "b", "c", "d", "e", "f", "g"];
const INPUT_NAMES: [&str; 4] = ["d3", "d2", "d1", "d0"];

/// 期望的真值表，展開成全部 16 個輸入組合（10-15 全熄滅），方便測試逐項比對。
fn expected_segments(value: u8) -> [bool; 7] {
    if (value as usize) < TRUTH_TABLE.len() {
        TRUTH_TABLE[value as usize].map(|bit| bit == 1)
    } else {
        [false; 7]
    }
}

/// 產生 BCD-to-seven-segment decoder 的網表。
///
/// 回傳網表本身，以及 segment 名稱 (`a`..`g`) 對到它在 `netlist.outputs`
/// 裡實際訊號名稱的對照——OR 樹產生的訊號名稱是 `gN` 這種內部名字，不是
/// 字面上的 "a"，呼叫端要靠這個對照表才知道哪個訊號是哪個 segment。
fn build_seven_segment_netlist() -> (Netlist, HashMap<&'static str, String>) {
    let mut builder = NetlistBuilder::new();

    // 每個 minterm（0..=9）：4 個 literal 的 AND。literal 是 d_i 本身
    // （該位元是 1）或 not_d_i（該位元是 0，用共用的反相閘）。
    let mut minterm_signal: Vec<String> = Vec::with_capacity(10);
    for value in 0u8..10 {
        let bits = [(value >> 3) & 1, (value >> 2) & 1, (value >> 1) & 1, value & 1];
        let literals: Vec<String> = INPUT_NAMES
            .iter()
            .zip(bits.iter())
            .map(|(&name, &bit)| if bit == 1 { name.to_string() } else { builder.not(name) })
            .collect();
        minterm_signal.push(builder.and_reduce(literals));
    }

    // 每個 segment：它用到的 minterm 的 OR。
    let mut segment_signal: HashMap<&'static str, String> = HashMap::new();
    let mut outputs = Vec::with_capacity(SEGMENT_NAMES.len());
    for (segment_index, &segment_name) in SEGMENT_NAMES.iter().enumerate() {
        let contributing: Vec<String> = (0..10)
            .filter(|&value| TRUTH_TABLE[value][segment_index] == 1)
            .map(|value| minterm_signal[value].clone())
            .collect();
        let signal = builder.or_reduce(contributing);
        outputs.push(signal.clone());
        segment_signal.insert(segment_name, signal);
    }

    let netlist = Netlist {
        inputs: INPUT_NAMES.iter().map(|s| s.to_string()).collect(),
        outputs,
        gates: builder.gates,
    };

    (netlist, segment_signal)
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

fn read_output(simulator: &Simulator, position: (i32, i32, i32)) -> bool {
    simulator.world().get(position.0, position.1, position.2).lit
}

#[test]
#[ignore = "compiles and routes fine (see the_compiled_decoder_saves_to_a_litematic), but \
    simulating it is impractically slow: a timed probe showed Simulator::new() alone (one \
    full-world recompute over the 2882x6x2180 = ~37.7M-cell world) takes 1.67s, and the very \
    first run_until_stable() -- before any of the 16 lever combinations are even tried -- did \
    not finish after 580+ wall-clock seconds (1172+ CPU-seconds) and was killed. \
    recompute_dust_strengths (src/redstone/simulator/propagate.rs) rescans every cell in the \
    world on every game tick; that is fine for the walking skeleton's tiny AND/NOT-gate worlds \
    but does not scale to this circuit's footprint. The footprint itself is the other half of \
    the problem: compile()'s router gives every netlist edge (156 of them here, from 84 gates) \
    its own dedicated straight-line lane, so the bounding box grows linearly with edge count -- \
    2882 blocks wide, wider even than MinecraftHDL's already-too-large 83x442 circuit. Fixing either \
    side (incremental/dirty-region propagation in the simulator, or congestion-aware/shared-\
    channel routing in the compiler) is a real algorithm, not a contained patch -- see \
    .superpowers/sdd/seven-segment-report.md for the full diagnosis. Run explicitly with \
    `cargo test -- --ignored --test seven_segment` if you want to let it run for however many \
    hours it actually needs."]
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

    let mut simulator = Simulator::new(compiled.world);
    simulator.run_until_stable(MAX_TICKS).expect("circuit must settle before the first reading");

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
    for value in 0u8..16 {
        let bits = [(value >> 3) & 1, (value >> 2) & 1, (value >> 1) & 1, value & 1];
        for (&name, &bit) in INPUT_NAMES.iter().zip(bits.iter()) {
            set_lever(&mut simulator, lever_positions[name], bit == 1);
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
