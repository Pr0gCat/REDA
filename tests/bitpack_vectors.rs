//! 已知向量測試：這些數字是手工推導的，用來釘住跨 long 的打包慣例。
//!
//! 如果有人不小心把實作改成 1.16+ 的非跨界慣例，這些測試會失敗。

use reda::formats::bitpack::{pack, unpack};

#[test]
fn two_bit_entries_pack_32_per_long() {
    // 2 bits × 32 = 64 bits = 剛好一個 long
    let values: Vec<u32> = (0..32).map(|i| i % 4).collect();
    let packed = pack(&values, 2);
    assert_eq!(packed.len(), 1, "32 two-bit entries fit in exactly one long");
    assert_eq!(unpack(&packed, 2, 32), values);
}

#[test]
fn five_bit_entries_straddle_boundaries() {
    // 5 bits/entry：第 12 個項目起點在 bit 60，跨越到下一個 long
    let values: Vec<u32> = (0..20).map(|i| (i * 3) % 32).collect();
    let packed = pack(&values, 5);
    let unpacked = unpack(&packed, 5, 20);
    assert_eq!(unpacked, values);
    assert_eq!(
        unpacked[12], values[12],
        "entry 12 starts at bit 60 and must span two longs"
    );
}

#[test]
fn known_vector_three_bits() {
    // 手工推導：values = [1, 2, 3, 4]，3 bits each
    // bit layout (LSB first): 001 010 011 100
    // long[0] = 0b100_011_010_001 = 0x8D1 = 2257
    let values = vec![1u32, 2, 3, 4];
    let packed = pack(&values, 3);
    assert_eq!(packed[0], 2257, "hand-computed packing must match");
    assert_eq!(unpack(&packed, 3, 4), values);
}
