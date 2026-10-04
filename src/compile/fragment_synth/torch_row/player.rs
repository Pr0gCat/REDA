//! Player-inspired matrix, selected by the qualified two-input/three-output compiler.
use super::*;
use crate::compile::{topology::GateKind, Gate};
use std::collections::BTreeMap;
fn put(w: &mut World, at: Position, kind: BlockKind) {
    let mut b = BlockState::air();
    b.kind = kind;
    b.name = match kind {
        BlockKind::Solid => "minecraft:stone",
        BlockKind::RedstoneWire => "minecraft:redstone_wire",
        BlockKind::WallTorch => "minecraft:redstone_wall_torch",
        BlockKind::Repeater => "minecraft:repeater",
        BlockKind::Lever => "minecraft:lever",
        BlockKind::Lamp => "minecraft:redstone_lamp",
        _ => unreachable!(),
    }
    .into();
    if kind == BlockKind::WallTorch {
        b.facing = Some(Facing::East);
        b.lit = true;
    }
    if kind == BlockKind::Repeater {
        b.facing = Some(Facing::West);
        b.delay = 1;
    }
    if kind == BlockKind::Lever {
        b.face = Some(Face::Floor);
        b.facing = Some(Facing::North);
    }
    w.set(at.x, at.y, at.z, b);
}
fn wire(w: &mut World, p: Position) {
    put(w, Position::new(p.x, p.y - 1, p.z), BlockKind::Solid);
    put(w, p, BlockKind::RedstoneWire);
}
fn program_rows(words: [u8; 4]) -> Vec<(usize, u8, u8, u8)> {
    let high = words.into_iter().fold(7, |a, b| a & b);
    let mut rows = BTreeMap::new();
    for bit in 0..3 {
        let table: Vec<_> = words.iter().map(|w| w & !high & (1 << bit) != 0).collect();
        for cube in cover::cover(&table, true) {
            *rows.entry((cube.bits, cube.mask)).or_insert(0u8) |= 1 << bit;
        }
    }
    assert!(rows.len() <= 6);
    rows.into_iter()
        .enumerate()
        .map(|(index, ((bits, mask), word))| (index, bits as u8, mask as u8, word))
        .collect()
}
fn row_x(index: usize) -> i32 {
    2 + 4 * index as i32
}

fn positive_literal(bits: u8, mask: u8) -> bool {
    mask.count_ones() == 1 && bits & mask == mask
}

fn direct_zero_product(rows: &[(usize, u8, u8, u8)]) -> Option<u8> {
    if !needs_input_buffer(rows)
        || rows.len() != 3
        || !rows
            .iter()
            .all(|r| r.2.count_ones() == 2 || (r.2.count_ones() == 1 && r.1 == 0))
        || !(0..3).all(|bit| rows.iter().filter(|r| r.3 & (1 << bit) != 0).count() == 1)
    {
        return None;
    }
    rows.iter().find(|r| r.1 == 0 && r.2 == 3).map(|r| r.3)
}

// Independent 00, 11 and mixed full cubes each drive one output. Keep
// matched restoration for the shared decoder, but feed the mixed comparator
// from the two public lever floors, avoiding both decoder restoration stages.
fn independent_mixed_product(rows: &[(usize, u8, u8, u8)]) -> Option<(u8, u8)> {
    direct_zero_product(rows)?;
    rows.iter()
        .find(|r| r.2 == 3 && (r.1 == 1 || r.1 == 2))
        .map(|r| (r.1, r.3))
}

fn independent_negative_literal(rows: &[(usize, u8, u8, u8)]) -> Option<(usize, u8)> {
    direct_zero_product(rows)?;
    rows.iter()
        .find(|r| r.2.count_ones() == 1 && r.1 == 0)
        .map(|r| (r.2.trailing_zeros() as usize, r.3))
}

fn matrix_address_z(rows: &[(usize, u8, u8, u8)]) -> [i32; 2] {
    match independent_mixed_product(rows) {
        Some((1, _)) => [8, 2],
        Some(_) => [2, 8],
        None if direct_zero_product(rows).is_some() => [2, 8],
        None => [4, 7],
    }
}

fn source_x(rows: &[(usize, u8, u8, u8)]) -> i32 {
    if direct_zero_product(rows).is_some() {
        return 14;
    }
    // Direct sources lose one cell of reach relative to a restored output.
    // Keep the single carrier close enough to power its farthest torch tap.
    let margin = if needs_input_buffer(rows) { 4 } else { 3 };
    16.max(rows.iter().map(|r| row_x(r.0) + margin).max().unwrap_or(16))
}

fn needs_input_buffer(rows: &[(usize, u8, u8, u8)]) -> bool {
    // The wider layout uses two restored source arms. Keep their matched
    // source stages. Complementary terms with different inversion depths also
    // retain source isolation even when they feed separate data outputs.
    rows.len() >= 5
        || rows.iter().any(|a| {
            rows.iter().any(|b| {
                a.2 == b.2
                    && a.2.count_ones() > 1
                    && (a.1 ^ b.1) & a.2 == a.2
                    && (a.1 & a.2).count_ones() != (b.1 & b.2).count_ones()
            })
        })
}

fn player_world(words: [u8; 4]) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let rows = program_rows(words);
    let source_x = source_x(&rows);
    let buffered = needs_input_buffer(&rows);
    let dual = rows.len() >= 5;
    let mut w = World::new(
        if independent_mixed_product(&rows).is_some() {
            28
        } else {
            (source_x + 5).max(20)
        },
        10,
        21,
    );
    let address_z = matrix_address_z(&rows);
    let inputs = if independent_mixed_product(&rows).is_some() {
        address_z.map(|z| {
            if z == 8 {
                Position::new(20, 5, 4)
            } else {
                Position::new(21, 5, 5)
            }
        })
    } else {
        address_z.map(|z| Position::new(source_x, 5, z))
    };
    let always_high = words.into_iter().fold(7, |a, b| a & b);
    let output_z = if direct_zero_product(&rows).is_some() {
        [10, 12, 14]
    } else {
        [10, 13, 16]
    };
    for (bit, z) in address_z.into_iter().enumerate() {
        let used = rows.iter().any(|r| r.2 & (1 << bit) != 0);
        wire(&mut w, Position::new(source_x, 5, z));
        if independent_mixed_product(&rows).is_none() {
            put(&mut w, Position::new(source_x, 5, z), BlockKind::Lever);
        }
        if !used {
            continue;
        }
        for x in 0..source_x {
            wire(&mut w, Position::new(x, 5, z));
        }
        if buffered {
            put(
                &mut w,
                Position::new(source_x - 1, 5, z),
                BlockKind::Repeater,
            );
            let mut r = w.get(source_x - 1, 5, z).clone();
            r.facing = Some(Facing::East);
            w.set(source_x - 1, 5, z, r);
        }
        if dual {
            let lane = z - 1;
            for p in [
                Position::new(source_x, 5, lane),
                Position::new(source_x - 1, 6, lane),
                Position::new(source_x - 2, 7, lane),
            ] {
                wire(&mut w, p);
            }
            for x in 15..=source_x - 3 {
                wire(&mut w, Position::new(x, 8, lane));
            }
            for p in [
                Position::new(14, 7, lane),
                Position::new(13, 6, lane),
                Position::new(12, 5, lane),
            ] {
                wire(&mut w, p);
            }
            put(&mut w, Position::new(16, 8, lane), BlockKind::Repeater);
            let mut r = w.get(16, 8, lane).clone();
            r.facing = Some(Facing::East);
            w.set(16, 8, lane, r);
        }
    }
    for &(row, address, mask, word) in &rows {
        if direct_zero_product(&rows).is_some() && address == 0
            || independent_mixed_product(&rows).is_some() && (address == 1 || address == 2)
        {
            continue;
        }
        let x = row_x(row)
            + if direct_zero_product(&rows).is_some() {
                2
            } else {
                0
            };
        for z in (*address_z.iter().min().unwrap() - 1)
            ..=if positive_literal(address, mask) && word & 4 != 0 {
                17
            } else if direct_zero_product(&rows).is_some() {
                14
            } else {
                16
            }
        {
            wire(&mut w, Position::new(x, 3, z));
        }
        for (bit, z) in address_z.into_iter().enumerate() {
            if mask & (1 << bit) == 0 {
                continue;
            }
            if address & (1 << bit) != 0 && !positive_literal(address, mask) {
                let at = Position::new(x, 4, z - 1);
                put(&mut w, at, BlockKind::WallTorch);
                let mut t = w.get(at.x, at.y, at.z).clone();
                t.facing = Some(Facing::North);
                w.set(at.x, at.y, at.z, t);
            } else {
                // Raw bus passes over the relay instead of connecting to its output.
                wire(&mut w, Position::new(x, 6, z));
                wire(&mut w, Position::new(x + 1, 6, z));
                put(&mut w, Position::new(x + 1, 3, z), BlockKind::Solid);
                put(&mut w, Position::new(x + 1, 4, z), BlockKind::Repeater);
                let mut r = w.get(x + 1, 4, z).clone();
                r.facing = Some(Facing::East);
                w.set(x + 1, 4, z, r);
            }
        }
        for (bit, z) in output_z.into_iter().enumerate() {
            if word & (1 << bit) != 0 {
                if positive_literal(address, mask) {
                    put(&mut w, Position::new(x, 2, z + 1), BlockKind::Solid);
                    put(&mut w, Position::new(x + 1, 1, z + 1), BlockKind::Solid);
                    put(&mut w, Position::new(x + 1, 2, z + 1), BlockKind::Repeater);
                    let mut r = w.get(x + 1, 2, z + 1).clone();
                    r.facing = Some(Facing::West);
                    w.set(x + 1, 2, z + 1, r);
                    put(&mut w, Position::new(x + 2, 2, z + 1), BlockKind::Solid);
                    wire(&mut w, Position::new(x + 2, 1, z + 1));
                } else {
                    put(&mut w, Position::new(x + 1, 2, z), BlockKind::WallTorch);
                }
            }
        }
    }
    let mut drivers = output_z.map(|z| {
        Position::new(
            if direct_zero_product(&rows).is_some() {
                1
            } else {
                10
            },
            1,
            z,
        )
    });
    let mut lamps = output_z.map(|z| {
        if direct_zero_product(&rows).is_some() {
            Position::new(0, 1, z)
        } else {
            Position::new(10, 1, z + 2)
        }
    });
    if let Some(word) = direct_zero_product(&rows) {
        for (x, y, z) in [
            (2, 4, 3),
            (2, 4, 4),
            (2, 4, 5),
            (3, 4, 5),
            (6, 4, 7),
            (6, 4, 6),
            (6, 4, 5),
            (5, 4, 5),
            (4, 3, 6),
            (3, 2, 6),
            (2, 2, 6),
        ] {
            wire(&mut w, Position::new(x, y, z));
        }
        put(&mut w, Position::new(4, 4, 5), BlockKind::Solid);
        put(&mut w, Position::new(4, 4, 6), BlockKind::WallTorch);
        let mut torch = w.get(4, 4, 6).clone();
        torch.facing = Some(Facing::South);
        w.set(4, 4, 6, torch);
        let bit = word.trailing_zeros() as usize;
        drivers[bit] = Position::new(2, 2, 6);
        lamps[bit] = Position::new(1, 2, 6);
        put(&mut w, lamps[bit].down(), BlockKind::Solid);
        put(&mut w, lamps[bit], BlockKind::Lamp);
    }
    if let Some((input, word)) = independent_negative_literal(&rows) {
        let z = address_z[input];
        put(&mut w, Position::new(8, 4, z), BlockKind::Solid);
        put(&mut w, Position::new(8, 4, z - 1), BlockKind::WallTorch);
        let mut t = w.get(8, 4, z - 1).clone();
        t.facing = Some(Facing::North);
        w.set(8, 4, z - 1, t);
        for at in [
            Position::new(8, 3, z - 1),
            Position::new(9, 2, z - 1),
            Position::new(10, 1, z - 1),
        ] {
            wire(&mut w, at);
        }
        let bit = word.trailing_zeros() as usize;
        drivers[bit] = Position::new(10, 1, z - 1);
        lamps[bit] = Position::new(11, 1, z - 1);
        put(&mut w, lamps[bit].down(), BlockKind::Solid);
        put(&mut w, lamps[bit], BlockKind::Lamp);
    }
    if let Some((_, word)) = independent_mixed_product(&rows) {
        // Separate raw arms feed the old matched source relays. The mixed
        // product reads strong lever floors directly; dust never bridges them.
        for x in 14..=23 {
            wire(&mut w, Position::new(x, 5, 2));
        }
        for z in 3..=5 {
            wire(&mut w, Position::new(23, 5, z));
        }
        wire(&mut w, Position::new(22, 5, 5));
        for x in 14..=18 {
            wire(&mut w, Position::new(x, 5, 8));
        }
        for z in 4..=7 {
            wire(&mut w, Position::new(18, 5, z));
        }
        wire(&mut w, Position::new(19, 5, 4));
        for at in inputs {
            put(&mut w, at.down(), BlockKind::Solid);
            put(&mut w, at, BlockKind::Lever);
        }
        put(&mut w, Position::new(20, 3, 5), BlockKind::Solid);
        let mut c = BlockState::air();
        c.kind = BlockKind::Comparator;
        c.name = "minecraft:comparator".into();
        c.facing = Some(Facing::North);
        c.extra_properties.insert("mode".into(), "subtract".into());
        w.set(20, 4, 5, c);
        wire(&mut w, Position::new(20, 4, 6));
        wire(&mut w, Position::new(20, 4, 7));
        let bit = word.trailing_zeros() as usize;
        drivers[bit] = Position::new(20, 4, 7);
        lamps[bit] = Position::new(20, 4, 8);
        put(&mut w, lamps[bit].down(), BlockKind::Solid);
        put(&mut w, lamps[bit], BlockKind::Lamp);
    }
    for (bit, z) in output_z.into_iter().enumerate() {
        if direct_zero_product(&rows).is_some_and(|word| word & (1 << bit) != 0)
            || independent_mixed_product(&rows).is_some_and(|(_, word)| word & (1 << bit) != 0)
            || independent_negative_literal(&rows).is_some_and(|(_, word)| word & (1 << bit) != 0)
        {
            continue;
        }
        for x in 1..=source_x {
            wire(&mut w, Position::new(x, 1, z));
        }
        if direct_zero_product(&rows).is_none() {
            wire(&mut w, Position::new(10, 1, z + 1));
        }
        let p = lamps[bit];
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lamp);
    }
    for (bit, z) in output_z.into_iter().enumerate() {
        if always_high & (1 << bit) != 0 {
            put(&mut w, Position::new(source_x + 1, 2, z), BlockKind::Solid);
            put(&mut w, Position::new(source_x, 2, z), BlockKind::WallTorch);
            let mut t = w.get(source_x, 2, z).clone();
            t.facing = Some(Facing::West);
            w.set(source_x, 2, z, t);
        }
    }
    // A low output has an explicit NOR0 -> NOR1 driver, rather than floating dust.
    // Keep earlier diagnostic modes unchanged so their frozen controls remain usable.
    {
        let ever_high = words.into_iter().fold(0, |a, b| a | b);
        for (bit, z) in output_z.into_iter().enumerate() {
            if ever_high & (1 << bit) == 0 {
                put(
                    &mut w,
                    Position::new(source_x - 1, 4, z + 1),
                    BlockKind::Solid,
                );
                wire(&mut w, Position::new(source_x - 1, 3, z));
                for (at, facing) in [
                    (Position::new(source_x - 1, 4, z), Facing::North),
                    (Position::new(source_x, 2, z), Facing::East),
                ] {
                    put(&mut w, at, BlockKind::WallTorch);
                    let mut t = w.get(at.x, at.y, at.z).clone();
                    t.facing = Some(facing);
                    w.set(at.x, at.y, at.z, t);
                }
            }
        }
    }
    (w, inputs, drivers, lamps)
}

