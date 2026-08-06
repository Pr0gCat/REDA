//! BCD-to-seven-segment decoder: netlist generator for the project's
//! reference circuit.
//!
//! `build_seven_segment_netlist` is the only entry point most callers need --
//! it returns a ready-to-`compile` [`Netlist`] plus a lookup from segment name
//! (`"a".."g"`) to that segment's actual signal name in `netlist.outputs`.

use std::collections::HashMap;

use crate::compile::{Gate, Netlist};

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
pub const TRUTH_TABLE: [[u8; 7]; 10] = [
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

pub const SEGMENT_NAMES: [&str; 7] = ["a", "b", "c", "d", "e", "f", "g"];
pub const INPUT_NAMES: [&str; 4] = ["d3", "d2", "d1", "d0"];

/// 產生 BCD-to-seven-segment decoder 的網表。
///
/// 回傳網表本身，以及 segment 名稱 (`a`..`g`) 對到它在 `netlist.outputs`
/// 裡實際訊號名稱的對照——OR 樹產生的訊號名稱是 `gN` 這種內部名字，不是
/// 字面上的 "a"，呼叫端要靠這個對照表才知道哪個訊號是哪個 segment。
pub fn build_seven_segment_netlist() -> (Netlist, HashMap<&'static str, String>) {
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
