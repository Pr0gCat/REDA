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

fn run(polarity: u8, swapped: bool) -> serde_json::Value {
    let mut w = World::new(18, 4, 12);
    let inputs = std::array::from_fn::<_, 2, _>(|bit| {
        let z = 4 + 4 * bit as i32;
        if polarity & (1 << bit) != 0 {
            Position::new(12, 2, z)
        } else {
            Position::new(11, 3, z + 1)
        }
    });
    for (bit, p) in inputs.into_iter().enumerate() {
        let z = 4 + 4 * bit as i32;
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::Lever, Some(Facing::North));
        let producer = Position::new(11, 2, z);
        set(&mut w, producer.down(), BlockKind::Solid, None);
        set(
            &mut w,
            producer,
            if polarity & (1 << bit) != 0 {
                BlockKind::Repeater
            } else {
                BlockKind::Comparator
            },
            Some(Facing::East),
        );
        if polarity & (1 << bit) == 0 {
            set(&mut w, Position::new(13, 2, z), BlockKind::Solid, None);
            set(
                &mut w,
                Position::new(12, 2, z),
                BlockKind::WallTorch,
                Some(Facing::West),
            );
        }
        for x in 3..=6 {
            let d = Position::new(x, 1, z);
            set(&mut w, d.down(), BlockKind::Solid, None);
            set(&mut w, d, BlockKind::RedstoneWire, None);
        }
        for x in 8..=10 {
            let d = Position::new(x, 2, z);
            set(&mut w, d.down(), BlockKind::Solid, None);
            set(&mut w, d, BlockKind::RedstoneWire, None);
        }
        let r = Position::new(7, 1, z);
        set(&mut w, r.down(), BlockKind::Solid, None);
        set(&mut w, r, BlockKind::Repeater, Some(Facing::East));
    }
    for z in 5..=7 {
        let p = Position::new(3, 1, z);
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::RedstoneWire, None);
    }
    let legacy_alias = std::env::args().any(|arg| arg == "--legacy-alias");
    let drivers = [
        Position::new(2, 1, 6),
        Position::new(9, 2, 3),
        if legacy_alias {
            Position::new(10, 2, 9)
        } else {
            Position::new(9, 2, 7)
        },
    ];
    let lamps = [
        Position::new(1, 1, 6),
        Position::new(9, 2, 2),
        if legacy_alias {
            Position::new(10, 2, 10)
        } else {
            Position::new(9, 2, 6)
        },
    ];
    for p in drivers {
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::RedstoneWire, None);
    }
    for p in lamps {
        set(&mut w, p.down(), BlockKind::Solid, None);
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
                let a = a == (polarity & 1 != 0);
                let b = b == (polarity & 2 != 0);
                let expected = [a || b, a, b];
                let driver = drivers.map(|p| sim.world().get(p.x, p.y, p.z).power > 0);
                let lamp = lamps.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                correct &= driver == expected && lamp == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,"driver":driver,"lamp":lamp,"expected":expected,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"polarity":polarity,"swapped":swapped,"legacy_alias":legacy_alias,"certified":false,"correct":correct,"phases":phases})
}
fn main() {
    let results: Vec<_> = (0..3)
        .flat_map(|polarity| [run(polarity, false), run(polarity, true)])
        .collect();
    println!("{}", json!({"results":results}));
    if results.iter().any(|r| r["correct"] != true) {
        std::process::exit(1);
    }
}