fn player_logic(words: [u8; 4]) -> (Netlist, BTreeMap<String, Position>) {
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    let mut terms: [Vec<String>; 3] = std::array::from_fn(|_| Vec::new());
    let rows = program_rows(words);
    let source_x = source_x(&rows);
    let address_z = matrix_address_z(&rows);
    let buffered = needs_input_buffer(&rows);
    let dual = rows.len() >= 5;
    let high = words.into_iter().fold(7, |a, b| a & b);
    let output_z = if direct_zero_product(&rows).is_some() {
        [10, 12, 14]
    } else {
        [10, 13, 16]
    };
    let mut add = |gate: Gate, at: Position| {
        positions.insert(gate.output.clone(), at);
        gates.push(gate);
    };
    let buf = |name: String, source: &str| Gate {
        name: name.clone(),
        output: name,
        inputs: vec![source.into()],
        kind: GateKind::Buf,
    };
    for (bit, name) in ["a", "b"].into_iter().enumerate() {
        if !rows.iter().any(|r| r.2 & (1 << bit) != 0) {
            continue;
        }
        let z = address_z[bit];
        if !buffered {
            continue;
        }
        if dual {
            let right = format!("source{bit}_right");
            let left = format!("source{bit}_left");
            add(buf(right.clone(), name), Position::new(source_x - 1, 5, z));
            add(buf(left.clone(), name), Position::new(16, 8, z - 1));
            add(
                Gate::merge(format!("source{bit}"), &[&right, &left]),
                Position::new(source_x - 5, 5, z),
            );
        } else {
            add(
                buf(format!("source{bit}"), name),
                Position::new(source_x - 1, 5, z),
            );
        }
    }
    for &(address, bits, mask, word) in &rows {
        if mask.count_ones() == 1 && independent_negative_literal(&rows).is_some() {
            let input = mask.trailing_zeros() as usize;
            let name = "independent_negative_literal";
            add(
                Gate::nor(name, &[&format!("source{input}")]),
                Position::new(8, 4, address_z[input] - 1),
            );
            terms[word.trailing_zeros() as usize].push(name.into());
            continue;
        }
        if bits == 0 && mask == 3 && direct_zero_product(&rows).is_some() {
            let name = "direct_zero_product";
            add(
                Gate::nor(name, &["source0", "source1"]),
                Position::new(4, 4, 6),
            );
            terms[word.trailing_zeros() as usize].push(name.into());
            continue;
        }
        if independent_mixed_product(&rows).is_some() && (bits == 1 || bits == 2) {
            let positive = bits.trailing_zeros() as usize;
            add(
                Gate {
                    name: "mixed_product".into(),
                    output: "mixed_product".into(),
                    inputs: vec![["a", "b"][positive].into(), ["a", "b"][1 - positive].into()],
                    kind: GateKind::AndNot,
                },
                Position::new(20, 4, 5),
            );
            terms[word.trailing_zeros() as usize].push("mixed_product".into());
            continue;
        }
        let x = row_x(address)
            + if direct_zero_product(&rows).is_some() {
                2
            } else {
                0
            };
        let mut mismatches = Vec::new();
        for bit in 0..2 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let name = format!("row{address}_mismatch{bit}");
            let source = if buffered {
                format!("source{bit}")
            } else {
                ["a", "b"][bit].to_string()
            };
            let z = address_z[bit];
            if bits & (1 << bit) != 0 && !positive_literal(bits, mask) {
                add(Gate::nor(&name, &[&source]), Position::new(x, 4, z - 1));
            } else {
                add(buf(name.clone(), &source), Position::new(x + 1, 4, z));
            }
            mismatches.push(name);
        }
        let control = if mismatches.len() == 1 {
            mismatches[0].clone()
        } else {
            let control = format!("row{address}_control");
            add(
                Gate::merge(
                    &control,
                    &mismatches.iter().map(String::as_str).collect::<Vec<_>>(),
                ),
                Position::new(
                    x,
                    3,
                    if direct_zero_product(&rows).is_some() {
                        12
                    } else {
                        8
                    },
                ),
            );
            control
        };
        for (bit, z) in output_z.into_iter().enumerate() {
            if word & (1 << bit) == 0 {
                continue;
            }
            let name = format!("row{address}_bit{bit}");
            if positive_literal(bits, mask) {
                add(buf(name.clone(), &control), Position::new(x + 1, 2, z + 1));
            } else {
                add(Gate::nor(&name, &[&control]), Position::new(x + 1, 2, z));
            }
            terms[bit].push(name);
        }
    }
    let mut outputs = Vec::new();
    for (bit, z) in output_z.into_iter().enumerate() {
        let output = if high & (1 << bit) != 0 {
            let name = format!("constant_high{bit}");
            add(Gate::nor(&name, &[]), Position::new(source_x, 2, z));
            name
        } else if terms[bit].is_empty() {
            let hi = format!("constant_high{bit}");
            let lo = format!("constant_low{bit}");
            add(Gate::nor(&hi, &[]), Position::new(source_x - 1, 4, z));
            add(Gate::nor(&lo, &[&hi]), Position::new(source_x, 2, z));
            lo
        } else if terms[bit].len() == 1 {
            terms[bit][0].clone()
        } else {
            let name = format!("output{bit}");
            add(
                Gate::merge(
                    &name,
                    &terms[bit].iter().map(String::as_str).collect::<Vec<_>>(),
                ),
                Position::new(10, 1, z),
            );
            name
        };
        outputs.push(output);
    }
    (
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs,
            gates,
        },
        positions,
    )
}

pub(super) fn candidate(
    words: [u8; 4],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    build_candidate(words)
}

#[cfg(test)]
fn independent_negative_inputs(words: [u8; 4]) -> Option<Vec<(usize, u8)>> {
    independent_literal_inputs(words)
        .filter(|clusters| clusters.iter().all(|c| !c.2))
        .map(|clusters| {
            clusters
                .into_iter()
                .map(|(bit, word, _)| (bit, word))
                .collect()
        })
}
#[cfg(test)]
fn independent_literal_inputs(words: [u8; 4]) -> Option<Vec<(usize, u8, bool)>> {
    disjoint_literal_inputs(words).filter(|clusters| {
        clusters
            .iter()
            .map(|c| c.0)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            == clusters.len()
    })
}
fn disjoint_literal_inputs(words: [u8; 4]) -> Option<Vec<(usize, u8, bool)>> {
    let rows = program_rows(words);
    if rows.is_empty() {
        return None;
    }
    let mut assigned = 0;
    let mut clusters = Vec::new();
    for (_, bits, mask, word) in rows {
        if mask.count_ones() != 1 || word & assigned != 0 {
            return None;
        }
        assigned |= word;
        clusters.push((mask.trailing_zeros() as usize, word, bits & mask != 0));
    }
    Some(clusters)
}

// A single mixed-sign product may drive any subset of the outputs; all
// remaining outputs must be constants. Recognize structure, not table IDs.
fn subtract_cube(words: [u8; 4]) -> Option<(usize, u8)> {
    let rows = program_rows(words);
    if rows.len() != 1 {
        return None;
    }
    let (_, bits, mask, outputs) = rows[0];
    (mask.count_ones() == 2 && (bits & mask).count_ones() == 1)
        .then_some(((bits & mask).trailing_zeros() as usize, outputs))
}

fn subtract_library(composed: bool) -> crate::compile::topology::Library {
    use crate::compile::topology::{
        ComparatorSubtractContract, Library, LibraryEntry, LibraryEntryId, Primitive, Template,
        TemplateNode,
    };
    let default = if composed {
        comparator_library()
    } else {
        super::typed_matrix_library()
    };
    let copy = |kind, ordinal| {
        let e = default.entry_at(kind, ordinal).unwrap();
        LibraryEntry {
            name: e.name,
            template: Template {
                nodes: e.template.nodes.clone(),
                internal_edges: e.template.internal_edges.clone(),
                inputs: e.template.inputs.clone(),
                output: e.template.output,
                embedding_hints: e.template.embedding_hints.clone(),
            },
        }
    };
    let mut library = Library::new(BTreeMap::from([
        (GateKind::Nor(0), vec![copy(GateKind::Nor(0), 0)]),
        (GateKind::Buf, vec![copy(GateKind::Buf, 0)]),
        (
            GateKind::Nor(2),
            vec![{
                let d = super::typed_matrix_library();
                let e = d.entry_at(GateKind::Nor(2), 0).unwrap();
                LibraryEntry {
                    name: e.name,
                    template: Template {
                        nodes: e.template.nodes.clone(),
                        internal_edges: e.template.internal_edges.clone(),
                        inputs: e.template.inputs.clone(),
                        output: e.template.output,
                        embedding_hints: e.template.embedding_hints.clone(),
                    },
                }
            }],
        ),
        (
            GateKind::Nor(1),
            if composed {
                vec![copy(GateKind::Nor(1), 0), copy(GateKind::Nor(1), 1)]
            } else {
                vec![copy(GateKind::Nor(1), 0)]
            },
        ),
        (
            GateKind::AndNot,
            vec![LibraryEntry {
                name: "direct comparator subtract",
                template: Template {
                    nodes: vec![(TemplateNode::Torch, Primitive::Comparator)],
                    inputs: vec![TemplateNode::Torch, TemplateNode::Torch],
                    internal_edges: vec![],
                    output: Some(TemplateNode::Torch),
                    embedding_hints: vec![],
                },
            }],
        ),
    ]));
    library
        .register_comparator_subtract(
            LibraryEntryId {
                kind: GateKind::AndNot,
                ordinal: 0,
            },
            ComparatorSubtractContract {
                allow_inverter_side: composed,
                comparator: TemplateNode::Torch,
                rear_input: 0,
                side_input: 1,
            },
        )
        .unwrap();
    if composed {
        library
            .register_comparator_inverter(
                LibraryEntryId {
                    kind: GateKind::Nor(1),
                    ordinal: 0,
                },
                crate::compile::topology::ComparatorInverterContract {
                    second_side_input: None,
                    comparator: TemplateNode::SecondTorch,
                    rear_constant: TemplateNode::Torch,
                    side_input: 0,
                },
            )
            .unwrap();
    }
    library
}

fn positive_product(words: [u8; 4]) -> Option<u8> {
    let rows = program_rows(words);
    if rows.len() != 1 {
        return None;
    }
    let (_, bits, mask, outputs) = rows[0];
    (mask.count_ones() == 2 && bits & mask == mask).then_some(outputs)
}

