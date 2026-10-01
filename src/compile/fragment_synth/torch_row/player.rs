//! Experimental player-inspired matrix. Not selected by the shipping compiler.
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
fn source_x(rows: &[(usize, u8, u8, u8)]) -> i32 {
    16.max(
        rows.iter()
            .map(|r| 2 + 3 * r.0 as i32 + 4)
            .max()
            .unwrap_or(16),
    )
}

fn needs_input_buffer(rows: &[(usize, u8, u8, u8)]) -> bool {
    // Retain timing isolation for torch controls and reconverging data outputs.
    // Repeater-only controls with one data torch per output have at most three
    // rows; the 16-block input position still reaches every real input tap.
    rows.iter().any(|r| r.1 & r.2 != 0)
        || (0..3).any(|bit| rows.iter().filter(|r| r.3 & (1 << bit) != 0).count() > 1)
}

fn player_world(words: [u8; 4]) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let rows = program_rows(words);
    let source_x = source_x(&rows);
    let buffered = needs_input_buffer(&rows);
    let dual = rows.len() == 6;
    let mut w = World::new((source_x + 5).max(20), 10, 21);
    let address_z = [4, 7];
    let inputs = address_z.map(|z| Position::new(source_x, 5, z));
    let always_high = words.into_iter().fold(7, |a, b| a & b);
    for (bit, z) in address_z.into_iter().enumerate() {
        let used = rows.iter().any(|r| r.2 & (1 << bit) != 0);
        wire(&mut w, Position::new(source_x, 5, z));
        put(&mut w, Position::new(source_x, 5, z), BlockKind::Lever);
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
            for p in [
                Position::new(22, 5, z),
                Position::new(24, 7, z),
                Position::new(25, 8, z),
                Position::new(23, 6, z),
            ] {
                wire(&mut w, p);
            }
            for x in 13..=25 {
                wire(&mut w, Position::new(x, 8, z + 1));
            }
            wire(&mut w, Position::new(12, 7, z + 1));
            wire(&mut w, Position::new(11, 6, z + 1));
            wire(&mut w, Position::new(10, 5, z + 1));
            put(&mut w, Position::new(14, 8, z + 1), BlockKind::Repeater);
            let mut r = w.get(14, 8, z + 1).clone();
            r.facing = Some(Facing::East);
            w.set(14, 8, z + 1, r);
        }
    }
    for &(row, address, mask, word) in &rows {
        let x = 2 + 3 * row as i32;
        for z in 3..=16 {
            wire(&mut w, Position::new(x, 3, z));
        }
        for (bit, z) in address_z.into_iter().enumerate() {
            if mask & (1 << bit) == 0 {
                continue;
            }
            if address & (1 << bit) != 0 {
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
        for (bit, z) in [10, 13, 16].into_iter().enumerate() {
            if word & (1 << bit) != 0 {
                put(&mut w, Position::new(x + 1, 2, z), BlockKind::WallTorch);
            }
        }
    }
    let drivers = [10, 13, 16].map(|z| Position::new(10, 1, z));
    let lamps = [10, 13, 16].map(|z| Position::new(10, 1, z + 2));
    for (bit, z) in [10, 13, 16].into_iter().enumerate() {
        for x in 1..=source_x {
            wire(&mut w, Position::new(x, 1, z));
        }
        wire(&mut w, Position::new(10, 1, z + 1));
        let p = lamps[bit];
        put(&mut w, p.down(), BlockKind::Solid);
        put(&mut w, p, BlockKind::Lamp);
    }
    for (bit, z) in [10, 13, 16].into_iter().enumerate() {
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
        for (bit, z) in [10, 13, 16].into_iter().enumerate() {
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
    let buffered = needs_input_buffer(&rows);
    let dual = rows.len() == 6;
    let high = words.into_iter().fold(7, |a, b| a & b);
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
        let z = 4 + 3 * bit as i32;
        if !buffered {
            continue;
        }
        if dual {
            let right = format!("source{bit}_right");
            let left = format!("source{bit}_left");
            add(buf(right.clone(), name), Position::new(source_x - 1, 5, z));
            add(buf(left.clone(), name), Position::new(14, 8, z + 1));
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
        let x = 2 + 3 * address as i32;
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
            let z = 4 + 3 * bit as i32;
            if bits & (1 << bit) != 0 {
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
                Position::new(x, 3, 8),
            );
            control
        };
        for (bit, z) in [10, 13, 16].into_iter().enumerate() {
            if word & (1 << bit) == 0 {
                continue;
            }
            let name = format!("row{address}_bit{bit}");
            add(Gate::nor(&name, &[&control]), Position::new(x + 1, 2, z));
            terms[bit].push(name);
        }
    }
    let mut outputs = Vec::new();
    for (bit, z) in [10, 13, 16].into_iter().enumerate() {
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

fn build_candidate(
    words: [u8; 4],
) -> (
    Netlist,
    World,
    super::super::candidate::ExpandedPhysicalCandidate,
) {
    let (netlist, positions) = player_logic(words);
    let (world, inputs, _, lamps) = player_world(words);
    typed_world_candidate(
        netlist,
        world,
        inputs.to_vec(),
        lamps.to_vec(),
        positions,
        true,
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
    fn input_buffers_protect_torch_and_reconverging_paths() {
        for words in [[1, 1, 1, 0], [0, 0, 0, 1], [0, 0, 1, 1]] {
            assert!(needs_input_buffer(&program_rows(words)));
        }
        for words in [[1, 1, 0, 0], [1, 0, 0, 0]] {
            let rows = program_rows(words);
            assert!(!needs_input_buffer(&rows));
            assert!(rows
                .iter()
                .all(|r| source_x(&rows) - (2 + 3 * r.0 as i32 + 2) <= 15));
            let (netlist, _, candidate) = candidate(words);
            assert!(!netlist.gates.iter().any(|g| g.output.starts_with("source")));
            realise_and_verify_expanded(&candidate, &netlist, &typed_matrix_library()).unwrap();
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
        let (netlist, _, candidate) = candidate([1, 0, 0, 0]);
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
        let (netlist, _, candidate) = candidate([1, 0, 0, 0]);
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
