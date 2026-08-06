//! 已知向量測試：這些數字是手工推導的，用來釘住跨 long 的打包慣例。
//!
//! 只有 `raw_long_layout_pins_the_cross_long_convention` 能區分兩種慣例 ——
//! 它直接檢查打包後的原始 long。其餘幾個測試在兩種慣例下都會通過，
//! 因為它們用同一份實作打包再解包。

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

#[test]
fn raw_long_layout_pins_the_cross_long_convention() {
    // 這是唯一能區分兩種慣例的測試 —— 其餘幾個在兩種慣例下都會通過。
    //
    // bits=5 時，索引 12 的起點在 bit 60：跨界慣例把它的低 4 bit 放進
    // long[0] 的 bit 60..63，第 5 個 bit 放進 long[1] 的 bit 0；
    // 1.16+ 的非跨界慣例則會把整個項目推到 long[1]，讓 long[0] 的高 4 bit 空著。
    let mut values = vec![0u32; 12];
    values.push(31);

    let packed = pack(&values, 5);

    assert_eq!(
        packed[0] as u64,
        0xF000_0000_0000_0000u64,
        "long[0] 的高 4 bit 必須裝著 31 的低 4 bit —— 若為 0 表示實作改成了非跨界慣例"
    );
    assert_eq!(
        packed[1] as u64 & 0x1,
        1,
        "31 的第 5 個 bit 必須落在 long[1] 的 bit 0"
    );

    assert_eq!(unpack(&packed, 5, values.len()), values);
}