fn negative_product(words: [u8; 4]) -> Option<u8> {
    let rows = program_rows(words);
    if rows.len() != 1 {
        return None;
    }
    let (_, bits, mask, outputs) = rows[0];
    (mask.count_ones() == 2 && bits & mask == 0).then_some(outputs)
}

fn nor_library(literals: bool) -> crate::compile::topology::Library {
    use crate::compile::topology::{
        ComparatorInverterContract, Library, LibraryEntry, LibraryEntryId, Template, TemplateNode,
    };
    let default = super::typed_matrix_library();
    let copy = |kind| {
        let e = default.entry_at(kind, 0).unwrap();
        LibraryEntry {
            name: e.name,
            template: Template {
                nodes: e.template.nodes.clone(),
                inputs: e.template.inputs.clone(),
                internal_edges: e.template.internal_edges.clone(),
                output: e.template.output,
                embedding_hints: e.template.embedding_hints.clone(),
            },
        }
    };
    let unary = comparator_library();
    let e = unary.entry_at(GateKind::Nor(1), 0).unwrap();
    let mut library = Library::new(BTreeMap::from([
        (GateKind::Nor(0), vec![copy(GateKind::Nor(0))]),
        (
            GateKind::Nor(1),
            if literals {
                vec![
                    LibraryEntry {
                        name: e.name,
                        template: Template {
                            nodes: e.template.nodes.clone(),
                            inputs: e.template.inputs.clone(),
                            internal_edges: e.template.internal_edges.clone(),
                            output: e.template.output,
                            embedding_hints: e.template.embedding_hints.clone(),
                        },
                    },
                    copy(GateKind::Nor(1)),
                ]
            } else {
                vec![copy(GateKind::Nor(1))]
            },
        ),
        (
            GateKind::Nor(2),
            vec![LibraryEntry {
                name: "two-side comparator NOR",
                template: Template {
                    nodes: e.template.nodes.clone(),
                    inputs: vec![TemplateNode::SecondTorch; 2],
                    internal_edges: e.template.internal_edges.clone(),
                    output: e.template.output,
                    embedding_hints: vec![],
                },
            }],
        ),
    ]));
    library
        .register_comparator_inverter(
            LibraryEntryId {
                kind: GateKind::Nor(2),
                ordinal: 0,
            },
            ComparatorInverterContract {
                comparator: TemplateNode::SecondTorch,
                rear_constant: TemplateNode::Torch,
                side_input: 0,
                second_side_input: Some(1),
            },
        )
        .unwrap();
    if literals {
        library
            .register_comparator_inverter(
                LibraryEntryId {
                    kind: GateKind::Nor(1),
                    ordinal: 0,
                },
                ComparatorInverterContract {
                    comparator: TemplateNode::SecondTorch,
                    rear_constant: TemplateNode::Torch,
                    side_input: 0,
                    second_side_input: None,
                },
            )
            .unwrap();
    }
    library
}

pub(super) fn library_for(words: [u8; 4]) -> crate::compile::topology::Library {
    if independent_mixed_product(&program_rows(words)).is_some() {
        subtract_library(true)
    } else if polarity_sum_conditions(words).is_some_and(|(polarity, _)| polarity != 3) {
        comparator_library()
    } else if let Some((bits, _)) = mixed_product_literals(words) {
        if bits == 0 {
            nor_library(true)
        } else {
            subtract_library(true)
        }
    } else if negative_product(words).is_some() {
        nor_library(false)
    } else if positive_product(words).is_some() {
        subtract_library(true)
    } else if subtract_cube(words).is_some() {
        subtract_library(false)
    } else if disjoint_literal_inputs(words).is_some() {
        comparator_library()
    } else {
        super::typed_matrix_library()
    }
}

fn comparator_library() -> crate::compile::topology::Library {
    use crate::compile::topology::{
        ComparatorInverterContract, Library, LibraryEntry, LibraryEntryId, Primitive, Template,
        TemplateNode,
    };
    let default = super::typed_matrix_library();
    let copy_entry = |kind| {
        let e = default.entry_at(kind, 0).unwrap();
        LibraryEntry {
            name: e.name,
            template: Template {
                nodes: e.template.nodes.clone(),
                internal_edges: e.template.internal_edges.clone(),
                inputs: e.template.inputs.clone(),
                output: e.template.output,
                embedding_hints: e.template.embedding_hints.clone(),
            },
        }
    };
    let mut library = Library::new(BTreeMap::from([
        (
            GateKind::Nor(1),
            vec![
                LibraryEntry {
                    name: "comparator inverter experiment",
                    template: Template {
                        nodes: vec![
                            (TemplateNode::Torch, Primitive::Torch),
                            (TemplateNode::SecondTorch, Primitive::Comparator),
                        ],
                        internal_edges: vec![(TemplateNode::Torch, TemplateNode::SecondTorch)],
                        inputs: vec![TemplateNode::SecondTorch],
                        output: Some(TemplateNode::SecondTorch),
                        embedding_hints: vec![],
                    },
                },
                copy_entry(GateKind::Nor(1)),
            ],
        ),
        (GateKind::Nor(0), vec![copy_entry(GateKind::Nor(0))]),
        (GateKind::Buf, vec![copy_entry(GateKind::Buf)]),
    ]));
    library
        .register_comparator_inverter(
            LibraryEntryId {
                kind: GateKind::Nor(1),
                ordinal: 0,
            },
            ComparatorInverterContract {
                second_side_input: None,
                comparator: TemplateNode::SecondTorch,
                rear_constant: TemplateNode::Torch,
                side_input: 0,
            },
        )
        .unwrap();
    library
}
fn shared_negative_candidate(
    words: [u8; 4],
    clusters: &[(usize, u8, bool)],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    let used: Vec<_> = clusters
        .iter()
        .map(|c| c.0)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let dual = used.len() != clusters.len();
    let base = |index: usize, bit: usize| {
        if dual {
            7 + 16 * used.iter().position(|b| *b == bit).unwrap() as i32
        } else {
            3 + 8 * index as i32
        }
    };
    let constant_offset = if dual {
        16 * used.len() as i32 - 1
    } else {
        8 * (clusters.len() as i32 - 1)
    };
    let mut w = World::new(16 + constant_offset, 6, 12);
    let mut inputs = [Position::new(14 + constant_offset, 2, 11); 2];
    for (index, &(bit, _, _)) in clusters.iter().enumerate() {
        inputs[bit] = Position::new(base(index, bit), 2, 2);
    }
    for p in inputs {
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lever);
    }
    let high = words.into_iter().fold(7, |a, b| a & b);
    let mut lamps = Vec::new();
    let mut outputs = Vec::new();
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    for (index, &(bit, variable, positive)) in clusters.iter().enumerate() {
        let x = base(index, bit);
        let cmp = if dual && positive {
            Position::new(x - 2, 3, 3)
        } else {
            Position::new(x, 1, 3)
        };
        put(&mut w, cmp.down(), BlockKind::Solid);
        if positive {
            wire(&mut w, Position::new(x - 1, 2, 2));
            wire(&mut w, Position::new(x - 1, if dual { 3 } else { 1 }, 3));
            put(&mut w, cmp, BlockKind::Repeater);
            let mut r = w.get(cmp.x, cmp.y, cmp.z).clone();
            r.facing = Some(if dual { Facing::East } else { Facing::West });
            w.set(cmp.x, cmp.y, cmp.z, r);
            wire(
                &mut w,
                Position::new(if dual { x - 3 } else { x + 1 }, cmp.y, 3),
            );
        } else {
            let mut c = BlockState::air();
            c.kind = BlockKind::Comparator;
            c.name = "minecraft:comparator".into();
            c.facing = Some(Facing::East);
            c.extra_properties.insert("mode".into(), "subtract".into());
            w.set(x, 1, 3, c);
            put(&mut w, Position::new(x + 2, 1, 3), BlockKind::Solid);
            put(&mut w, Position::new(x + 1, 1, 3), BlockKind::WallTorch);
            let mut t = w.get(x + 1, 1, 3).clone();
            t.facing = Some(Facing::West);
            w.set(x + 1, 1, 3, t);
        }
        let last = (0..3).filter(|b| variable & (1 << b) != 0).max().unwrap();
        let y = if dual && positive { 3 } else { 1 };
        let rail = if positive {
            if dual {
                x - 4
            } else {
                x + 2
            }
        } else {
            x - 1
        };
        for z in 3..=3 + 3 * last {
            wire(&mut w, Position::new(rail, y, z));
        }
        let name = format!("shared_condition{index}");
        gates.push(if positive {
            Gate {
                name: name.clone(),
                output: name.clone(),
                inputs: vec![["a", "b"][bit].into()],
                kind: GateKind::Buf,
            }
        } else {
            Gate::nor(&name, &[["a", "b"][bit]])
        });
        positions.insert(name, cmp);
    }
    for output in 0..3 {
        let z = 3 + 3 * output;
        let (lamp, name) = if let Some(index) = clusters
            .iter()
            .position(|(_, variable, _)| variable & (1 << output) != 0)
        {
            let x = base(index, clusters[index].0);
            let positive = clusters[index].2;
            let y = if dual && positive { 3 } else { 1 };
            let spur = if positive {
                if dual {
                    x - 5
                } else {
                    x + 3
                }
            } else {
                x - 2
            };
            let lamp_x = if positive {
                if dual {
                    x - 6
                } else {
                    x + 4
                }
            } else {
                x - 3
            };
            wire(&mut w, Position::new(spur, y, z));
            (
                Position::new(lamp_x, y, z),
                format!("shared_condition{index}"),
            )
        } else {
            let hi = format!("constant_high{output}");
            let is_high = high & (1 << output) != 0;
            let at = Position::new(12 + constant_offset, if is_high { 2 } else { 4 }, z);
            put(
                &mut w,
                Position::new(13 + constant_offset, at.y, z),
                BlockKind::Solid,
            );
            put(&mut w, at, BlockKind::WallTorch);
            let mut t = w.get(at.x, at.y, at.z).clone();
            t.facing = Some(Facing::West);
            w.set(at.x, at.y, at.z, t);
            gates.push(Gate::nor(&hi, &[]));
            positions.insert(hi.clone(), at);
            let name = if is_high {
                hi
            } else {
                wire(&mut w, Position::new(12 + constant_offset, 3, z));
                put(
                    &mut w,
                    Position::new(11 + constant_offset, 2, z),
                    BlockKind::WallTorch,
                );
                let mut t = w.get(11 + constant_offset, 2, z).clone();
                t.facing = Some(Facing::West);
                w.set(11 + constant_offset, 2, z, t);
                let lo = format!("constant_low{output}");
                gates.push(Gate::nor(&lo, &[&hi]));
                positions.insert(lo.clone(), Position::new(11 + constant_offset, 2, z));
                lo
            };
            for x in 9 + constant_offset..=constant_offset + if is_high { 12 } else { 11 } {
                wire(&mut w, Position::new(x, 1, z));
            }
            (Position::new(8 + constant_offset, 1, z), name)
        };
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        lamps.push(lamp);
        outputs.push(name);
    }
    let netlist = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs,
        gates,
    };
    super::typed_world_candidate_with_library(
        netlist,
        w,
        inputs.to_vec(),
        lamps,
        positions,
        true,
        &comparator_library(),
    )
}

enum ProductImplementation {
    Subtract { positive: usize },
    And,
    Nor,
}

// Reuse the ordinary high/low torch construction in a separate output domain.
pub(super) fn constant_output(
    w: &mut World,
    gates: &mut Vec<Gate>,
    positions: &mut BTreeMap<String, Position>,
    output: usize,
    is_high: bool,
    constant_offset: i32,
    z: i32,
) -> (Position, String) {
    let hi = format!("constant_high{output}");
    let at = Position::new(12 + constant_offset, if is_high { 2 } else { 4 }, z);
    put(
        w,
        Position::new(13 + constant_offset, at.y, z),
        BlockKind::Solid,
    );
    put(w, at, BlockKind::WallTorch);
    let mut t = w.get(at.x, at.y, at.z).clone();
    t.facing = Some(Facing::West);
    w.set(at.x, at.y, at.z, t);
    gates.push(Gate::nor(&hi, &[]));
    positions.insert(hi.clone(), at);
    let name = if is_high {
        hi
    } else {
        wire(w, Position::new(12 + constant_offset, 3, z));
        put(
            w,
            Position::new(11 + constant_offset, 2, z),
            BlockKind::WallTorch,
        );
        let mut t = w.get(11 + constant_offset, 2, z).clone();
        t.facing = Some(Facing::West);
        w.set(11 + constant_offset, 2, z, t);
        let lo = format!("constant_low{output}");
        gates.push(Gate::nor(&lo, &[&hi]));
        positions.insert(lo.clone(), Position::new(11 + constant_offset, 2, z));
        lo
    };
    for x in 9 + constant_offset..=constant_offset + if is_high { 12 } else { 11 } {
        wire(w, Position::new(x, 1, z));
    }
    (Position::new(8 + constant_offset, 1, z), name)
}

