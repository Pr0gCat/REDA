//! Physical probe for a programmed wall-torch row with outputs below it.
use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use reda::redstone::world::storage::World;

fn block(kind: BlockKind, name: &str) -> BlockState {
    let mut b = BlockState::air();
    b.kind = kind;
    b.name = name.into();
    b
}

#[test]
fn programmed_torch_row_drives_separate_outputs_below() {
    let mut world = World::new(12, 6, 8);
    for x in 1..10 {
        world.set(x, 2, 3, block(BlockKind::Solid, "minecraft:stone"));
        world.set(
            x,
            3,
            3,
            block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
        );
    }
    let mut lever = block(BlockKind::Lever, "minecraft:lever");
    lever.face = Some(Face::Floor);
    lever.facing = Some(Facing::North);
    world.set(1, 3, 3, lever);
    // Separated output columns; the middle column has no programmed torch.
    for x in [3, 6, 9] {
        world.set(x, 0, 4, block(BlockKind::Solid, "minecraft:stone"));
        world.set(
            x,
            1,
            4,
            block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
        );
    }
    for x in [3, 9] {
        let mut torch = block(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
        torch.facing = Some(Facing::South);
        torch.lit = true;
        world.set(x, 2, 4, torch);
    }
    let mut sim = Simulator::new(world);
    for on in [false, true, false, true] {
        let mut lever = sim.world().get(1, 3, 3).clone();
        lever.lit = on;
        sim.world_mut().set(1, 3, 3, lever);
        sim.run_until_stable(200).expect("row must settle");
        for x in [3, 6, 9] {
            assert_eq!(
                sim.world().get(x, 1, 4).power > 0,
                x != 6 && !on,
                "input={on}, output column={x}"
            );
        }
    }
}

/// Rows are active-low enables; each programmed torch contributes to a
/// shared output column. Exercise all enable combinations, not only one-hot.
#[test]
fn multiple_programmed_rows_share_output_columns_without_feedback() {
    assert_eq!(probe_matrix(0), (32, 165, 12));
}

#[test]
fn held_rows_do_not_emit_extra_output_pulses() {
    let (_, _, pulses) = probe_matrix(61);
    assert_eq!(pulses, 0);
}

fn probe_matrix(hold_ticks: usize) -> (u64, usize, usize) {
    let words = [0b101u8, 0b110, 0b011];
    let mut world = World::new(12, 6, 14);
    for z in [3, 6, 9] {
        for x in 1..10 {
            world.set(x, 2, z, block(BlockKind::Solid, "minecraft:stone"));
            world.set(
                x,
                3,
                z,
                block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
            );
        }
        let mut lever = block(BlockKind::Lever, "minecraft:lever");
        lever.face = Some(Face::Floor);
        lever.facing = Some(Facing::North);
        lever.lit = true;
        world.set(1, 3, z, lever);
    }
    for x in [3, 6, 9] {
        for z in 2..12 {
            world.set(x, 0, z, block(BlockKind::Solid, "minecraft:stone"));
            world.set(
                x,
                1,
                z,
                block(BlockKind::RedstoneWire, "minecraft:redstone_wire"),
            );
        }
    }
    for (row, z) in [3, 6, 9].into_iter().enumerate() {
        for (bit, x) in [3, 6, 9].into_iter().enumerate() {
            if words[row] & (1 << bit) != 0 {
                let mut torch = block(BlockKind::WallTorch, "minecraft:redstone_wall_torch");
                torch.facing = Some(Facing::South);
                torch.lit = true;
                world.set(x, 2, z + 1, torch);
            }
        }
    }
    let mut sim = Simulator::new(world);
    sim.run_until_stable(200).unwrap();
    let mut worst_ticks = 0;
    let mut observed_changes = 0;
    let mut pulsed_columns = 0;
    sim.attach_observer(
        [3, 6, 9]
            .into_iter()
            .map(|x| (Position::new(x, 1, 11), format!("bit-{x}"))),
    );
    for from in 0u8..8 {
        for to in 0u8..8 {
            for enabled in [from, to] {
                for _ in 0..hold_ticks {
                    sim.step();
                }
                sim.reset_observer();
                let mut expected = 0;
                for (row, z) in [3, 6, 9].into_iter().enumerate() {
                    let mut lever = sim.world().get(1, 3, z).clone();
                    lever.lit = enabled & (1 << row) == 0;
                    if !lever.lit {
                        expected |= words[row];
                    }
                    sim.world_mut().set(1, 3, z, lever);
                }
                worst_ticks =
                    worst_ticks.max(sim.run_until_stable(200).expect("matrix must settle"));
                observed_changes += sim.observations().len();
                for x in [3, 6, 9] {
                    let changes: Vec<_> = sim
                        .observations()
                        .iter()
                        .filter(|o| o.position == Position::new(x, 1, 11))
                        .collect();
                    if changes.len() > 1 {
                        pulsed_columns += 1;
                        println!("pulse: from={from}, to={to}, applied={enabled}, x={x}, changes={changes:?}");
                    }
                }
                for (bit, x) in [3, 6, 9].into_iter().enumerate() {
                    assert_eq!(
                        sim.world().get(x, 1, 11).power > 0,
                        expected & (1 << bit) != 0,
                        "transition={from}->{to}, enables={enabled}, bit={bit}"
                    );
                }
                for (row, z) in [3, 6, 9].into_iter().enumerate() {
                    assert_eq!(
                        sim.world().get(9, 3, z).power > 0,
                        enabled & (1 << row) == 0,
                        "output must not feed back into row"
                    );
                }
            }
        }
    }
    println!("matrix hold={hold_ticks}: 64 transitions plus 64 setup transitions; worst settling={worst_ticks} game ticks; output changes={observed_changes}; pulsed column transitions={pulsed_columns}");
    (worst_ticks, observed_changes, pulsed_columns)
}
