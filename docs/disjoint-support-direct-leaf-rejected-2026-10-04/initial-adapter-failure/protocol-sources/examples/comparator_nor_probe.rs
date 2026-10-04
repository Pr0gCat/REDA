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
fn run_nor(swapped: bool) -> serde_json::Value {
    let mut w = World::new(16, 4, 16);
    for x in 0..16 {
        for z in 0..16 {
            set(&mut w, Position::new(x, 0, z), BlockKind::Solid, None);
        }
    }
    let a = Position::new(8, 2, 7);
    let b = Position::new(8, 2, 5);
    for p in [a, b] {
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::Lever, Some(Facing::North));
    }
    set(
        &mut w,
        Position::new(8, 1, 6),
        BlockKind::Comparator,
        Some(Facing::East),
    );
    set(
        &mut w,
        Position::new(7, 1, 6),
        BlockKind::RedstoneWire,
        None,
    );
    for z in 6..=12 {
        set(
            &mut w,
            Position::new(6, 1, z),
            BlockKind::RedstoneWire,
            None,
        );
    }
    let drivers = [6, 9, 12].map(|z| Position::new(5, 1, z));
    let lamps = [6, 9, 12].map(|z| Position::new(4, 1, z));
    for p in drivers {
        set(&mut w, p, BlockKind::RedstoneWire, None);
    }
    for p in lamps {
        set(&mut w, p, BlockKind::Lamp, None);
    }
    set(&mut w, Position::new(10, 1, 6), BlockKind::Solid, None);
    set(
        &mut w,
        Position::new(9, 1, 6),
        BlockKind::WallTorch,
        Some(Facing::West),
    );
    let inputs = if swapped { [b, a] } else { [a, b] };
    let mut sim = Simulator::new(w);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        drivers
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, format!("driver{i}")))
            .chain(
                lamps
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| (p, format!("lamp{i}"))),
            )
            .chain([(Position::new(8, 1, 6), "compound".into())]),
    );
    let mut correct = true;
    let mut phases = Vec::new();
    for from in 0..4 {
        for to in 0..4 {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (input, p) in inputs.into_iter().enumerate() {
                    let mut state = sim.world().get(p.x, p.y, p.z).clone();
                    state.lit = address & (1 << input) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, state);
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                let driver = drivers.map(|p| sim.world().get(p.x, p.y, p.z).power > 0);
                let lamp = lamps.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                let expected = [address == 0; 3];
                assert!(
                    sim.world().get(9, 1, 6).lit,
                    "reference torch must remain lit"
                );
                correct &= driver == expected && lamp == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,"expected":expected,"driver":driver,"lamp":lamp,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"function":"nor","swapped":swapped,"correct":correct,"certified":false,"phases":phases})
}
fn main() {
    for swapped in [false, true] {
        let result = run_nor(swapped);
        assert!(result["correct"].as_bool().unwrap());
        assert_eq!(result, run_nor(swapped));
        println!("{}", result);
    }
}