// Product, negative literal, and constant output dependencies are recognised
// from the cover. Repeated conditions share a producer, not a public terminal.
fn mixed_product_literals(words: [u8; 4]) -> Option<(u8, [usize; 3])> {
    let rows = program_rows(words);
    let mut conditions = [0; 3];
    let mut product_bits = None;
    for (output, condition) in conditions.iter_mut().enumerate() {
        let mut terms = rows.iter().filter(|r| r.3 & (1 << output) != 0);
        let Some(&(_, bits, mask, _)) = terms.next() else {
            *condition = if words.iter().all(|w| w & (1 << output) != 0) {
                4
            } else {
                3
            };
            continue;
        };
        if terms.next().is_some() {
            return None;
        }
        *condition = if mask == 3 {
            if product_bits.is_some_and(|prior| prior != bits & mask) {
                return None;
            }
            product_bits = Some(bits & mask);
            0
        } else if mask.count_ones() == 1 && bits & mask == 0 {
            1 + mask.trailing_zeros() as usize
        } else {
            return None;
        };
    }
    if !conditions.contains(&0) || !conditions.iter().any(|c| matches!(c, 1 | 2)) {
        return None;
    }
    Some((product_bits?, conditions))
}

fn mixed_nor_candidate(
    conditions: [usize; 3],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    let mut w = World::new(36, 6, 16);
    let inputs = [Position::new(11, 2, 7), Position::new(11, 2, 5)];
    for p in inputs {
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lever);
    }
    let names = ["shared_product", "negative_a", "negative_b"];
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    for (condition, z) in [(0, 6), (1, 8), (2, 4)] {
        if condition != 0 && !conditions.contains(&condition) {
            continue;
        }
        let p = Position::new(11, 1, z);
        put(&mut w, p.down(), BlockKind::Solid);
        let mut c = BlockState::air();
        c.kind = BlockKind::Comparator;
        c.name = "minecraft:comparator".into();
        c.facing = Some(Facing::East);
        c.extra_properties.insert("mode".into(), "subtract".into());
        w.set(p.x, p.y, p.z, c);
        put(&mut w, Position::new(13, 1, z), BlockKind::Solid);
        let torch = Position::new(12, 1, z);
        put(&mut w, torch, BlockKind::WallTorch);
        let mut t = w.get(torch.x, torch.y, torch.z).clone();
        t.facing = Some(Facing::West);
        w.set(torch.x, torch.y, torch.z, t);
        gates.push(if condition == 0 {
            Gate::nor(names[condition], &["a", "b"])
        } else {
            Gate::nor(names[condition], &[["a", "b"][condition - 1]])
        });
        positions.insert(names[condition].into(), p);
        for x in (if condition == 0 { 0 } else { 6 })..=10 {
            wire(&mut w, Position::new(x, 1, z));
        }
    }
    let mut lamps = Vec::new();
    let mut outputs = Vec::new();
    for (output, condition) in conditions.into_iter().enumerate() {
        let (lamp, name) = if condition >= 3 {
            constant_output(
                &mut w,
                &mut gates,
                &mut positions,
                output,
                condition == 4,
                16,
                6 + 3 * output as i32,
            )
        } else {
            let (x, z, lamp_z) = match condition {
                0 => (2 * output as i32, 5, 4),
                1 => (6 + 2 * output as i32, 9, 10),
                _ => (6 + 2 * output as i32, 3, 2),
            };
            wire(&mut w, Position::new(x, 1, z));
            (Position::new(x, 1, lamp_z), names[condition].to_string())
        };
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        lamps.push(lamp);
        outputs.push(name);
    }
    let netlist = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs,
        gates,
    };
    super::typed_world_candidate_with_library(
        netlist,
        w,
        inputs.to_vec(),
        lamps,
        positions,
        true,
        &nor_library(true),
    )
}

fn mixed_product_candidate(
    product_bits: u8,
    conditions: [usize; 3],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    if product_bits == 0 {
        return mixed_nor_candidate(conditions);
    }
    let mut w = World::new(36, 6, 16);
    let composed = product_bits == 3;
    let positive = if composed {
        0
    } else {
        product_bits.trailing_zeros() as usize
    };
    let positive_condition = positive + 1;
    let other_condition = 2 - positive;
    let mut inputs = [if composed {
        Position::new(7, 2, 4)
    } else {
        Position::new(8, 2, 5)
    }; 2];
    inputs[positive] = Position::new(9, 2, 6);
    for p in inputs {
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lever);
    }
    let mut comparator = |p: Position, facing: Facing| {
        put(&mut w, p.down(), BlockKind::Solid);
        let mut c = BlockState::air();
        c.kind = BlockKind::Comparator;
        c.name = "minecraft:comparator".into();
        c.facing = Some(facing);
        c.extra_properties.insert("mode".into(), "subtract".into());
        w.set(p.x, p.y, p.z, c);
    };
    if composed {
        comparator(Position::new(8, 1, 4), Facing::North);
    }
    comparator(Position::new(8, 1, 6), Facing::East);
    let names = ["shared_product", "negative_a", "negative_b"];
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    if composed {
        gates.push(Gate::nor("side_inverter", &[["a", "b"][1 - positive]]));
        positions.insert("side_inverter".into(), Position::new(8, 1, 4));
    }
    gates.push(Gate {
        name: names[0].into(),
        output: names[0].into(),
        kind: GateKind::AndNot,
        inputs: vec![
            ["a", "b"][positive].into(),
            if composed {
                "side_inverter".into()
            } else {
                ["a", "b"][1 - positive].into()
            },
        ],
    });
    positions.insert(names[0].into(), Position::new(8, 1, 6));
    for (index, p, facing) in [
        (positive_condition, Position::new(10, 1, 6), Facing::South),
        (
            other_condition,
            if composed {
                Position::new(6, 1, 4)
            } else {
                Position::new(8, 1, 4)
            },
            if composed {
                Facing::North
            } else {
                Facing::West
            },
        ),
    ] {
        if conditions.contains(&index) {
            comparator(p, facing);
            gates.push(Gate::nor(names[index], &[["a", "b"][index - 1]]));
            positions.insert(names[index].into(), p);
        }
    }
    for (index, p, support, facing) in [
        (
            0,
            Position::new(8, 1, 3),
            Position::new(8, 1, 2),
            Facing::South,
        ),
        (
            positive_condition,
            Position::new(10, 1, 7),
            Position::new(10, 1, 8),
            Facing::North,
        ),
        (
            other_condition,
            if composed {
                Position::new(6, 1, 3)
            } else {
                Position::new(7, 1, 4)
            },
            if composed {
                Position::new(6, 1, 2)
            } else {
                Position::new(6, 1, 4)
            },
            if composed {
                Facing::South
            } else {
                Facing::East
            },
        ),
    ] {
        if (index == 0 && composed) || (index != 0 && conditions.contains(&index)) {
            put(&mut w, support, BlockKind::Solid);
            put(&mut w, p, BlockKind::WallTorch);
            let mut t = w.get(p.x, p.y, p.z).clone();
            t.facing = Some(facing);
            w.set(p.x, p.y, p.z, t);
        }
    }
    put(&mut w, Position::new(8, 1, 5), BlockKind::Solid);
    for z in 6..=8 {
        wire(&mut w, Position::new(7, 1, z));
    }
    for x in 3..=6 {
        wire(&mut w, Position::new(x, 1, 8));
    }
    if conditions.contains(&positive_condition) {
        for x in 10..=16 {
            wire(&mut w, Position::new(x, 1, 5));
        }
    }
    if conditions.contains(&other_condition) {
        if composed {
            for x in 0..=6 {
                wire(&mut w, Position::new(x, 1, 5));
            }
        } else {
            wire(&mut w, Position::new(9, 1, 4));
            for x in 9..=14 {
                wire(&mut w, Position::new(x, 1, 3));
            }
        }
    }
    // Aliases share the producer but retain distinct public terminals.
    let mut lamp_sites = [Position::new(0, 0, 0); 3];
    let mut outputs = Vec::new();
    for (output, condition) in conditions.into_iter().enumerate() {
        let (lamp, name) = if condition >= 3 {
            constant_output(
                &mut w,
                &mut gates,
                &mut positions,
                output,
                condition == 4,
                16,
                6 + 3 * output as i32,
            )
        } else {
            let (x, z, lamp_z) = if condition == 0 {
                (3 + 2 * output as i32, 9, 10)
            } else if condition == positive_condition {
                (
                    12 + 2 * output as i32,
                    if composed { 4 } else { 6 },
                    if composed { 3 } else { 7 },
                )
            } else if composed {
                (2 * output as i32, 4, 3)
            } else {
                (10 + 2 * output as i32, 2, 1)
            };
            wire(&mut w, Position::new(x, 1, z));
            (Position::new(x, 1, lamp_z), names[condition].to_string())
        };
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        lamp_sites[output] = lamp;
        outputs.push(name);
    }
    let netlist = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs,
        gates,
    };
    super::typed_world_candidate_with_library(
        netlist,
        w,
        inputs.to_vec(),
        lamp_sites.to_vec(),
        positions,
        true,
        &subtract_library(true),
    )
}

fn product_candidate(
    words: [u8; 4],
    variable: u8,
    implementation: ProductImplementation,
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    let positive = match implementation {
        ProductImplementation::Subtract { positive } => positive,
        _ => 0,
    };
    let composed = matches!(implementation, ProductImplementation::And);
    let nor = matches!(implementation, ProductImplementation::Nor);
    let mut w = World::new(32, 6, 16);
    let mut inputs = [if composed {
        Position::new(7, 2, 4)
    } else {
        Position::new(8, 2, 5)
    }; 2];
    inputs[positive] = if nor {
        Position::new(8, 2, 7)
    } else {
        Position::new(9, 2, 6)
    };
    for p in inputs {
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lever);
    }
    let cmp = Position::new(8, 1, 6);
    put(&mut w, cmp.down(), BlockKind::Solid);
    let mut c = BlockState::air();
    c.kind = BlockKind::Comparator;
    c.name = "minecraft:comparator".into();
    c.facing = Some(Facing::East);
    c.extra_properties.insert("mode".into(), "subtract".into());
    w.set(cmp.x, cmp.y, cmp.z, c);
    wire(&mut w, Position::new(7, 1, 6));
    let last_z = 6 + 3 * (0..3).rev().find(|bit| variable & (1 << bit) != 0).unwrap();
    for z in 6..=last_z {
        wire(&mut w, Position::new(6, 1, z));
    }
    let name = "shared_product".to_string();
    let mut gates = vec![Gate {
        name: name.clone(),
        output: name.clone(),
        kind: GateKind::AndNot,
        inputs: vec![["a", "b"][positive].into(), ["a", "b"][1 - positive].into()],
    }];
    let mut positions = BTreeMap::from([(name.clone(), cmp)]);
    if nor {
        gates[0].kind = GateKind::Nor(2);
        put(&mut w, Position::new(10, 1, 6), BlockKind::Solid);
        put(&mut w, Position::new(9, 1, 6), BlockKind::WallTorch);
        let mut t = w.get(9, 1, 6).clone();
        t.facing = Some(Facing::West);
        w.set(9, 1, 6, t);
    }
    if composed {
        let inverter = "side_inverter".to_string();
        gates[0].inputs[1] = inverter.clone();
        gates.insert(0, Gate::nor(&inverter, &[["a", "b"][1 - positive]]));
        let at = Position::new(8, 1, 4);
        positions.insert(inverter, at);
        let mut c = w.get(cmp.x, cmp.y, cmp.z).clone();
        c.facing = Some(Facing::North);
        w.set(at.x, at.y, at.z, c);
        put(&mut w, at.down(), BlockKind::Solid);
        put(&mut w, Position::new(8, 1, 5), BlockKind::Solid);
        put(&mut w, Position::new(8, 1, 2), BlockKind::Solid);
        put(&mut w, Position::new(8, 1, 3), BlockKind::WallTorch);
        let mut t = w.get(8, 1, 3).clone();
        t.facing = Some(Facing::South);
        w.set(8, 1, 3, t);
    }
    let mut lamps = Vec::new();
    let mut outputs = Vec::new();
    let high = words.into_iter().fold(7, |a, b| a & b);
    let constant_offset = 8;
    for output in 0..3 {
        let z = 6 + 3 * output;
        let (lamp, name) = if variable & (1 << output) != 0 {
            wire(&mut w, Position::new(5, 1, z));
            (Position::new(4, 1, z), name.clone())
        } else {
            constant_output(
                &mut w,
                &mut gates,
                &mut positions,
                output as usize,
                high & (1 << output) != 0,
                constant_offset,
                z,
            )
        };
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        lamps.push(lamp);
        outputs.push(name);
    }
    let netlist = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs,
        gates,
    };
    super::typed_world_candidate_with_library(
        netlist,
        w,
        inputs.to_vec(),
        lamps,
        positions,
        true,
        &if nor {
            nor_library(false)
        } else {
            subtract_library(composed)
        },
    )
}

// Match a sum whose input polarities are shared by its single-literal outputs.
// Keep sibling outputs on the input side of the sum's isolation.
// Keep OR restoration independent of unary polarity and output alias count.
fn positive_sum_with_opposite_literals(words: [u8; 4]) -> Option<[usize; 3]> {
    let rows = program_rows(words);
    let mut conditions = [3; 3];
    for (bit, condition) in conditions.iter_mut().enumerate() {
        let terms: Vec<_> = rows.iter().filter(|r| r.3 & (1 << bit) != 0).collect();
        *condition = match terms.as_slice() {
            [] => {
                if words.iter().all(|w| w & (1 << bit) != 0) {
                    4
                } else {
                    3
                }
            }
            [term] if term.2.count_ones() == 1 => {
                let input = term.2.trailing_zeros() as usize;
                if positive_literal(term.1, term.2) {
                    1 + input
                } else {
                    5 + input
                }
            }
            [a, b] if positive_literal(a.1, a.2) && positive_literal(b.1, b.2) => 0,
            _ => return None,
        };
    }
    (conditions.contains(&0) && conditions.iter().any(|&c| c >= 5)).then_some(conditions)
}

