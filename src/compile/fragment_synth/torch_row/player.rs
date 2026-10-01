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
fn row_x(index: usize) -> i32 {
    2 + 4 * index as i32
}

fn positive_literal(bits: u8, mask: u8) -> bool {
    mask.count_ones() == 1 && bits & mask == mask
}

fn source_x(rows: &[(usize, u8, u8, u8)]) -> i32 {
    16.max(rows.iter().map(|r| row_x(r.0) + 4).max().unwrap_or(16))
}

fn needs_input_buffer(rows: &[(usize, u8, u8, u8)]) -> bool {
    // Retain timing isolation for torch controls and reconverging data outputs.
    // Pure positive literals use noninverting repeaters throughout. Other
    // unbuffered outputs each have one data torch, without reconvergence.
    rows.iter()
        .any(|r| !positive_literal(r.1, r.2) && r.1 & r.2 != 0)
        || (0..3).any(|bit| {
            let terms: Vec<_> = rows.iter().filter(|r| r.3 & (1 << bit) != 0).collect();
            terms.len() > 1 && terms.iter().any(|r| !positive_literal(r.1, r.2))
        })
}

fn player_world(words: [u8; 4]) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let rows = program_rows(words);
    let source_x = source_x(&rows);
    let buffered = needs_input_buffer(&rows);
    let dual = rows.len() >= 5;
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
        let x = row_x(row);
        for z in 3..=if positive_literal(address, mask) && word & 4 != 0 {
            17
        } else {
            16
        } {
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
        for (bit, z) in [10, 13, 16].into_iter().enumerate() {
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
    let dual = rows.len() >= 5;
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
        let x = row_x(address);
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
                Position::new(x, 3, 8),
            );
            control
        };
        for (bit, z) in [10, 13, 16].into_iter().enumerate() {
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
    fn comparator_candidate_certifies_distinct_inputs_and_retained_history() {
        use crate::compile::topology::{
            ComparatorInverterContract, Library, LibraryEntry, LibraryEntryId, Primitive, Template,
            TemplateNode,
        };
        let mut library = Library::new(BTreeMap::from([(
            GateKind::Nor(1),
            vec![LibraryEntry {
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
            }],
        )]));
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
                },
            )
            .unwrap();
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
    fn input_buffers_protect_torch_and_reconverging_paths() {
        for words in [[1, 1, 1, 0], [0, 0, 0, 1], [1, 1, 0, 1]] {
            assert!(needs_input_buffer(&program_rows(words)));
        }
        for words in [
            [1, 1, 0, 0],
            [1, 0, 0, 0],
            [0, 0, 1, 1],
            [0, 1, 0, 1],
            [0, 1, 1, 1],
        ] {
            let rows = program_rows(words);
            assert!(!needs_input_buffer(&rows));
            assert!(rows
                .iter()
                .all(|r| source_x(&rows) - (row_x(r.0) + 2) <= 15));
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
