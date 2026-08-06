//! BCD-to-seven-segment decoder: netlist generator for the project's
//! reference circuit.
//!
//! `build_seven_segment_netlist` is the only entry point most callers need --
//! it returns a ready-to-`compile` [`Netlist`] plus a lookup from segment name
//! (`"a".."g"`) to that segment's actual signal name in `netlist.outputs`.

use std::collections::HashMap;

use crate::circuits::netlist_builder::NetlistBuilder;
use crate::compile::Netlist;

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

/// Build the AND-reduced minterm signal for every decimal digit 0..=9.
///
/// Each minterm is the AND of 4 literals: `d_i` itself if that bit is 1, or a
/// shared `NOT d_i` if it is 0. This is the one place the digit-to-bits
/// decoding happens, so both the full decoder and any single-segment slice of
/// it build on the exact same minterms instead of re-deriving them.
fn build_minterms(builder: &mut NetlistBuilder) -> Vec<String> {
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
    minterm_signal
}

/// 產生 BCD-to-seven-segment decoder 的網表。
///
/// 回傳網表本身，以及 segment 名稱 (`a`..`g`) 對到它在 `netlist.outputs`
/// 裡實際訊號名稱的對照——OR 樹產生的訊號名稱是 `gN` 這種內部名字，不是
/// 字面上的 "a"，呼叫端要靠這個對照表才知道哪個訊號是哪個 segment。
pub fn build_seven_segment_netlist() -> (Netlist, HashMap<&'static str, String>) {
    let mut builder = NetlistBuilder::new();
    let minterm_signal = build_minterms(&mut builder);

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

/// Build the netlist for a single segment of the decoder (e.g. just segment
/// `a`, `segment_index == 0`), reusing the exact same minterms as the full
/// decoder -- this is a real slice of the target circuit, not a separate
/// hand-written approximation of it.
///
/// Returns the netlist and the name of its one output signal.
pub fn build_single_segment_netlist(segment_index: usize) -> (Netlist, String) {
    assert!(
        segment_index < SEGMENT_NAMES.len(),
        "segment index must be in 0..{}, got {segment_index}",
        SEGMENT_NAMES.len()
    );

    let mut builder = NetlistBuilder::new();
    let minterm_signal = build_minterms(&mut builder);

    let contributing: Vec<String> = (0..10)
        .filter(|&value| TRUTH_TABLE[value][segment_index] == 1)
        .map(|value| minterm_signal[value].clone())
        .collect();
    let signal = builder.or_reduce(contributing);

    let netlist = Netlist {
        inputs: INPUT_NAMES.iter().map(|s| s.to_string()).collect(),
        outputs: vec![signal.clone()],
        gates: builder.gates,
    };

    (netlist, signal)
}
