//! Dedicated shared-channel terminal component, not ordinary split source ports.
//! Used by native physical compilation and exported shared-inout pinouts.
use super::planner::Anchor;
use crate::frontend::resolved::InoutBoundary;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct DriverInjection {
    /// Private stitch pad for the compiled present AND value contribution.
    pub(crate) at: Anchor,
    #[cfg(test)]
    pub(crate) diode: Anchor,
    pub(crate) signal: String,
}
pub(crate) struct SharedBusTerminal {
    #[cfg(test)]
    pub(crate) original_bit: String,
    pub(crate) channel: Vec<Anchor>,
    /// This actual diode reads the very same channel that drivers inject into.
    #[cfg(test)]
    pub(crate) read_tap: Anchor,
    pub(crate) read_output: Anchor,
    pub(crate) internal: Vec<DriverInjection>,
    /// External ownership/value protocol stays distinct from the common channel.
    pub(crate) external: DriverInjection,
    pub(crate) external_entry: Anchor,
    #[cfg(test)]
    pub(crate) external_value_signal: String,
    #[cfg(test)]
    pub(crate) external_present_signal: String,
    #[cfg(test)]
    pub(crate) reserved_air: Vec<Anchor>,
}
fn block(kind: BlockKind, name: &str) -> BlockState {
    let mut s = BlockState::air();
    s.kind = kind;
    s.name = name.into();
    s
}
/// Atomic placement into a fresh reserved envelope. This is a component builder,
/// not a claim that source reader routes/status circuits have been stitched.
pub(crate) fn materialize(
    world: &mut World,
    b: &InoutBoundary,
    base: Anchor,
) -> Result<SharedBusTerminal, String> {
    let count = b.internal_contributions.len() + 1;
    // A bidirectional dust channel cannot be refreshed by a one-way repeater.
    // Four injection slots span twelve dust edges plus two to the read tap, below the real strength limit.
    if !(1..=4).contains(&count) {
        return Err(
            "shared dust terminal needs 1..4 drivers; channel exceeds unrefreshed strength reach"
                .into(),
        );
    }
    let at = |x, y, z| Anchor {
        x: base.x + x,
        y: base.y + y,
        z: base.z + z,
    };
    let mut cells = BTreeMap::new();
    let mut air = BTreeSet::new();
    let mut place = |p: Anchor, state: BlockState| {
        cells.insert(p, state);
    };
    let last = 1 + 4 * (count as i32 - 1);
    let channel = (1..=last + 2).map(|z| at(6, 1, z)).collect::<Vec<_>>();
    for &p in &channel {
        place(
            at(6, 0, p.z - base.z),
            block(BlockKind::Glass, "minecraft:glass"),
        );
        place(p, block(BlockKind::RedstoneWire, "minecraft:redstone_wire"));
        air.insert(at(6, 2, p.z - base.z));
    }
    let signals = b
        .internal_contributions
        .iter()
        .map(|d| d.injection_value.clone())
        .chain(std::iter::once(b.external_injection.clone()));
    let mut injections = Vec::new();
    for (i, signal) in signals.enumerate() {
        let z = 1 + 4 * i as i32;
        let pad = at(8, 1, z);
        let diode = at(7, 1, z);
        for x in [7, 8] {
            place(at(x, 0, z), block(BlockKind::Glass, "minecraft:glass"));
        }
        place(
            pad,
            block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
        );
        let mut s = super::repeater(Facing::West);
        s.lit = false;
        place(diode, s);
        for x in [7, 8] {
            for dz in [-1, 1] {
                if i + 1 == count && x == 8 && dz == -1 {
                    continue;
                }
                air.insert(at(x, 1, z + dz));
            }
            air.insert(at(x, 2, z));
        }
        injections.push(DriverInjection {
            at: pad,
            #[cfg(test)]
            diode,
            signal,
        });
    }
    let read_z = last + 2;
    let read_tap = at(7, 1, read_z);
    let read_output = at(8, 1, read_z);
    for x in [7, 8] {
        place(at(x, 0, read_z), block(BlockKind::Glass, "minecraft:glass"));
    }
    let mut tap = super::repeater(Facing::East);
    tap.lit = false;
    place(read_tap, tap);
    place(
        read_output,
        block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
    );
    for x in [7, 8] {
        air.insert(at(x, 1, read_z - 1));
        air.insert(at(x, 1, read_z + 1));
        air.insert(at(x, 2, read_z));
    }
    let size = world.size();
    for p in cells.keys().chain(air.iter()) {
        if p.x < 0 || p.y < 0 || p.z < 0 || p.x >= size.0 || p.y >= size.1 || p.z >= size.2 {
            return Err("shared-bus envelope exceeds world bounds".into());
        }
        if world.get(p.x, p.y, p.z).kind != BlockKind::Air {
            return Err(format!("shared-bus footprint collides at {p:?}"));
        }
    }
    for (p, s) in cells {
        world.set(p.x, p.y, p.z, s);
    }
    let external = injections.pop().unwrap();
    Ok(SharedBusTerminal {
        #[cfg(test)]
        original_bit: b.original.name.clone(),
        channel,
        #[cfg(test)]
        read_tap,
        read_output,
        internal: injections,
        external,
        external_entry: at(8, 1, last - 1),
        #[cfg(test)]
        external_value_signal: b.external_value.clone(),
        #[cfg(test)]
        external_present_signal: b.external_present.clone(),
        #[cfg(test)]
        reserved_air: air.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::{evaluate::Evaluator, resolved::compile_semantics, SourceInput};
    use crate::redstone::simulator::Simulator;
    use crate::redstone::world::block::Face;
    #[test]
    fn actual_shared_channel_isolates_disabled_injections_and_reads_all_oracle_transitions() {
        let text = include_str!("../../tests/fixtures/resolved-inout.sv");
        let semantic = compile_semantics(
            &[SourceInput {
                name: "fixture.sv",
                text,
            }],
            "top",
        )
        .unwrap();
        let b = &semantic.boundaries[0];
        let mut logical = Evaluator::new(&semantic.netlist).unwrap();
        let mut world = World::new(12, 4, 14);
        let terminal = materialize(&mut world, b, Anchor { x: 0, y: 0, z: 0 }).unwrap();
        assert_eq!(terminal.original_bit, "bus");
        assert!(!terminal.reserved_air.is_empty());
        assert_eq!(terminal.external_value_signal, b.external_value);
        assert_eq!(terminal.external_present_signal, b.external_present);
        let injections = terminal
            .internal
            .iter()
            .chain(std::iter::once(&terminal.external))
            .collect::<Vec<_>>();
        let levers = injections
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i + 1 == injections.len() {
                    terminal.external_entry
                } else {
                    Anchor {
                        x: p.at.x + 1,
                        y: p.at.y,
                        z: p.at.z,
                    }
                }
            })
            .collect::<Vec<_>>();
        for p in &levers {
            world.set(
                p.x,
                p.y - 1,
                p.z,
                block(BlockKind::Solid, "minecraft:stone"),
            );
            let mut lever = block(BlockKind::Lever, "minecraft:lever");
            lever.face = Some(Face::Floor);
            lever.facing = Some(Facing::North);
            world.set(p.x, p.y, p.z, lever);
        }
        // These caller levers exercise the terminal's stitch seam using the
        // emitted source-bound contribution values. They are not a compiled
        // source logic circuit and cannot prove the full product consumer.
        let mut sim = Simulator::new(world);
        sim.run_until_stable(2000).unwrap();
        let raw = include_str!("../../tests/fixtures/resolved-inout-standard-oracle.csv");
        let mut rows = 0;
        for line in raw.lines() {
            let f = line.split(',').collect::<Vec<_>>();
            if f.len() != 4 {
                continue;
            }
            let before = f[0].parse::<usize>().unwrap();
            let after = f[1].parse::<usize>().unwrap();
            let phase = f[2].parse::<usize>().unwrap();
            let state = if phase == 0 { before } else { after };
            let inputs = [
                ("e0", 0),
                ("d0", 1),
                ("e1", 2),
                ("d1", 3),
                (b.external_present.as_str(), 4),
                (b.external_value.as_str(), 5),
            ]
            .into_iter()
            .map(|(n, bit)| (n.into(), state & (1 << bit) != 0))
            .collect();
            let values = logical.step(&inputs).unwrap();
            for (injection, p) in injections.iter().zip(&levers) {
                let mut s = sim.world().get(p.x, p.y, p.z).clone();
                s.lit = values[&injection.signal];
                sim.world_mut().set(p.x, p.y, p.z, s);
            }
            sim.run_until_stable(2000).unwrap();
            let high = injections.iter().any(|p| values[&p.signal]);
            for p in &terminal.channel {
                assert_eq!(
                    sim.world().get(p.x, p.y, p.z).power > 0,
                    high,
                    "state={state},channel={p:?}"
                );
            }
            assert_eq!(
                sim.world()
                    .get(
                        terminal.read_tap.x,
                        terminal.read_tap.y,
                        terminal.read_tap.z
                    )
                    .lit,
                high
            );
            assert_eq!(
                sim.world()
                    .get(
                        terminal.read_output.x,
                        terminal.read_output.y,
                        terminal.read_output.z
                    )
                    .power
                    > 0,
                high
            );
            for p in &injections {
                assert_eq!(
                    sim.world().get(p.at.x, p.at.y, p.at.z).power > 0,
                    values[&p.signal],
                    "foreign drive entered pad at {:?},state={state}",
                    p.at
                );
                assert_eq!(
                    sim.world().get(p.diode.x, p.diode.y, p.diode.z).lit,
                    values[&p.signal]
                );
            }
            if matches!(f[3], "0" | "1") {
                assert_eq!(high, f[3] == "1");
                assert_eq!(b.observe(&values), Ok(high));
            } else {
                assert!(
                    b.observe(&values).is_err(),
                    "invalid raw power cannot become an HDL observation"
                );
            }
            rows += 1;
        }
        assert_eq!(rows, 8192);
    }
    #[test]
    fn every_supported_driver_count_has_full_strength_reach_and_no_reverse_drive() {
        for count in [1, 2, 4] {
            let internal = count - 1;
            let ports = (0..internal)
                .map(|i| format!("input e{i},d{i},"))
                .collect::<String>();
            let assigns = (0..internal)
                .map(|i| format!("assign bus=e{i} ? d{i} : 1'bz;"))
                .collect::<String>();
            let text = format!(
                "module top({ports}inout wire bus,output y);{assigns}assign y=bus;endmodule"
            );
            let semantic = compile_semantics(
                &[SourceInput {
                    name: "boundary.sv",
                    text: &text,
                }],
                "top",
            )
            .unwrap();
            let mut world = World::new(12, 4, 18);
            let terminal = materialize(
                &mut world,
                &semantic.boundaries[0],
                Anchor { x: 0, y: 0, z: 0 },
            )
            .unwrap();
            let injections = terminal
                .internal
                .iter()
                .chain(std::iter::once(&terminal.external))
                .collect::<Vec<_>>();
            assert_eq!(injections.len(), count);
            let levers = injections
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    if i + 1 == injections.len() {
                        terminal.external_entry
                    } else {
                        Anchor {
                            x: p.at.x + 1,
                            y: p.at.y,
                            z: p.at.z,
                        }
                    }
                })
                .collect::<Vec<_>>();
            for p in &levers {
                world.set(
                    p.x,
                    p.y - 1,
                    p.z,
                    block(BlockKind::Solid, "minecraft:stone"),
                );
                let mut lever = block(BlockKind::Lever, "minecraft:lever");
                lever.face = Some(Face::Floor);
                lever.facing = Some(Facing::North);
                world.set(p.x, p.y, p.z, lever);
            }
            let mut sim = Simulator::new(world);
            sim.run_until_stable(2000).unwrap();
            for active in 0..count {
                for on in [true, false] {
                    for (i, p) in levers.iter().enumerate() {
                        let mut state = sim.world().get(p.x, p.y, p.z).clone();
                        state.lit = on && i == active;
                        sim.world_mut().set(p.x, p.y, p.z, state);
                    }
                    sim.run_until_stable(2000).unwrap();
                    for p in &terminal.channel {
                        assert_eq!(
                            sim.world().get(p.x, p.y, p.z).power > 0,
                            on,
                            "drivers={count},active={active},at={p:?}"
                        );
                    }
                    assert_eq!(
                        sim.world()
                            .get(
                                terminal.read_tap.x,
                                terminal.read_tap.y,
                                terminal.read_tap.z
                            )
                            .lit,
                        on
                    );
                    assert_eq!(
                        sim.world()
                            .get(
                                terminal.read_output.x,
                                terminal.read_output.y,
                                terminal.read_output.z
                            )
                            .power
                            > 0,
                        on
                    );
                    for (i, p) in injections.iter().enumerate() {
                        assert_eq!(
                            sim.world().get(p.at.x, p.at.y, p.at.z).power > 0,
                            on && i == active,
                            "reverse power drivers={count},active={active},pad={i}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn collision_refusal_preserves_every_original_world_cell() {
        let semantic = compile_semantics(
            &[SourceInput {
                name: "fixture.sv",
                text: include_str!("../../tests/fixtures/resolved-inout.sv"),
            }],
            "top",
        )
        .unwrap();
        let snapshot = |w: &World| {
            (0..12)
                .flat_map(|x| {
                    (0..4).flat_map(move |y| (0..14).map(move |z| w.get(x, y, z).clone()))
                })
                .collect::<Vec<_>>()
        };
        for obstruction in [Anchor { x: 6, y: 1, z: 5 }, Anchor { x: 7, y: 1, z: 0 }] {
            let mut world = World::new(12, 4, 14);
            world.set(
                obstruction.x,
                obstruction.y,
                obstruction.z,
                block(BlockKind::Solid, "minecraft:stone"),
            );
            let before = snapshot(&world);
            assert!(materialize(
                &mut world,
                &semantic.boundaries[0],
                Anchor { x: 0, y: 0, z: 0 }
            )
            .is_err());
            assert_eq!(snapshot(&world), before, "collision refusal changed world");
        }
    }
}
