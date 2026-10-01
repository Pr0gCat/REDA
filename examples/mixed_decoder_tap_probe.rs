//! Uncertified physical tap experiment; never selected by the compiler.
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::{
        block::{BlockKind, BlockState, Face, Facing},
        storage::World,
    },
};
use serde_json::json;

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
fn build(relay: bool, inverted: bool) -> (World, Position, Position, Position) {
    let mut w = World::new(10, 8, 6);
    let input = Position::new(2, 5, 2);
    wire(&mut w, input);
    put(&mut w, input, BlockKind::Lever);
    let output = if relay {
        assert!(!inverted);
        wire(&mut w, Position::new(3, 5, 2));
        wire(&mut w, Position::new(4, 4, 2));
        wire(&mut w, Position::new(5, 4, 2));
        put(&mut w, Position::new(5, 4, 2), BlockKind::Repeater);
        put(&mut w, Position::new(6, 4, 2), BlockKind::Solid);
        let out = Position::new(6, 3, 2);
        wire(&mut w, out);
        out
    } else {
        put(&mut w, Position::new(3, 4, 2), BlockKind::WallTorch);
        wire(&mut w, Position::new(3, 3, 2));
        if inverted {
            Position::new(3, 3, 2)
        } else {
            put(&mut w, Position::new(4, 2, 2), BlockKind::WallTorch);
            wire(&mut w, Position::new(4, 1, 2));
            Position::new(4, 1, 2)
        }
    };
    for dz in 1..=2 {
        wire(&mut w, Position::new(output.x, output.y, output.z + dz));
    }
    let lamp = Position::new(output.x, output.y, output.z + 3);
    put(
        &mut w,
        Position::new(lamp.x, lamp.y - 1, lamp.z),
        BlockKind::Solid,
    );
    put(&mut w, lamp, BlockKind::Lamp);
    (w, input, output, lamp)
}
fn probe(relay: bool, inverted: bool) -> serde_json::Value {
    let (world, input, output, lamp) = build(relay, inverted);
    let fp = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    assert_eq!(fp(&world), fp(&build(relay, inverted).0));
    let fingerprint = fp(&world);
    let mut sim = Simulator::new(world);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer([(output, "driver".into()), (lamp, "lamp".into())]);
    let mut phases = Vec::new();
    let mut correct = true;
    for from in 0..2 {
        for to in 0..2 {
            for (phase, value) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                let mut b = sim.world().get(input.x, input.y, input.z).clone();
                b.lit = value != 0;
                sim.world_mut().set(input.x, input.y, input.z, b);
                let ticks = sim.run_until_stable(2000).unwrap();
                let expected = (value != 0) ^ inverted;
                let driver = sim.world().get(output.x, output.y, output.z).power > 0;
                let lit = sim.world().get(lamp.x, lamp.y, lamp.z).lit;
                correct &= driver == expected && lit == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"ticks":ticks,"expected":expected,"driver":driver,"lamp":lit,
            "events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"relay":relay,"inverted":inverted,"fingerprint":fingerprint,"correct":correct,"certified":false,"phases":phases})
}
fn main() {
    if std::env::args().any(|a| a == "--matrix-witness") {
        println!(
            "{}",
            json!({"words":[1,0,0,0],"control":matrix_probe([1,0,0,0],false),"candidate":matrix_probe([1,0,0,0],true)})
        );
        return;
    }
    if std::env::args().any(|a| a == "--matrix") {
        let mut failed = false;
        for encoding in 0..4096u16 {
            let words = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
            let control = matrix_probe(words, false);
            let candidate = matrix_probe(words, true);
            failed |= control["correct"] != true || candidate["correct"] != true;
            println!(
                "{}",
                json!({"encoding":encoding,"words":words,"control":control,"candidate":candidate})
            );
        }
        if failed {
            std::process::exit(1);
        }
        return;
    }
    let results = [probe(false, false), probe(true, false), probe(false, true)];
    println!("{}", json!({"controls_and_candidate":results}));
    if results.iter().any(|r| r["correct"] != true) {
        std::process::exit(1);
    }
}

// Independent full-minterm experiment, not the qualified production matrix.
fn matrix(words: [u8; 4], mixed: bool) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let mut w = World::new(28, 8, 20);
    let inputs = [Position::new(2, 5, 1), Position::new(8, 5, 1)];
    for x in if mixed { vec![2, 8] } else { vec![2, 5, 8, 11] } {
        for z in 1..=12 {
            wire(&mut w, Position::new(x, 5, z));
        }
        put(&mut w, Position::new(x, 5, 2), BlockKind::Repeater);
        let mut r = w.get(x, 5, 2).clone();
        r.facing = Some(Facing::North);
        // Equalize the shared true/inverse rail source stage in the control.
        r.delay = if !mixed && [2, 8].contains(&x) { 2 } else { 1 };
        w.set(x, 5, 2, r);
    }
    for input in inputs {
        put(&mut w, input, BlockKind::Lever);
        if !mixed {
            put(
                &mut w,
                Position::new(input.x + 1, 4, 1),
                BlockKind::WallTorch,
            );
            wire(&mut w, Position::new(input.x + 2, 4, 1));
        }
    }
    for (address, word) in words.into_iter().enumerate() {
        let z = 3 + 3 * address as i32;
        for x in 3..26 {
            wire(&mut w, Position::new(x, 3, z + 1));
        }
        for (bit, x) in [2, 8].into_iter().enumerate() {
            let expects_one = address & (1 << bit) != 0;
            let tap_x = if mixed && !expects_one {
                wire(&mut w, Position::new(x + 1, 5, z));
                wire(&mut w, Position::new(x + 2, 4, z));
                wire(&mut w, Position::new(x + 3, 4, z));
                put(&mut w, Position::new(x + 3, 4, z), BlockKind::Repeater);
                put(&mut w, Position::new(x + 4, 4, z), BlockKind::Solid);
                x + 4
            } else {
                let literal_x = if expects_one { x } else { x + 3 };
                put(
                    &mut w,
                    Position::new(literal_x + 1, 4, z),
                    BlockKind::WallTorch,
                );
                literal_x + 1
            };
            wire(&mut w, Position::new(tap_x, 3, z));
        }
        put(&mut w, Position::new(16, 3, z + 1), BlockKind::Repeater);
        for (bit, x) in [18, 21, 24].into_iter().enumerate() {
            if word & (1 << bit) != 0 {
                let at = Position::new(x, 2, z + 2);
                put(&mut w, at, BlockKind::WallTorch);
                let mut t = w.get(at.x, at.y, at.z).clone();
                t.facing = Some(Facing::South);
                w.set(at.x, at.y, at.z, t);
            }
        }
    }
    let drivers = [18, 21, 24].map(|x| Position::new(x, 1, 16));
    let lamps = [18, 21, 24].map(|x| Position::new(x, 1, 17));
    for x in [18, 21, 24] {
        for z in 4..=16 {
            wire(&mut w, Position::new(x, 1, z));
        }
        put(&mut w, Position::new(x, 0, 17), BlockKind::Solid);
        put(&mut w, Position::new(x, 1, 17), BlockKind::Lamp);
    }
    (w, inputs, drivers, lamps)
}

fn matrix_probe(words: [u8; 4], mixed: bool) -> serde_json::Value {
    let (w, inputs, drivers, lamps) = matrix(words, mixed);
    let fp = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    assert_eq!(fp(&w), fp(&matrix(words, mixed).0));
    let fingerprint = fp(&w);
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
    for from in 0..4 {
        for to in 0..4 {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (bit, input) in inputs.into_iter().enumerate() {
                    let mut b = sim.world().get(input.x, input.y, input.z).clone();
                    b.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(input.x, input.y, input.z, b);
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                let actual_driver = drivers.map(|p| sim.world().get(p.x, p.y, p.z).power > 0);
                let actual_lamp = lamps.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                let expected: [bool; 3] =
                    std::array::from_fn(|bit| words[address] & (1 << bit) != 0);
                let correct = actual_driver == expected && actual_lamp == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,
            "expected":expected,"driver":actual_driver,"lamp":actual_lamp,
            "events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
                if !correct {
                    let witness: Vec<_> = (0..4).map(|row| {
                        let z=3+3*row;
                        let cells: Vec<_> = [Position::new(2,5,z),Position::new(8,5,z),
                            Position::new(3,3,z),Position::new(6,3,z),Position::new(9,3,z),Position::new(12,3,z),
                            Position::new(5,4,z),Position::new(11,4,z),Position::new(14,3,z+1),Position::new(18,3,z+1)]
                            .into_iter().map(|p| {
                                let b=sim.world().get(p.x,p.y,p.z);
                                json!({"at":[p.x,p.y,p.z],"kind":format!("{:?}",b.kind),"power":b.power,"lit":b.lit})
                            }).collect();
                        json!({"row":row,"cells":cells})
                    }).collect();
                    let links: Vec<_> = (0..4).flat_map(|row| [2,8].into_iter().map(move |x|(row,x)))
                        .flat_map(|(row,x)| {
                            let z=3+3*row;
                            [(Position::new(x+2,3,z+1),Facing::North),
                             (Position::new(x+2,4,z),Facing::West),
                             (Position::new(x+1,5,z),Facing::West)]
                        }).map(|(from,direction)| {
                            let to=reda::redstone::simulator::connectivity::dust_connects(sim.world(),from,direction);
                            json!({"from":[from.x,from.y,from.z],"direction":format!("{direction:?}"),"to":to.map(|p|[p.x,p.y,p.z])})
                        }).collect();
                    return json!({"correct":false,"certified":false,"fingerprint":fingerprint,"phases":phases,"witness":witness,"connections":links});
                }
            }
        }
    }
    json!({"correct":true,"certified":false,"fingerprint":fingerprint,"phases":phases})
}