fn polarity_sum_conditions(words: [u8; 4]) -> Option<(u8, [usize; 3])> {
    let rows = program_rows(words);
    let mut conditions = [3; 3];
    let mut polarities = [None; 2];
    for (output, condition) in conditions.iter_mut().enumerate() {
        let terms: Vec<_> = rows.iter().filter(|r| r.3 & (1 << output) != 0).collect();
        if terms.is_empty() {
            *condition = if words.iter().all(|w| w & (1 << output) != 0) {
                4
            } else {
                3
            };
            continue;
        }
        if terms.len() > 2 {
            return None;
        }
        for term in &terms {
            if term.2.count_ones() != 1 {
                return None;
            }
            let bit = term.2.trailing_zeros() as usize;
            let positive = term.1 & term.2 != 0;
            if polarities[bit].is_some_and(|prior| prior != positive) {
                return None;
            }
            polarities[bit] = Some(positive);
        }
        *condition = if terms.len() == 2 {
            0
        } else {
            1 + terms[0].2.trailing_zeros() as usize
        };
    }
    if !conditions.contains(&0) {
        return None;
    }
    Some((
        u8::from(polarities[0]?) | (u8::from(polarities[1]?) << 1),
        conditions,
    ))
}

#[cfg(test)]
fn positive_sum_conditions(words: [u8; 4]) -> Option<[usize; 3]> {
    polarity_sum_conditions(words)
        .and_then(|(polarity, conditions)| (polarity == 3).then_some(conditions))
}

fn polarity_sum_candidate(
    polarity: u8,
    conditions: [usize; 3],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    if polarity == 3 {
        return positive_sum_candidate(conditions);
    }
    let mut w = World::new(36, 6, 16);
    let inputs = std::array::from_fn::<_, 2, _>(|bit| {
        let z = 4 + 4 * bit as i32;
        if polarity & (1 << bit) != 0 {
            Position::new(12, 2, z)
        } else {
            Position::new(11, 3, z + 1)
        }
    });
    let names = ["shared_sum", "literal_a", "literal_b"];
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    for (bit, p) in inputs.into_iter().enumerate() {
        let z = 4 + 4 * bit as i32;
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lever);
        let producer = Position::new(11, 2, z);
        put(&mut w, producer.down(), BlockKind::Solid);
        if polarity & (1 << bit) != 0 {
            put(&mut w, producer, BlockKind::Repeater);
            gates.push(Gate {
                name: names[bit + 1].into(),
                output: names[bit + 1].into(),
                kind: GateKind::Buf,
                inputs: vec![["a", "b"][bit].into()],
            });
        } else {
            let mut c = BlockState::air();
            c.kind = BlockKind::Comparator;
            c.name = "minecraft:comparator".into();
            c.extra_properties.insert("mode".into(), "subtract".into());
            w.set(producer.x, producer.y, producer.z, c);
            put(&mut w, Position::new(13, 2, z), BlockKind::Solid);
            let torch = Position::new(12, 2, z);
            put(&mut w, torch, BlockKind::WallTorch);
            let mut t = w.get(torch.x, torch.y, torch.z).clone();
            t.facing = Some(Facing::West);
            w.set(torch.x, torch.y, torch.z, t);
            gates.push(Gate::nor(names[bit + 1], &[["a", "b"][bit]]));
        }
        let mut c = w.get(producer.x, producer.y, producer.z).clone();
        c.facing = Some(Facing::East);
        w.set(producer.x, producer.y, producer.z, c);
        positions.insert(names[bit + 1].into(), producer);
        for x in 3..=6 {
            wire(&mut w, Position::new(x, 1, z));
        }
        for x in 8..=10 {
            wire(&mut w, Position::new(x, 2, z));
        }
        let isolation = Position::new(7, 1, z);
        put(&mut w, isolation.down(), BlockKind::Solid);
        put(&mut w, isolation, BlockKind::Repeater);
        let mut r = w.get(isolation.x, isolation.y, isolation.z).clone();
        r.facing = Some(Facing::East);
        w.set(isolation.x, isolation.y, isolation.z, r);
        let name = format!("sum_input{bit}");
        gates.push(Gate {
            name: name.clone(),
            output: name.clone(),
            kind: GateKind::Buf,
            inputs: vec![names[bit + 1].into()],
        });
        positions.insert(name, isolation);
    }
    for z in 5..=7 {
        wire(&mut w, Position::new(3, 1, z));
    }
    gates.push(Gate::merge(names[0], &["sum_input0", "sum_input1"]));
    positions.insert(names[0].into(), Position::new(3, 1, 6));
    let mut lamps = Vec::new();
    let mut outputs = Vec::new();
    let mut literal_aliases = [0; 2];
    for (output, condition) in conditions.into_iter().enumerate() {
        let (lamp, name) = if condition >= 3 {
            constant_output(
                &mut w,
                &mut gates,
                &mut positions,
                output,
                condition == 4,
                16,
                6 + 3 * output as i32,
            )
        } else {
            let (x, z, lamp_x, lamp_z) = if condition == 0 {
                (2, 4 + 2 * output as i32, 1, 4 + 2 * output as i32)
            } else {
                let bit = condition - 1;
                let x = 8 + 2 * literal_aliases[bit];
                literal_aliases[bit] += 1;
                (
                    x,
                    if bit == 0 { 3 } else { 7 },
                    x,
                    if bit == 0 { 2 } else { 6 },
                )
            };
            let y = if condition == 0 { 1 } else { 2 };
            wire(&mut w, Position::new(x, y, z));
            (
                Position::new(lamp_x, y, lamp_z),
                names[condition].to_string(),
            )
        };
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        lamps.push(lamp);
        outputs.push(name);
    }
    let netlist = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs,
        gates,
    };
    super::typed_world_candidate_with_library(
        netlist,
        w,
        inputs.to_vec(),
        lamps,
        positions,
        true,
        &comparator_library(),
    )
}

fn positive_sum_candidate(
    conditions: [usize; 3],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    let inverse_counts =
        std::array::from_fn::<_, 2, _>(|bit| conditions.iter().filter(|&&c| c == 5 + bit).count());
    let z_shift = if inverse_counts.iter().any(|&count| count > 1) {
        2
    } else {
        0
    };
    let mut w = World::new(36, 6, 16 + z_shift);
    let y = if conditions.iter().any(|&c| c >= 5) {
        2
    } else {
        1
    };
    let inputs = [
        Position::new(9, y, 4 + z_shift),
        Position::new(9, y, 8 + z_shift),
    ];
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    let names = ["shared_sum", "positive_a", "positive_b"];
    for (bit, p) in inputs.into_iter().enumerate() {
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lever);
        for (name, x, facing) in [
            (format!("sum_input{bit}"), 8, Facing::East),
            (names[bit + 1].to_string(), 10, Facing::West),
        ] {
            if x == 10 && !conditions.contains(&(bit + 1)) {
                continue;
            }
            let r = Position::new(x, y, p.z);
            put(&mut w, r.down(), BlockKind::Solid);
            put(&mut w, r, BlockKind::Repeater);
            let mut b = w.get(r.x, r.y, r.z).clone();
            b.facing = Some(facing);
            w.set(r.x, r.y, r.z, b);
            gates.push(Gate {
                name: name.clone(),
                output: name.clone(),
                kind: GateKind::Buf,
                inputs: vec![["a", "b"][bit].into()],
            });
            positions.insert(name, r);
        }
        for x in 3..=7 {
            wire(&mut w, Position::new(x, y, p.z));
        }
        if conditions.contains(&(bit + 1)) {
            for x in 11..=16 {
                wire(&mut w, Position::new(x, y, p.z));
            }
        }
    }
    for z in (5 + z_shift)..=(7 + z_shift) {
        wire(&mut w, Position::new(3, y, z));
    }
    gates.push(Gate::merge(names[0], &["sum_input0", "sum_input1"]));
    positions.insert(names[0].into(), Position::new(3, y, 6 + z_shift));
    let mut lamps = Vec::new();
    let mut outputs = Vec::new();
    let mut inverse_aliases = [0; 2];
    for (output, condition) in conditions.into_iter().enumerate() {
        let (lamp, name) = if condition >= 5 {
            let bit = condition - 5;
            let direction = if bit == 0 { -1 } else { 1 };
            let source = inputs[bit];
            let name = format!("negative_literal{bit}");
            if inverse_aliases[bit] == 0 {
                // Preserve the unary restoration stage under continuous torch history;
                // only the shared OR path loses its redundant second stage.
                let restore = Position::new(source.x, y, source.z + direction);
                put(&mut w, restore.down(), BlockKind::Solid);
                put(&mut w, restore, BlockKind::Repeater);
                let mut state = w.get(restore.x, restore.y, restore.z).clone();
                state.facing = Some(if direction < 0 {
                    Facing::South
                } else {
                    Facing::North
                });
                w.set(restore.x, restore.y, restore.z, state);
                let restored = format!("negative_input{bit}");
                gates.push(Gate {
                    name: restored.clone(),
                    output: restored.clone(),
                    kind: GateKind::Buf,
                    inputs: vec![["a", "b"][bit].into()],
                });
                positions.insert(restored.clone(), restore);
                wire(&mut w, Position::new(source.x, y, source.z + 2 * direction));
                let torch = Position::new(source.x, y - 1, source.z + 3 * direction);
                put(&mut w, torch, BlockKind::WallTorch);
                let mut state = w.get(torch.x, torch.y, torch.z).clone();
                state.facing = Some(if direction < 0 {
                    Facing::North
                } else {
                    Facing::South
                });
                w.set(torch.x, torch.y, torch.z, state);
                gates.push(Gate::nor(&name, &[&restored]));
                positions.insert(name.clone(), torch);
                wire(
                    &mut w,
                    Position::new(source.x, y - 1, source.z + 4 * direction),
                );
                wire(
                    &mut w,
                    Position::new(source.x + 1, y - 1, source.z + 4 * direction),
                );
                if inverse_counts[bit] > 1 {
                    for step in 4..=6 {
                        wire(
                            &mut w,
                            Position::new(source.x + 1, y - 1, source.z + step * direction),
                        );
                    }
                    for step in [4, 6] {
                        for x in (source.x + 2)..=(source.x + 3) {
                            wire(&mut w, Position::new(x, y - 1, source.z + step * direction));
                        }
                    }
                }
            }
            let lamp = if inverse_counts[bit] > 1 {
                Position::new(
                    source.x + 4,
                    y - 1,
                    source.z + (4 + 2 * inverse_aliases[bit]) * direction,
                )
            } else {
                Position::new(source.x + 2, y - 1, source.z + 4 * direction)
            };
            inverse_aliases[bit] += 1;
            (lamp, name)
        } else if condition >= 3 {
            constant_output(
                &mut w,
                &mut gates,
                &mut positions,
                output,
                condition == 4,
                16,
                6 + 3 * output as i32 + z_shift,
            )
        } else {
            let (x, z, lamp_x, lamp_z) = match condition {
                0 => (
                    2,
                    4 + 2 * output as i32 + z_shift,
                    1,
                    4 + 2 * output as i32 + z_shift,
                ),
                1 => (
                    12 + 2 * output as i32,
                    3 + z_shift,
                    12 + 2 * output as i32,
                    2 + z_shift,
                ),
                _ => (
                    12 + 2 * output as i32,
                    9 + z_shift,
                    12 + 2 * output as i32,
                    10 + z_shift,
                ),
            };
            wire(&mut w, Position::new(x, y, z));
            (
                Position::new(lamp_x, y, lamp_z),
                names[condition].to_string(),
            )
        };
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        lamps.push(lamp);
        outputs.push(name);
    }
    let netlist = Netlist {
        inputs: vec!["a".into(), "b".into()],
        outputs,
        gates,
    };
    super::typed_world_candidate(netlist, w, inputs.to_vec(), lamps, positions, true)
}

fn build_candidate(
    words: [u8; 4],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    if let Some(conditions) = positive_sum_with_opposite_literals(words) {
        return positive_sum_candidate(conditions);
    }
    if let Some((polarity, conditions)) = polarity_sum_conditions(words) {
        return polarity_sum_candidate(polarity, conditions);
    }
    if let Some((bits, conditions)) = mixed_product_literals(words) {
        return mixed_product_candidate(bits, conditions);
    }
    if let Some(variable) = negative_product(words) {
        return product_candidate(words, variable, ProductImplementation::Nor);
    }
    if let Some(variable) = positive_product(words) {
        return product_candidate(words, variable, ProductImplementation::And);
    }
    if let Some((positive, variable)) = subtract_cube(words) {
        return product_candidate(
            words,
            variable,
            ProductImplementation::Subtract { positive },
        );
    }
    if let Some(clusters) = disjoint_literal_inputs(words) {
        return shared_negative_candidate(words, &clusters);
    }
    matrix_candidate(words)
}

