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

fn run(swapped: bool) -> serde_json::Value {
    let mut w = World::new(18, 5, 12);
    for x in 0..18 {
        for z in 0..12 {
            set(&mut w, Position::new(x, 0, z), BlockKind::Solid, None);
        }
    }
    let inputs = [Position::new(9, 2, 6), Position::new(8, 2, 5)];
    for p in inputs {
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::Lever, Some(Facing::North));
    }
    for (p, f) in [
        (Position::new(8, 1, 4), Facing::West),
        (Position::new(8, 1, 6), Facing::East),
        (Position::new(10, 1, 6), Facing::South),
    ] {
        set(&mut w, p, BlockKind::Comparator, Some(f));
    }
    for (p, f, s) in [
        (Position::new(7, 1, 4), Facing::East, Position::new(6, 1, 4)),
        (
            Position::new(10, 1, 7),
            Facing::North,
            Position::new(10, 1, 8),
        ),
    ] {
        set(&mut w, s, BlockKind::Solid, None);
        set(&mut w, p, BlockKind::WallTorch, Some(f));
    }
    set(&mut w, Position::new(8, 1, 5), BlockKind::Solid, None);
    let mut wires = Vec::new();

    for x in 10..=14 {
        wires.push(Position::new(x, 1, 5));
    }
    wires.extend([Position::new(7, 1, 6), Position::new(7, 1, 7)]);
    for x in 3..=7 {
        wires.push(Position::new(x, 1, 8));
    }
    wires.extend([Position::new(9, 1, 4), Position::new(9, 1, 3)]);
    for x in 10..=12 {
        wires.push(Position::new(x, 1, 3));
    }
    for p in wires {
        if p.y > 1 && w.get(p.x, p.y - 1, p.z).kind == BlockKind::Air {
            set(&mut w, p.down(), BlockKind::Solid, None);
        }
        set(&mut w, p, BlockKind::RedstoneWire, None);
    }
    let drivers = [
        Position::new(3, 1, 8),
        Position::new(14, 1, 5),
        Position::new(12, 1, 3),
    ];
    let lamps = [
        Position::new(2, 1, 8),
        Position::new(15, 1, 5),
        Position::new(13, 1, 3),
    ];
    for p in lamps {
        set(&mut w, p, BlockKind::Lamp, None);
    }
    if let Some(cut) = std::env::args().find_map(|a| a.strip_prefix("--cut=").map(str::to_owned)) {
        let c: Vec<i32> = cut.split(',').map(|v| v.parse().unwrap()).collect();
        w.set(c[0], c[1], c[2], BlockState::air());
    }
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
            ),
    );
    let mut phases = Vec::new();
    let mut correct = true;
    for from in 0..4 {
        for to in 0..4 {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (bit, p) in inputs.into_iter().enumerate() {
                    let bit = if swapped { 1 - bit } else { bit };
                    let mut b = sim.world().get(p.x, p.y, p.z).clone();
                    b.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, b);
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                let a = address & (if swapped { 2 } else { 1 }) != 0;
                let b = address & (if swapped { 1 } else { 2 }) != 0;
                let expected = [a && !b, !a, !b];
                let driver = drivers.map(|p| sim.world().get(p.x, p.y, p.z).power > 0);
                let lamp = lamps.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                correct &= driver == expected && lamp == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,"driver":driver,"lamp":lamp,"expected":expected,"diagnostic": ([Position::new(9,1,6),Position::new(8,1,5),Position::new(10,1,6),Position::new(8,1,4)].map(|p| {let s=sim.world().get(p.x,p.y,p.z);let power=reda::redstone::simulator::propagate::block_signal_at(sim.world(),p);json!({"position":[p.x,p.y,p.z],"kind":format!("{:?}",s.kind),"power":s.power,"lit":s.lit,"block_power":format!("{:?}",power)})})),"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"swapped":swapped,"certified":false,"correct":correct,"phases":phases})
}
fn main() {
    let results = [run(false), run(true)];
    println!("{}", json!({"results":results}));
    if results.iter().any(|r| r["correct"] != true) {
        std::process::exit(1);
    }
}
