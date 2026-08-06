//! litematica 的位元打包。
//!
//! **這是整個格式最容易寫錯的地方。**
//!
//! litematica 使用**跨 long 的舊式慣例**（pre-1.16）：一個項目的位元
//! 可以橫跨兩個 `i64` 的邊界。這與 Minecraft 1.16+ 區塊儲存所用的
//! 「非跨界」慣例**不同** —— 後者會在每個 long 末尾留下未使用的位元。
//!
//! 用錯慣例不會報錯，只會讀出垃圾，所以本模組有獨立的向量測試。
//!
//! 另外 `bits_per_entry` 的**最小值是 2**，即使 palette 只有一個項目。

/// 給定 palette 大小，算出每個項目佔幾個 bit。最小值是 2。
pub fn bits_per_entry(palette_len: usize) -> u32 {
    let needed = if palette_len <= 1 {
        1
    } else {
        usize::BITS - (palette_len - 1).leading_zeros()
    };
    needed.max(2)
}

/// 從 long array 解出 `count` 個項目。
///
/// `longs` 以有號 `i64` 儲存（NBT 的 LongArray 是有號的），但位元操作
/// 一律當成無號處理。
pub fn unpack(longs: &[i64], bits: u32, count: usize) -> Vec<u32> {
    assert!(bits >= 1 && bits <= 32, "bits must be in 1..=32");
    let mask: u64 = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let mut out = Vec::with_capacity(count);

    for i in 0..count {
        let bit_offset = (i as u64) * (bits as u64);
        let start_long = (bit_offset / 64) as usize;
        let start_bit = (bit_offset % 64) as u32;
        let end_bit = start_bit + bits;

        if start_long >= longs.len() {
            out.push(0);
            continue;
        }

        let value = if end_bit <= 64 {
            // 完全落在一個 long 裡
            ((longs[start_long] as u64) >> start_bit) & mask
        } else {
            // 跨越兩個 long —— 這正是舊式慣例
            let end_long = start_long + 1;
            if end_long >= longs.len() {
                ((longs[start_long] as u64) >> start_bit) & mask
            } else {
                let low_bits = 64 - start_bit;
                let low = (longs[start_long] as u64) >> start_bit;
                let high = (longs[end_long] as u64) << low_bits;
                (low | high) & mask
            }
        };

        out.push(value as u32);
    }

    out
}

/// 把項目打包成 long array，使用與 `unpack` 相同的跨界慣例。
pub fn pack(values: &[u32], bits: u32) -> Vec<i64> {
    assert!(bits >= 1 && bits <= 32, "bits must be in 1..=32");
    if values.is_empty() {
        return Vec::new();
    }
    let total_bits = (values.len() as u64) * (bits as u64);
    let long_count = ((total_bits + 63) / 64) as usize;
    let mut longs = vec![0u64; long_count];
    let mask: u64 = (1u64 << bits) - 1;

    for (i, &v) in values.iter().enumerate() {
        let value = (v as u64) & mask;
        let bit_offset = (i as u64) * (bits as u64);
        let start_long = (bit_offset / 64) as usize;
        let start_bit = (bit_offset % 64) as u32;
        let end_bit = start_bit + bits;

        longs[start_long] |= value << start_bit;

        if end_bit > 64 {
            let low_bits = 64 - start_bit;
            longs[start_long + 1] |= value >> low_bits;
        }
    }

    longs.into_iter().map(|x| x as i64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_per_entry_has_a_floor_of_two() {
        assert_eq!(bits_per_entry(1), 2);
        assert_eq!(bits_per_entry(2), 2);
        assert_eq!(bits_per_entry(4), 2);
        assert_eq!(bits_per_entry(5), 3);
        assert_eq!(bits_per_entry(8), 3);
        assert_eq!(bits_per_entry(9), 4);
        assert_eq!(bits_per_entry(256), 8);
        assert_eq!(bits_per_entry(257), 9);
    }

    #[test]
    fn pack_then_unpack_roundtrips() {
        for bits in 2..=16u32 {
            let max = (1u32 << bits) - 1;
            let values: Vec<u32> = (0..100).map(|i| (i * 7) % (max + 1)).collect();
            let packed = pack(&values, bits);
            let unpacked = unpack(&packed, bits, values.len());
            assert_eq!(unpacked, values, "roundtrip failed at bits={bits}");
        }
    }

    #[test]
    fn entries_span_across_long_boundaries() {
        // 3 bits/entry：第 21 個項目（索引 21）跨越第一個和第二個 long。
        // 21 * 3 = 63，所以它只有 1 bit 在第一個 long 裡，2 bits 在第二個。
        // 這正是舊式（pre-1.16）打包慣例，與 1.16+ 的非跨界慣例不同。
        let values: Vec<u32> = (0..25).map(|i| i % 8).collect();
        let packed = pack(&values, 3);
        let unpacked = unpack(&packed, 3, values.len());
        assert_eq!(unpacked[21], values[21], "entry 21 must span two longs");
        assert_eq!(unpacked, values);
    }

    #[test]
    fn unpack_stops_at_requested_count() {
        let values: Vec<u32> = vec![1, 2, 3];
        let packed = pack(&values, 2);
        let unpacked = unpack(&packed, 2, 3);
        assert_eq!(unpacked.len(), 3);
    }
}