// Keep generic routing fixtures independent of optional product selection.
fn matrix_library(words: [u8; 4]) -> crate::compile::topology::Library {
    if independent_mixed_product(&program_rows(words)).is_some() {
        subtract_library(true)
    } else {
        super::typed_matrix_library()
    }
}

fn matrix_candidate(
    words: [u8; 4],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    let (netlist, positions) = player_logic(words);
    let (world, inputs, _, lamps) = player_world(words);
    super::typed_world_candidate_with_library(
        netlist,
        world,
        inputs.to_vec(),
        lamps.to_vec(),
        positions,
        true,
        &matrix_library(words),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::{
        candidate::RepeaterHandoverKind,
        realise::{realise_and_verify_expanded, CertificationError},
    };
    use crate::compile::verification::ExpandedPhysicalError;

    #[test]
    fn shared_primary_support_rejects_other_terminal_contracts() {
        use crate::compile::fragment_synth::realise::ExpandedCandidateAdapter;
        let (_, _, candidate) = mixed_product_candidate(3, [0, 1, 2]);
        ExpandedCandidateAdapter::new(&candidate).unwrap();
        let route = candidate
            .routes
            .values()
            .find(|route| {
                route.branches.iter().any(|a| {
                    route
                        .branches
                        .iter()
                        .any(|b| a.sink != b.sink && a.terminal.at == b.terminal.at)
                })
            })
            .unwrap();
        let route_id = route.id;
        let sink = route.branches[0].sink;
        for mutation in 0..5 {
            let mut invalid = candidate.clone();
            let branch = invalid
                .routes
                .get_mut(&route_id)
                .unwrap()
                .branches
                .iter_mut()
                .find(|b| b.sink == sink)
                .unwrap();
            match mutation {
                0 => {
                    branch.terminal.kind =
                        crate::compile::planner::RouteTerminalKind::DirectedDustIntoSupport
                }
                1 => branch.terminal.repeaters = 1,
                2 => branch.path.push(branch.terminal.at),
                3 => branch.root.x += 1,
                _ => {
                    branch.target =
                        crate::compile::fragment_synth::candidate::RouteTarget::DeclaredOutput(
                            crate::compile::fragment_synth::identity::PortId(0),
                        )
                }
            }
            assert!(
                ExpandedCandidateAdapter::new(&invalid).is_err(),
                "invalid shared support {mutation}"
            );
        }
    }

    #[test]
    fn polarity_sums_certify_isolation_aliases_and_retained_history() {
        let mut checked = 0;
        for polarity in 0..=3u8 {
            for a in 0..5 {
                for b in 0..5 {
                    for c in 0..5 {
                        let conditions = [a, b, c];
                        if !conditions.contains(&0) {
                            continue;
                        }
                        let words = std::array::from_fn(|address| {
                            conditions
                                .iter()
                                .enumerate()
                                .fold(0, |word, (bit, condition)| {
                                    let value = match condition {
                                        0 => address ^ polarity as usize != 3,
                                        1 => (address & 1 != 0) == (polarity & 1 != 0),
                                        2 => (address & 2 != 0) == (polarity & 2 != 0),
                                        _ => *condition == 4,
                                    };
                                    word | if value { 1 << bit } else { 0 }
                                })
                        });
                        assert_eq!(polarity_sum_conditions(words), Some((polarity, conditions)));
                        if polarity == 3 {
                            assert_eq!(positive_sum_conditions(words), Some(conditions));
                        }
                        let (source, _) = player_logic(words);
                        let result = super::super::compile_player_torch_row(&source, 1_000_000)
                            .unwrap_or_else(|e| panic!("polarity sum {words:?}: {e:?}"));
                        let mut sim = crate::redstone::simulator::Simulator::new(
                            result.physical().world().clone(),
                        );
                        sim.run_until_stable(2000).unwrap();
                        for from in 0..4 {
                            for to in 0..4 {
                                for address in [from, to] {
                                    for (bit, &(x, y, z)) in
                                        result.source_input_positions().iter().enumerate()
                                    {
                                        let mut lever = sim.world().get(x, y, z).clone();
                                        lever.lit = address & (1 << bit) != 0;
                                        sim.world_mut().set(x, y, z, lever);
                                    }
                                    sim.run_until_stable(2000).unwrap();
                                    for (bit, &(x, y, z)) in
                                        result.source_output_positions().iter().enumerate()
                                    {
                                        assert_eq!(sim.world().get(x,y,z).lit, words[address] & (1 << bit) != 0,
                        "words={words:?}, transition={from}->{to}, address={address}, output={bit}");
                                    }
                                }
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 244);
        assert!(positive_sum_conditions([0, 1, 1, 2]).is_none());
    }

    #[test]
    fn positive_sums_with_opposite_literals_preserve_retained_history() {
        let mut checked = 0;
        for a in 0..7 {
            for b in 0..7 {
                for c in 0..7 {
                    let conditions = [a, b, c];
                    if !conditions.contains(&0) || !conditions.iter().any(|&c| c >= 5) {
                        continue;
                    }
                    let words = std::array::from_fn(|address| {
                        let values = [
                            address != 0,
                            address & 1 != 0,
                            address & 2 != 0,
                            false,
                            true,
                            address & 1 == 0,
                            address & 2 == 0,
                        ];
                        conditions
                            .iter()
                            .enumerate()
                            .fold(0, |word, (bit, &c)| word | (u8::from(values[c]) << bit))
                    });
                    assert_eq!(positive_sum_with_opposite_literals(words), Some(conditions));
                    let (source, _) = player_logic(words);
                    let rewrite = super::super::compile_player_torch_row(&source, 1_000_000)
                        .unwrap_or_else(|e| panic!("opposite sum {words:?}: {e:?}"));
                    let mut sim = crate::redstone::simulator::Simulator::new(
                        rewrite.physical().world().clone(),
                    );
                    sim.run_until_stable(2000).unwrap();
                    for from in 0..4 {
                        for to in 0..4 {
                            for address in [from, to] {
                                for (bit, &(x, y, z)) in
                                    rewrite.source_input_positions().iter().enumerate()
                                {
                                    let mut lever = sim.world().get(x, y, z).clone();
                                    lever.lit = address & (1 << bit) != 0;
                                    sim.world_mut().set(x, y, z, lever);
                                }
                                sim.run_until_stable(2000).unwrap();
                                for (bit, &(x, y, z)) in
                                    rewrite.source_output_positions().iter().enumerate()
                                {
                                    assert_eq!(sim.world().get(x, y, z).lit, words[address] & (1 << bit) != 0,
                                        "words={words:?}, transition={from}->{to}, address={address}, output={bit}");
                                }
                            }
                        }
                    }
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 66);
    }

    #[test]
    fn mixed_product_and_negative_literals_certify_shared_boundaries() {
        let mut checked = 0;
        for product_bits in 0..=3u8 {
            for a in 0..5 {
                for b in 0..5 {
                    for c in 0..5 {
                        let conditions = [a, b, c];
                        if !conditions.contains(&0)
                            || !conditions.iter().any(|i| matches!(i, 1 | 2))
                        {
                            continue;
                        }
                        let words = std::array::from_fn(|address| {
                            conditions
                                .iter()
                                .enumerate()
                                .fold(0, |word, (bit, condition)| {
                                    let value = match condition {
                                        0 => address == product_bits as usize,
                                        1 => address & 1 == 0,
                                        2 => address & 2 == 0,
                                        _ => *condition == 4,
                                    };
                                    word | if value { 1 << bit } else { 0 }
                                })
                        });
                        assert_eq!(
                            mixed_product_literals(words),
                            Some((product_bits, conditions))
                        );
                        let (source, _) = player_logic(words);
                        let result = super::super::compile_player_torch_row(&source, 1_000_000)
                            .unwrap_or_else(|e| panic!("mixed conditions {words:?}: {e:?}"));
                        let mut sim = crate::redstone::simulator::Simulator::new(
                            result.physical().world().clone(),
                        );
                        sim.run_until_stable(2000).unwrap();
                        for from in 0..4 {
                            for to in 0..4 {
                                for address in [from, to] {
                                    for (bit, &(x, y, z)) in
                                        result.source_input_positions().iter().enumerate()
                                    {
                                        let mut lever = sim.world().get(x, y, z).clone();
                                        lever.lit = address & (1 << bit) != 0;
                                        sim.world_mut().set(x, y, z, lever);
                                    }
                                    sim.run_until_stable(2000).unwrap();
                                    for (bit, &(x, y, z)) in
                                        result.source_output_positions().iter().enumerate()
                                    {
                                        assert_eq!(
                                        sim.world().get(x, y, z).lit,
                                        words[address] & (1 << bit) != 0,
                                        "words={words:?}, from={from}, to={to}, address={address}, output={bit}"
                                    );
                                    }
                                }
                            }
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 168);
        assert!(mixed_product_literals([0, 1, 2, 7]).is_none());
    }

    #[test]
    fn mixed_sign_cube_certifies_constants_fanout_and_retained_history() {
        let mut checked = 0;
        for positive in 0..2 {
            for variable in 1..8u8 {
                for high in 0..8u8 {
                    if high & variable != 0 {
                        continue;
                    }
                    let words = std::array::from_fn(|address| {
                        high | if address == 1 << positive {
                            variable
                        } else {
                            0
                        }
                    });
                    assert_eq!(subtract_cube(words), Some((positive, variable)));
                    let (source, _) = player_logic(words);
                    let result = super::super::compile_player_torch_row(&source, 1_000_000)
                        .unwrap_or_else(|e| panic!("mixed-sign {words:?}: {e:?}"));
                    let replay = || {
                        let mut sim = crate::redstone::simulator::Simulator::new(
                            result.physical().world().clone(),
                        );
                        sim.run_until_stable(2000).unwrap();
                        sim.attach_observer(
                            result.source_output_positions().iter().enumerate().map(
                                |(i, &(x, y, z))| (Position::new(x, y, z), format!("output{i}")),
                            ),
                        );
                        let mut records = Vec::new();
                        for from in 0..4 {
                            for to in 0..4 {
                                for address in [from, to] {
                                    sim.reset_observer();
                                    let start = sim.current_tick();
                                    for (input, &(x, y, z)) in
                                        result.source_input_positions().iter().enumerate()
                                    {
                                        let mut lever = sim.world().get(x, y, z).clone();
                                        lever.lit = address & (1 << input) != 0;
                                        sim.world_mut().set(x, y, z, lever);
                                    }
                                    let ticks = sim.run_until_stable(2000).unwrap();
                                    for (output, &(x, y, z)) in
                                        result.source_output_positions().iter().enumerate()
                                    {
                                        assert_eq!(sim.world().get(x,y,z).lit, words[address] & (1 << output) != 0,
                                    "mixed-sign {words:?} {from}->{to} address {address} output {output}");
                                    }
                                    records.push((
                                        ticks,
                                        sim.observations()
                                            .iter()
                                            .map(|e| (e.label.clone(), e.tick - start, e.value))
                                            .collect::<Vec<_>>(),
                                    ));
                                }
                            }
                        }
                        records
                    };
                    assert_eq!(replay(), replay());
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 38);
        for words in [[0, 0, 0, 7], [7, 0, 0, 0], [0, 7, 7, 0], [0, 1, 2, 0]] {
            assert!(subtract_cube(words).is_none());
        }
    }

    #[test]
    fn positive_product_certifies_constants_fanout_and_retained_history() {
        let mut checked = 0;

        for variable in 1..8u8 {
            for high in 0..8u8 {
                if high & variable != 0 {
                    continue;
                }
                let words =
                    std::array::from_fn(|address| high | if address == 3 { variable } else { 0 });
                assert_eq!(positive_product(words), Some(variable));
                let (source, _) = player_logic(words);
                let result = super::super::compile_player_torch_row(&source, 1_000_000)
                    .unwrap_or_else(|e| panic!("positive-product {words:?}: {e:?}"));
                let replay = || {
                    let mut sim = crate::redstone::simulator::Simulator::new(
                        result.physical().world().clone(),
                    );
                    sim.run_until_stable(2000).unwrap();
                    sim.attach_observer(
                        result
                            .source_output_positions()
                            .iter()
                            .enumerate()
                            .map(|(i, &(x, y, z))| (Position::new(x, y, z), format!("output{i}"))),
                    );
                    let mut records = Vec::new();
                    for from in 0..4 {
                        for to in 0..4 {
                            for address in [from, to] {
                                sim.reset_observer();
                                let start = sim.current_tick();
                                for (input, &(x, y, z)) in
                                    result.source_input_positions().iter().enumerate()
                                {
                                    let mut lever = sim.world().get(x, y, z).clone();
                                    lever.lit = address & (1 << input) != 0;
                                    sim.world_mut().set(x, y, z, lever);
                                }
                                let ticks = sim.run_until_stable(2000).unwrap();
                                for (output, &(x, y, z)) in
                                    result.source_output_positions().iter().enumerate()
                                {
                                    assert_eq!(sim.world().get(x,y,z).lit, words[address] & (1 << output) != 0,
                                    "positive-product {words:?} {from}->{to} address {address} output {output}");
                                }
                                records.push((
                                    ticks,
                                    sim.observations()
                                        .iter()
                                        .map(|e| (e.label.clone(), e.tick - start, e.value))
                                        .collect::<Vec<_>>(),
                                ));
                            }
                        }
                    }
                    records
                };
                assert_eq!(replay(), replay());
                checked += 1;
            }
        }

        assert_eq!(checked, 19);
        for words in [[0, 7, 0, 0], [7, 0, 0, 0], [0, 7, 7, 0], [0, 1, 2, 0]] {
            assert!(positive_product(words).is_none());
        }
    }

    #[test]
    fn negative_product_certifies_constants_fanout_and_retained_history() {
        let mut checked = 0;

        for variable in 1..8u8 {
            for high in 0..8u8 {
                if high & variable != 0 {
                    continue;
                }
                let words =
                    std::array::from_fn(|address| high | if address == 0 { variable } else { 0 });
                assert_eq!(negative_product(words), Some(variable));
                let (source, _) = player_logic(words);
                let result = super::super::compile_player_torch_row(&source, 1_000_000)
                    .unwrap_or_else(|e| panic!("negative-product {words:?}: {e:?}"));
                let replay = || {
                    let mut sim = crate::redstone::simulator::Simulator::new(
                        result.physical().world().clone(),
                    );
                    sim.run_until_stable(2000).unwrap();
                    sim.attach_observer(
                        result
                            .source_output_positions()
                            .iter()
                            .enumerate()
                            .map(|(i, &(x, y, z))| (Position::new(x, y, z), format!("output{i}"))),
                    );
                    let mut records = Vec::new();
                    for from in 0..4 {
                        for to in 0..4 {
                            for address in [from, to] {
                                sim.reset_observer();
                                let start = sim.current_tick();
                                for (input, &(x, y, z)) in
                                    result.source_input_positions().iter().enumerate()
                                {
                                    let mut lever = sim.world().get(x, y, z).clone();
                                    lever.lit = address & (1 << input) != 0;
                                    sim.world_mut().set(x, y, z, lever);
                                }
                                let ticks = sim.run_until_stable(2000).unwrap();
                                for (output, &(x, y, z)) in
                                    result.source_output_positions().iter().enumerate()
                                {
                                    assert_eq!(sim.world().get(x,y,z).lit, words[address] & (1 << output) != 0,
                                    "negative-product {words:?} {from}->{to} address {address} output {output}");
                                }
                                records.push((
                                    ticks,
                                    sim.observations()
                                        .iter()
                                        .map(|e| (e.label.clone(), e.tick - start, e.value))
                                        .collect::<Vec<_>>(),
                                ));
                            }
                        }
                    }
                    records
                };
                assert_eq!(replay(), replay());
                checked += 1;
            }
        }

        assert_eq!(checked, 19);
        for words in [[0, 7, 0, 0], [0, 0, 0, 7], [0, 7, 7, 0], [0, 1, 2, 0]] {
            assert!(negative_product(words).is_none());
        }
    }

    #[test]
    fn comparator_candidate_certifies_distinct_inputs_and_retained_history() {
        let library = comparator_library();
        let mut w = World::new(8, 4, 8);
        let input = Position::new(3, 2, 2);
        put(&mut w, input.down(), BlockKind::Solid);
        put(&mut w, input, BlockKind::Lever);
        let cmp = Position::new(3, 1, 3);
        put(&mut w, cmp.down(), BlockKind::Solid);
        let mut c = BlockState::air();
        c.kind = BlockKind::Comparator;
        c.name = "minecraft:comparator".into();
        c.facing = Some(Facing::East);
        c.extra_properties.insert("mode".into(), "subtract".into());
        w.set(cmp.x, cmp.y, cmp.z, c);
        put(&mut w, Position::new(5, 1, 3), BlockKind::Solid);
        put(&mut w, Position::new(4, 1, 3), BlockKind::WallTorch);
        let mut t = w.get(4, 1, 3).clone();
        t.facing = Some(Facing::West);
        w.set(4, 1, 3, t);
        wire(&mut w, Position::new(2, 1, 3));
        let lamp = Position::new(1, 1, 3);
        put(&mut w, lamp.down(), BlockKind::Solid);
        put(&mut w, lamp, BlockKind::Lamp);
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let (netlist, _, candidate) = super::super::typed_world_candidate_with_library(
            netlist,
            w,
            vec![input],
            vec![lamp],
            BTreeMap::from([("y".into(), cmp)]),
            true,
            &library,
        );
        assert_eq!(candidate.placements.len(), 2);
        assert_eq!(candidate.connections.len(), 2);
        let certified = realise_and_verify_expanded(&candidate, &netlist, &library)
            .expect("declared distinct comparator ports must certify");
        let replay = || {
            let mut sim = crate::redstone::simulator::Simulator::new(certified.world().clone());
            sim.run_until_stable(2000).unwrap();
            sim.attach_observer([
                (cmp, "comparator".to_string()),
                (Position::new(4, 1, 3), "constant".to_string()),
                (Position::new(2, 1, 3), "driver".to_string()),
                (lamp, "lamp".to_string()),
            ]);
            let mut records = Vec::new();
            for from in 0..4 {
                for to in 0..4 {
                    for address in [from, to] {
                        sim.reset_observer();
                        let start = sim.current_tick();
                        let high = address & 1 != 0;
                        let mut lever = sim.world().get(input.x, input.y, input.z).clone();
                        lever.lit = high;
                        sim.world_mut().set(input.x, input.y, input.z, lever);
                        let ticks = sim.run_until_stable(2000).unwrap();
                        assert_eq!(sim.world().get(cmp.x, cmp.y, cmp.z).power > 0, !high);
                        assert_eq!(sim.world().get(lamp.x, lamp.y, lamp.z).lit, !high);
                        assert!(
                            sim.world().get(4, 1, 3).lit,
                            "constant torch must remain lit"
                        );
                        records.push((
                            ticks,
                            sim.observations()
                                .iter()
                                .map(|e| (e.label.clone(), e.tick - start, e.value))
                                .collect::<Vec<_>>(),
                        ));
                    }
                }
            }
            records
        };
        assert_eq!(
            replay(),
            replay(),
            "retained-history replay must be deterministic"
        );
        // A shared inverter drives three ordered output occurrences through
        // real passive dust arms, preserving a single primitive owner.
        let mut fanout_world = World::new(8, 4, 12);
        let original = certified.world();
        for x in 0..8 {
            for y in 0..4 {
                for z in 0..8 {
                    fanout_world.set(x, y, z, original.get(x, y, z).clone());
                }
            }
        }
        fanout_world.set(lamp.x, lamp.y, lamp.z, BlockState::air());
        for z in 3..=9 {
            wire(&mut fanout_world, Position::new(2, 1, z));
        }
        let fanout_lamps = [3, 6, 9].map(|z| Position::new(0, 1, z));
        for p in fanout_lamps {
            wire(&mut fanout_world, Position::new(1, 1, p.z));
            put(&mut fanout_world, p.down(), BlockKind::Solid);
            put(&mut fanout_world, p, BlockKind::Lamp);
        }
        let mut fanout_netlist = netlist.clone();
        fanout_netlist.outputs = vec!["y".into(); 3];
        let (fanout_netlist, _, fanout_candidate) =
            super::super::typed_world_candidate_with_library(
                fanout_netlist,
                fanout_world,
                vec![input],
                fanout_lamps.to_vec(),
                BTreeMap::from([("y".into(), cmp)]),
                true,
                &library,
            );
        let fanout = realise_and_verify_expanded(&fanout_candidate, &fanout_netlist, &library)
            .expect("shared comparator fanout must certify");
        let mut sim = crate::redstone::simulator::Simulator::new(fanout.into_world());
        sim.run_until_stable(2000).unwrap();
        for from in 0..4 {
            for to in 0..4 {
                for address in [from, to] {
                    let high = address & 1 != 0;
                    let mut lever = sim.world().get(input.x, input.y, input.z).clone();
                    lever.lit = high;
                    sim.world_mut().set(input.x, input.y, input.z, lever);
                    sim.run_until_stable(2000).unwrap();
                    for p in fanout_lamps {
                        assert_eq!(sim.world().get(p.x, p.y, p.z).lit, !high);
                    }
                }
            }
        }
        for mutation in ["compare", "reversed", "missing_rear", "displaced_side"] {
            let mut invalid = candidate.clone();
            match mutation {
                "compare" | "reversed" => {
                    let cell = invalid
                        .placements
                        .values_mut()
                        .flat_map(|p| &mut p.blocks)
                        .find(|b| b.state.kind == BlockKind::Comparator)
                        .unwrap();
                    if mutation == "compare" {
                        cell.state
                            .extra_properties
                            .insert("mode".into(), "compare".into());
                    } else {
                        cell.state.facing = Some(Facing::West);
                    }
                }
                "missing_rear" => {
                    for placement in invalid.placements.values_mut() {
                        placement
                            .blocks
                            .retain(|b| b.state.kind != BlockKind::WallTorch);
                    }
                }
                "displaced_side" => {
                    let lever = invalid
                        .boundaries
                        .values_mut()
                        .flat_map(|b| &mut b.blocks)
                        .find(|b| b.state.kind == BlockKind::Lever)
                        .unwrap();
                    lever.at.x += 1;
                }
                _ => unreachable!(),
            }
            assert!(
                realise_and_verify_expanded(&invalid, &netlist, &library).is_err(),
                "invalid electrical cell {mutation} must not certify"
            );
        }
    }

    #[test]
    fn shared_negative_generation_uses_structure_with_renamed_sources() {
        for bit in 0..2 {
            let words =
                std::array::from_fn(|address| if address & (1 << bit) == 0 { 7 } else { 0 });
            let (mut source, _) = player_logic(words);
            let rename = |name: &str| format!("renamed_{name}");
            source.inputs = source.inputs.iter().map(|n| rename(n)).collect();
            source.outputs = source.outputs.iter().map(|n| rename(n)).collect();
            for g in &mut source.gates {
                g.name = rename(&g.name);
                g.output = rename(&g.output);
                g.inputs = g.inputs.iter().map(|n| rename(n)).collect();
            }
            let result = super::super::compile_player_torch_row(&source, 1_000_000).unwrap();
            assert_eq!(result.implemented().gates.len(), 1);
            assert_eq!(
                result
                    .physical()
                    .world()
                    .positions_of(BlockKind::Comparator)
                    .count(),
                1
            );
            assert_eq!(result.source_output_positions().len(), 3);
        }
        for words in [[0, 7, 0, 7], [0, 0, 7, 7], [7, 7, 7, 7], [7, 0, 0, 0]] {
            assert!(independent_negative_inputs(words).is_none());
        }
    }

    #[test]
    fn mixed_constant_outputs_use_their_declared_implementations() {
        let mut checked = 0;
        for bit in 0..2 {
            for variable in 1..8u8 {
                for high in 0..8u8 {
                    if variable & high != 0 {
                        continue;
                    }
                    let words = std::array::from_fn(|address| {
                        high | if address & (1 << bit) == 0 {
                            variable
                        } else {
                            0
                        }
                    });
                    let (source, _) = player_logic(words);
                    let result =
                        super::super::compile_player_torch_row(&source, 1_000_000).unwrap();
                    assert_eq!(
                        result
                            .physical()
                            .world()
                            .positions_of(BlockKind::Comparator)
                            .count(),
                        1
                    );
                    assert_eq!(result.source_output_positions().len(), 3);
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, 38);
    }

    #[test]
    fn independent_negative_outputs_keep_separate_certified_domains() {
        let mut checked = 0;
        for pattern in 0..64 {
            let words = std::array::from_fn(|address| {
                (0..3).fold(0, |word, output| {
                    let high = match (pattern >> (2 * output)) & 3 {
                        0 => false,
                        1 => true,
                        2 => address & 1 == 0,
                        3 => address & 2 == 0,
                        _ => unreachable!(),
                    };
                    word | (u8::from(high) << output)
                })
            });
            let Some(clusters) = independent_negative_inputs(words) else {
                continue;
            };
            let (source, _) = player_logic(words);
            let result = super::super::compile_player_torch_row(&source, 1_000_000).unwrap();
            assert_eq!(
                result
                    .physical()
                    .world()
                    .positions_of(BlockKind::Comparator)
                    .count(),
                clusters.len()
            );
            assert_eq!(result.source_output_positions().len(), 3);
            checked += 1;
        }
        assert_eq!(checked, 56);
        assert!(
            independent_negative_inputs([7, 7, 7, 0]).is_none(),
            "OR of independent negative literals needs reconvergence"
        );
    }

    #[test]
    fn independent_literals_restore_signal_before_private_output_fanout() {
        let mut checked = 0;
        for pattern in 0..216usize {
            let words = std::array::from_fn(|address| {
                (0..3u32).fold(0, |word, output| {
                    let high = match (pattern / 6usize.pow(output)) % 6 {
                        0 => false,
                        1 => true,
                        2 => address & 1 != 0,
                        3 => address & 1 == 0,
                        4 => address & 2 != 0,
                        5 => address & 2 == 0,
                        _ => unreachable!(),
                    };
                    word | (u8::from(high) << output)
                })
            });
            let Some(clusters) = disjoint_literal_inputs(words) else {
                continue;
            };
            let (source, _) = player_logic(words);
            let result = super::super::compile_player_torch_row(&source, 1_000_000).unwrap();
            let world = result.physical().world();
            assert_eq!(
                world.positions_of(BlockKind::Comparator).count(),
                clusters.iter().filter(|c| !c.2).count()
            );
            assert_eq!(
                world.positions_of(BlockKind::Repeater).count(),
                clusters.iter().filter(|c| c.2).count()
            );
            assert_eq!(result.source_output_positions().len(), 3);
            // Toggle both ports, including unused inputs, without clearing history.
            let mut sim = crate::redstone::simulator::Simulator::new(world.clone());
            sim.run_until_stable(2000).unwrap();
            for from in 0..4 {
                for to in 0..4 {
                    for address in [from, to] {
                        for (input, &(x, y, z)) in
                            result.source_input_positions().iter().enumerate()
                        {
                            let mut state = sim.world().get(x, y, z).clone();
                            state.lit = address & (1 << input) != 0;
                            sim.world_mut().set(x, y, z, state);
                        }
                        sim.run_until_stable(2000).unwrap();
                        for (output, &(x, y, z)) in
                            result.source_output_positions().iter().enumerate()
                        {
                            assert_eq!(sim.world().get(x,y,z).lit, words[address] & (1<<output)!=0,
                        "literal layout {words:?}, transition {from}->{to}, address {address}, output {output}");
                        }
                    }
                }
            }

            checked += 1;
        }
        assert_eq!(checked, 208);
        assert_eq!(disjoint_literal_inputs([1, 2, 1, 2]).unwrap().len(), 2);
        assert!(
            independent_literal_inputs([0, 1, 1, 1]).is_none(),
            "reconvergent literal outputs retain synchronization"
        );
    }

    #[test]
    fn dual_polarity_layers_share_one_boundary_and_certify_separate_outputs() {
        for bit in 0..2 {
            let mut w = World::new(20, 6, 14);
            let mut inputs = [Position::new(16, 2, 11); 2];
            inputs[bit] = Position::new(7, 2, 2);
            for p in inputs {
                put(&mut w, p.down(), BlockKind::Solid);
                put(&mut w, p, BlockKind::Lever);
            }
            let cmp = Position::new(7, 1, 3);
            put(&mut w, cmp.down(), BlockKind::Solid);
            let mut c = BlockState::air();
            c.kind = BlockKind::Comparator;
            c.name = "minecraft:comparator".into();
            c.facing = Some(Facing::East);
            c.extra_properties.insert("mode".into(), "subtract".into());
            w.set(7, 1, 3, c);
            put(&mut w, Position::new(9, 1, 3), BlockKind::Solid);
            put(&mut w, Position::new(8, 1, 3), BlockKind::WallTorch);
            let mut t = w.get(8, 1, 3).clone();
            t.facing = Some(Facing::West);
            w.set(8, 1, 3, t);
            wire(&mut w, Position::new(6, 2, 2));
            wire(&mut w, Position::new(6, 3, 3));
            let relay = Position::new(5, 3, 3);
            put(&mut w, relay.down(), BlockKind::Solid);
            put(&mut w, relay, BlockKind::Repeater);
            let mut r = w.get(5, 3, 3).clone();
            r.facing = Some(Facing::East);
            w.set(5, 3, 3, r);
            wire(&mut w, Position::new(4, 3, 3));
            for z in 3..=9 {
                wire(&mut w, Position::new(6, 1, z));
            }
            for z in 3..=6 {
                wire(&mut w, Position::new(3, 3, z));
            }
            let lamps = [
                Position::new(4, 1, 3),
                Position::new(1, 3, 6),
                Position::new(4, 1, 9),
            ];
            for (output, p) in lamps.into_iter().enumerate() {
                wire(
                    &mut w,
                    Position::new(if output == 1 { 2 } else { 5 }, p.y, p.z),
                );
                put(&mut w, p.down(), BlockKind::Solid);
                put(&mut w, p, BlockKind::Lamp);
            }
            let source = ["a", "b"][bit];
            let netlist = Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["negative".into(), "positive".into(), "negative".into()],
                gates: vec![
                    Gate::nor("negative", &[source]),
                    Gate {
                        name: "positive".into(),
                        output: "positive".into(),
                        inputs: vec![source.into()],
                        kind: GateKind::Buf,
                    },
                ],
            };
            let library = comparator_library();
            let (netlist, _, candidate) = super::super::typed_world_candidate_with_library(
                netlist,
                w,
                inputs.to_vec(),
                lamps.to_vec(),
                BTreeMap::from([("negative".into(), cmp), ("positive".into(), relay)]),
                true,
                &library,
            );
            let certified = realise_and_verify_expanded(&candidate, &netlist, &library).unwrap();
            let mut sim = crate::redstone::simulator::Simulator::new(certified.into_world());
            sim.run_until_stable(2000).unwrap();
            for from in 0..4 {
                for to in 0..4 {
                    for address in [from, to] {
                        for (input, p) in inputs.into_iter().enumerate() {
                            let mut v = sim.world().get(p.x, p.y, p.z).clone();
                            v.lit = address & (1 << input) != 0;
                            sim.world_mut().set(p.x, p.y, p.z, v);
                        }
                        sim.run_until_stable(2000).unwrap();
                        let high = address & (1 << bit) != 0;
                        for (output, p) in lamps.into_iter().enumerate() {
                            assert_eq!(
                                sim.world().get(p.x, p.y, p.z).lit,
                                if output == 1 { high } else { !high }
                            );
                        }
                        assert!(sim.world().get(8, 1, 3).lit);
                    }
                }
            }
        }
    }

    #[test]
    fn single_carriers_and_restored_arms_preserve_internal_truth() {
        let mut tables = vec![
            [5, 2, 0, 0],
            [4, 0, 1, 2],
            [4, 1, 0, 2],
            [1, 0, 0, 1],
            [0, 4, 5, 3],
            [5, 3, 3, 0],
            [1, 6, 3, 5],
            [1, 1, 1, 0],
            [0, 0, 0, 1],
            [1, 1, 0, 1],
            [0, 1, 1, 1],
        ];
        for middle in [1, 2] {
            let addresses = [0, middle, 3];
            for a in 0..3 {
                for b in 0..3 {
                    for c in 0..3 {
                        if a == b || b == c || a == c {
                            continue;
                        }
                        let mut words = [0; 4];
                        for (bit, index) in [a, b, c].into_iter().enumerate() {
                            words[addresses[index]] = 1 << bit;
                        }
                        tables.push(words);
                    }
                }
            }
        }
        for negative_input in 0..2 {
            for nor_output in 0..3 {
                for and_output in 0..3 {
                    if nor_output == and_output {
                        continue;
                    }
                    let unary_output = 3 - nor_output - and_output;
                    let mut words = [0; 4];
                    words[0] |= 1 << nor_output;
                    words[3] |= 1 << and_output;
                    for (address, word) in words.iter_mut().enumerate() {
                        if address & (1 << negative_input) == 0 {
                            *word |= 1 << unary_output;
                        }
                    }
                    tables.push(words);
                }
            }
        }
        for words in tables {
            let (netlist, _, candidate) = matrix_candidate(words);
            let world =
                realise_and_verify_expanded(&candidate, &netlist, &matrix_library(words)).unwrap();
            let views = candidate.compatibility_views(&netlist).unwrap();
            let mut sim = crate::redstone::simulator::Simulator::new(world.world().clone());
            sim.run_until_stable(2000).unwrap();
            for from in 0..4 {
                for to in 0..4 {
                    for address in [from, to] {
                        for (bit, name) in netlist.inputs.iter().enumerate() {
                            let &(x, y, z) = &views.input_positions[name];
                            let mut lever = sim.world().get(x, y, z).clone();
                            lever.lit = address & (1 << bit) != 0;
                            sim.world_mut().set(x, y, z, lever);
                        }
                        sim.run_until_stable(2000).unwrap();
                        let mut values: std::collections::HashMap<String, bool> = netlist
                            .inputs
                            .iter()
                            .enumerate()
                            .map(|(bit, name)| (name.clone(), address & (1 << bit) != 0))
                            .collect();
                        for index in netlist.combinational_order().unwrap() {
                            let gate = &netlist.gates[index];
                            let expected = gate.kind.evaluate(
                                &gate
                                    .inputs
                                    .iter()
                                    .map(|name| values[name])
                                    .collect::<Vec<_>>(),
                            );
                            let &(x, y, z) = &views.gate_output_positions[&gate.output];
                            let block = sim.world().get(x, y, z);
                            let actual = if block.kind == BlockKind::RedstoneWire {
                                block.power > 0
                            } else {
                                block.lit
                            };
                            assert_eq!(
                                actual, expected,
                                "internal {}: {words:?}, {from}->{to}, address={address}",
                                gate.output
                            );
                            values.insert(gate.output.clone(), expected);
                        }
                        for (bit, name) in netlist.outputs.iter().enumerate() {
                            let &(x, y, z) = &views.output_positions[name];
                            assert_eq!(
                                sim.world().get(x, y, z).lit,
                                words[address] & (1 << bit) != 0
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_wide_cover_observes_a_real_shared_carrier() {
        let mut wide = 0;
        for encoding in 0..4096u16 {
            let words = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
            if program_rows(words).len() != 6 {
                continue;
            }
            wide += 1;
            let (netlist, world, candidate) = candidate(words);
            for (name, at) in player_logic(words).1 {
                if name == "source0" || name == "source1" {
                    assert_eq!(world.get(at.x, at.y, at.z).kind, BlockKind::RedstoneWire);
                }
            }
            realise_and_verify_expanded(&candidate, &netlist, &typed_matrix_library())
                .unwrap_or_else(|error| panic!("wide cover {words:?}: {error:?}"));
        }
        assert_eq!(wide, 48);
    }

    #[test]
    fn supported_endpoint_contracts_fail_closed() {
        let (netlist, _, candidate) = matrix_candidate([1, 0, 0, 0]);
        let certify = |c: &crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate| {
            realise_and_verify_expanded(c, &netlist, &typed_matrix_library())
        };
        certify(&candidate).unwrap();
        let mut absent = candidate.clone();
        absent.repeater_handovers.clear();
        assert!(matches!(
            certify(&absent),
            Err(CertificationError::Physical(
                ExpandedPhysicalError::WrongRepeaterAxis { .. }
            ))
        ));
        for kind in [
            RepeaterHandoverKind::WeakFloorInput,
            RepeaterHandoverKind::StrongFloorOutput,
        ] {
            let mut missing = candidate.clone();
            missing.repeater_handovers.retain(|_, cs| {
                cs.retain(|c| c.kind != kind);
                !cs.is_empty()
            });
            assert!(certify(&missing).is_err(), "missing {kind:?} must fail");
        }
        let (&sink, contracts) = candidate.repeater_handovers.iter().next().unwrap();
        for mutation in 0..4 {
            let mut forged = candidate.clone();
            let cs = forged.repeater_handovers.get_mut(&sink).unwrap();
            match mutation {
                0 => cs[0].support.z += 1,
                1 => {
                    cs[0].kind = match cs[0].kind {
                        RepeaterHandoverKind::WeakFloorInput => {
                            RepeaterHandoverKind::StrongFloorOutput
                        }
                        _ => RepeaterHandoverKind::WeakFloorInput,
                    }
                }
                2 => {
                    let copy = cs[0];
                    cs.push(copy);
                }
                3 => cs[0].primitive.instance.0 += 1000,
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    certify(&forged),
                    Err(CertificationError::Physical(
                        ExpandedPhysicalError::InvalidRepeaterHandover { .. }
                    ))
                ),
                "forged contract {mutation}: {:?}",
                certify(&forged)
            );
            assert_ne!(candidate.fingerprint(), forged.fingerprint());
        }
        let mut unknown = candidate.clone();
        let mut unknown_sink = sink;
        unknown_sink.ordinal = 60000;
        unknown
            .repeater_handovers
            .insert(unknown_sink, contracts.clone());
        assert!(matches!(
            certify(&unknown),
            Err(CertificationError::Physical(
                ExpandedPhysicalError::InvalidRepeaterHandover { .. }
            ))
        ));
    }

    #[test]
    fn junction_taps_require_exact_source_and_support_landing() {
        let (netlist, _, candidate) = matrix_candidate([1, 0, 0, 0]);
        let certify = |c: &crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate| {
            realise_and_verify_expanded(c, &netlist, &typed_matrix_library())
        };
        certify(&candidate).unwrap();
        let (&route, ordinal) = candidate
            .routes
            .iter()
            .find_map(|(id, r)| {
                if !matches!(
                    r.source,
                    crate::compile::fragment_synth::identity::PhysicalEndpointId::Junction(_)
                ) {
                    return None;
                }
                r.branches
                    .iter()
                    .position(|b| {
                        b.terminal.kind
                            == crate::compile::routing::RouteTerminalKind::DirectedDustIntoSupport
                            && b.path.len() > 1
                    })
                    .map(|i| (id, i))
            })
            .unwrap();
        let mut misplaced = candidate.clone();
        let branch = &mut misplaced.routes.get_mut(&route).unwrap().branches[ordinal];
        branch.path.pop();
        branch.terminal.at = *branch.path.last().unwrap();
        assert!(certify(&misplaced).is_err());
    }
}
