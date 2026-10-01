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
fn player_world(words: [u8; 4]) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let pitch = 3;
    let prune = true;
    let source_delay = 1;
    let mut w = World::new(20, 8, 19);
    let address_z = [4, 4 + pitch];
    let inputs = address_z.map(|z| Position::new(16, 5, z));
    for z in address_z {
        for x in 0..=16 {
            wire(&mut w, Position::new(x, 5, z));
        }
        put(&mut w, Position::new(16, 5, z), BlockKind::Lever);
    }
    let always_high = if prune {
        words.into_iter().fold(7, |a, b| a & b)
    } else {
        0
    };
    if source_delay > 0 && words.into_iter().any(|word| word & !always_high != 0) {
        for z in address_z {
            put(&mut w, Position::new(15, 5, z), BlockKind::Repeater);
            let mut r = w.get(15, 5, z).clone();
            r.facing = Some(Facing::East);
            r.delay = source_delay;
            w.set(15, 5, z, r);
        }
    }
    for (address, word) in words.into_iter().enumerate() {
        let word = word & !always_high;
        if prune && word == 0 {
            continue;
        }
        let x = 2 + 3 * address as i32;
        for z in 3..=16 {
            wire(&mut w, Position::new(x, 3, z));
        }
        for (bit, z) in address_z.into_iter().enumerate() {
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
    let drivers = [10, 13, 16].map(|z| Position::new(16, 1, z));
    let lamps = [10, 13, 16].map(|z| Position::new(17, 1, z));
    for z in [10, 13, 16] {
        for x in 1..=16 {
            wire(&mut w, Position::new(x, 1, z));
        }
        put(&mut w, Position::new(17, 0, z), BlockKind::Solid);
        put(&mut w, Position::new(17, 1, z), BlockKind::Lamp);
    }
    for (bit, z) in [10, 13, 16].into_iter().enumerate() {
        if always_high & (1 << bit) != 0 {
            put(&mut w, Position::new(17, 2, z), BlockKind::Solid);
            put(&mut w, Position::new(16, 2, z), BlockKind::WallTorch);
            let mut t = w.get(16, 2, z).clone();
            t.facing = Some(Facing::West);
            w.set(16, 2, z, t);
        }
    }
    // A low output has an explicit NOR0 -> NOR1 driver, rather than floating dust.
    // Keep earlier diagnostic modes unchanged so their frozen controls remain usable.
    {
        let ever_high = words.into_iter().fold(0, |a, b| a | b);
        for (bit, z) in [10, 13, 16].into_iter().enumerate() {
            if ever_high & (1 << bit) == 0 {
                put(&mut w, Position::new(15, 4, z + 1), BlockKind::Solid);
                wire(&mut w, Position::new(15, 3, z));
                for (at, facing) in [
                    (Position::new(15, 4, z), Facing::North),
                    (Position::new(16, 2, z), Facing::East),
                ] {
                    put(&mut w, at, BlockKind::WallTorch);
                    let mut t = w.get(at.x, at.y, at.z).clone();
                    t.facing = Some(facing);
                    w.set(at.x, at.y, at.z, t);
                }
            }
        }
    }
    if words.into_iter().all(|word| word & !always_high == 0) {
        for z in address_z {
            for x in 0..16 {
                w.set(x, 5, z, BlockState::air());
                w.set(x, 4, z, BlockState::air());
            }
        }
    }
    (w, inputs, drivers, lamps)
}

fn player_logic(words: [u8; 4]) -> (Netlist, BTreeMap<String, Position>) {
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    let mut terms: [Vec<String>; 3] = std::array::from_fn(|_| Vec::new());
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
    if words.into_iter().any(|word| word & !high != 0) {
        for (bit, name) in ["a", "b"].into_iter().enumerate() {
            add(
                buf(format!("source{bit}"), name),
                Position::new(15, 5, 4 + 3 * bit as i32),
            );
        }
    }
    for (address, word) in words.into_iter().enumerate() {
        let word = word & !high;
        if word == 0 {
            continue;
        }
        let x = 2 + 3 * address as i32;
        let mut mismatches = Vec::new();
        for bit in 0..2 {
            let name = format!("row{address}_mismatch{bit}");
            let source = format!("source{bit}");
            let z = 4 + 3 * bit as i32;
            if address & (1 << bit) != 0 {
                add(Gate::nor(&name, &[&source]), Position::new(x, 4, z - 1));
            } else {
                add(buf(name.clone(), &source), Position::new(x + 1, 4, z));
            }
            mismatches.push(name);
        }
        let control = format!("row{address}_control");
        add(
            Gate::merge(
                &control,
                &mismatches.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
            Position::new(x, 3, 8),
        );
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
            add(Gate::nor(&name, &[]), Position::new(16, 2, z));
            name
        } else if terms[bit].is_empty() {
            let hi = format!("constant_high{bit}");
            let lo = format!("constant_low{bit}");
            add(Gate::nor(&hi, &[]), Position::new(15, 4, z));
            add(Gate::nor(&lo, &[&hi]), Position::new(16, 2, z));
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
                Position::new(16, 1, z),
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
