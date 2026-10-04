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
fn wire(w: &mut World, p: Position) {
    set(w, p.down(), BlockKind::Solid, None);
    set(w, p, BlockKind::RedstoneWire, None);
}
fn run(bit: usize) -> serde_json::Value {
    let mut w = World::new(20, 6, 14);
    let mut inputs = [Position::new(16, 2, 11); 2];
    inputs[bit] = Position::new(7, 2, 2);
    for p in inputs {
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::Lever, Some(Facing::North));
    }
    set(&mut w, Position::new(7, 0, 3), BlockKind::Solid, None);
    set(
        &mut w,
        Position::new(7, 1, 3),
        BlockKind::Comparator,
        Some(Facing::East),
    );
    set(
        &mut w,
        Position::new(8, 1, 3),
        BlockKind::WallTorch,
        Some(Facing::West),
    );
    set(&mut w, Position::new(9, 1, 3), BlockKind::Solid, None);
    wire(&mut w, Position::new(6, 2, 2));
    wire(&mut w, Position::new(6, 3, 3));
    set(&mut w, Position::new(5, 2, 3), BlockKind::Solid, None);
    set(
        &mut w,
        Position::new(5, 3, 3),
        BlockKind::Repeater,
        Some(Facing::East),
    );
    wire(&mut w, Position::new(4, 3, 3));
    for z in 3..=9 {
        wire(&mut w, Position::new(6, 1, z));
        wire(&mut w, Position::new(3, 3, z));
    }
    let negative = [3, 6, 9].map(|z| Position::new(4, 1, z));
    let positive = [3, 6, 9].map(|z| Position::new(1, 3, z));
    for p in negative {
        wire(&mut w, Position::new(5, 1, p.z));
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::Lamp, None);
    }
    for p in positive {
        wire(&mut w, Position::new(2, 3, p.z));
        set(&mut w, p.down(), BlockKind::Solid, None);
        set(&mut w, p, BlockKind::Lamp, None);
    }
    let mut sim = Simulator::new(w);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        negative
            .into_iter()
            .enumerate()
            .map(|(i, p)| (p, format!("negative{i}")))
            .chain(
                positive
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| (p, format!("positive{i}"))),
            )
            .chain([
                (Position::new(8, 1, 3), "constant".into()),
                (Position::new(7, 1, 3), "negative_driver".into()),
                (Position::new(5, 3, 3), "positive_driver".into()),
            ]),
    );
    let mut correct = true;
    let mut phases = Vec::new();
    for from in 0..4 {
        for to in 0..4 {
            for address in [from, to] {
                sim.reset_observer();
                let start = sim.current_tick();
                for (input, p) in inputs.into_iter().enumerate() {
                    let mut b = sim.world().get(p.x, p.y, p.z).clone();
                    b.lit = address & (1 << input) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, b);
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                let high = address & (1 << bit) != 0;
                let neg = negative.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                let pos = positive.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                let constant = sim.world().get(8, 1, 3).lit;
                correct &=
                    neg.iter().all(|v| *v == !high) && pos.iter().all(|v| *v == high) && constant;
                phases.push(json!({"from":from,"to":to,"address":address,"ticks":ticks,"negative":neg,"positive":pos,"constant":constant,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"bit":bit,"correct":correct,"phases":phases})
}
fn main() {
    for bit in 0..2 {
        let result = run(bit);
        assert_eq!(result, run(bit));
        println!("{}", result);
    }
}
