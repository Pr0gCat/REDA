//! Independent physical experiment; no compiler selection or certification claim.
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::{
        block::{BlockKind, BlockState, Face, Facing},
        storage::World,
    },
};
use serde_json::json;
fn set(w: &mut World, p: Position, k: BlockKind, f: Option<Facing>) {
    let mut b = BlockState::air();
    b.kind = k;
    b.facing = f;
    b.name = match k {
        BlockKind::Solid => "minecraft:stone",
        BlockKind::Comparator => "minecraft:comparator",
        BlockKind::Repeater => "minecraft:repeater",
        BlockKind::WallTorch => "minecraft:redstone_wall_torch",
        BlockKind::Lever => "minecraft:lever",
        BlockKind::RedstoneWire => "minecraft:redstone_wire",
        BlockKind::Lamp => "minecraft:redstone_lamp",
        _ => unreachable!(),
    }
    .into();
    if k == BlockKind::WallTorch {
        b.lit = true;
    }
    if k == BlockKind::Repeater {
        b.delay = 1;
    }
    if k == BlockKind::Lever {
        b.face = Some(Face::Floor);
    }
    if k == BlockKind::Comparator {
        b.extra_properties.insert("mode".into(), "subtract".into());
    }
    w.set(p.x, p.y, p.z, b);
}
fn run(comparator: bool, bit: usize, distance: i32) -> serde_json::Value {
    let mut w = World::new(10, 4, 24);
    for x in 0..10 {
        for z in 0..24 {
            set(&mut w, Position::new(x, 0, z), BlockKind::Solid, None);
        }
    }
    let input = Position::new(5, 2, 3);
    set(&mut w, input.down(), BlockKind::Solid, None);
    set(&mut w, input, BlockKind::Lever, Some(Facing::North));
    if comparator {
        set(
            &mut w,
            Position::new(5, 1, 4),
            BlockKind::Comparator,
            Some(Facing::East),
        );
        set(
            &mut w,
            Position::new(6, 1, 4),
            BlockKind::WallTorch,
            Some(Facing::West),
        );
        set(&mut w, Position::new(7, 1, 4), BlockKind::Solid, None);
        set(
            &mut w,
            Position::new(4, 1, 4),
            BlockKind::RedstoneWire,
            None,
        );
    } else {
        set(
            &mut w,
            Position::new(5, 1, 4),
            BlockKind::Repeater,
            Some(Facing::North),
        );
        set(&mut w, Position::new(5, 1, 5), BlockKind::Solid, None);
        set(
            &mut w,
            Position::new(4, 1, 5),
            BlockKind::WallTorch,
            Some(Facing::West),
        );
    }
    let start_z = if comparator { 4 } else { 5 };
    for z in start_z..=5 + distance {
        set(
            &mut w,
            Position::new(3, 1, z),
            BlockKind::RedstoneWire,
            None,
        );
    }
    let lamps = [0, distance / 2, distance].map(|offset| Position::new(1, 1, 5 + offset));
    for lamp in lamps {
        set(
            &mut w,
            Position::new(2, 1, lamp.z),
            BlockKind::RedstoneWire,
            None,
        );
        set(&mut w, lamp, BlockKind::Lamp, None);
    }
    let driver = Position::new(3, 1, 5 + distance);
    let mut sim = Simulator::new(w);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        lamps
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, format!("lamp{i}")))
            .chain([(driver, "driver".into())]),
    );
    let mut correct = true;
    let mut phases = Vec::new();
    for from in 0..4 {
        for to in 0..4 {
            for address in [from, to] {
                sim.reset_observer();
                let start = sim.current_tick();
                let high = address & (1 << bit) != 0;
                let mut state = sim.world().get(input.x, input.y, input.z).clone();
                state.lit = high;
                sim.world_mut().set(input.x, input.y, input.z, state);
                let ticks = sim.run_until_stable(2000).unwrap();
                let values = lamps.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                correct &= values.iter().all(|v| *v == !high);
                phases.push(json!({"from":from,"to":to,"address":address,"ticks":ticks,"lamps":values,"driver_power":sim.world().get(driver.x,driver.y,driver.z).power,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"comparator":comparator,"bit":bit,"distance":distance,"correct":correct,"phases":phases})
}
fn main() {
    for bit in 0..2 {
        for distance in [4, 8, 12, 16] {
            let control = run(false, bit, distance);
            let candidate = run(true, bit, distance);
            assert_eq!(control, run(false, bit, distance));
            assert_eq!(candidate, run(true, bit, distance));
            println!("{}", json!({"control":control,"candidate":candidate}));
        }
    }
}
