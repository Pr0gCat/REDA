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
    if std::env::args().any(|a| {
        a == "--player-matrix"
            || a == "--player-isolated"
            || a == "--player-pruned"
            || a == "--player-restored"
            || a == "--certified-control"
            || a == "--player-driven"
            || a == "--player-typed"
            || a == "--shipping-control"
    }) {
        let args: Vec<_> = std::env::args().collect();
        let start = args.get(2).map(|s| s.parse::<u16>().unwrap()).unwrap_or(0);
        let end = args
            .get(3)
            .map(|s| s.parse::<u16>().unwrap())
            .unwrap_or(4096);
        assert!(start < end && end <= 4096);
        let mut failed = false;
        for encoding in start..end {
            let words = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
            let control = matrix_probe(words, false);
            let pitch = if args[1] != "--player-matrix" { 3 } else { 2 };
            let candidate = matrix_probe_world(
                words,
                false,
                true,
                pitch,
                args[1] == "--player-pruned"
                    || args[1] == "--player-restored"
                    || args[1] == "--certified-control"
                    || args[1] == "--player-driven",
                if [
                    "--player-restored",
                    "--certified-control",
                    "--player-driven",
                ]
                .contains(&args[1].as_str())
                {
                    1
                } else {
                    0
                },
            );
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
    matrix_probe_world(words, mixed, false, 2, false, 0)
}

// Player DECODE003's 3-wide stencil, lifted above a programmable torch matrix.
// No existing player world is pasted and no source/module name is recognized.
fn player_matrix(
    words: [u8; 4],
    pitch: i32,
    prune: bool,
    source_delay: u8,
) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    assert!([2, 3].contains(&pitch));
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
    let mut word_lines = Vec::new();
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
        word_lines.push(Position::new(x, 3, 8));
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
    if std::env::args().any(|a| a == "--player-driven") {
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
    if pitch == 3 {
        let mut domains = address_z.map(|z| Position::new(14, 5, z)).to_vec();
        domains.extend(word_lines);
        domains.extend(drivers);
        verify_player_isolation(&w, &domains);
    }
    (w, inputs, drivers, lamps)
}

// Necessary local guards for this stencil, not a sealed physical certificate.
fn verify_player_isolation(w: &World, domains: &[Position]) {
    use reda::redstone::{
        rules::taxonomy::{power_emitted_toward, PowerOutput},
        simulator::{
            component::torch_support_position, connectivity::dust_connections, position::HORIZONTAL,
        },
    };
    use std::collections::{HashMap, HashSet, VecDeque};
    let mut owners = HashMap::new();
    for (id, &start) in domains.iter().enumerate() {
        assert_eq!(
            w.get(start.x, start.y, start.z).kind,
            BlockKind::RedstoneWire
        );
        let mut queue = VecDeque::from([start]);
        let mut visited = HashSet::new();
        while let Some(at) = queue.pop_front() {
            if !visited.insert(at) {
                continue;
            }
            if let Some(old) = owners.insert(at, id) {
                assert_eq!(old, id, "dust bypass between stencil domains at {at:?}");
            }
            for direction in HORIZONTAL {
                queue.extend(dust_connections(w, at, direction).iter());
            }
        }
    }
    let torches: Vec<_> = w
        .positions_of(BlockKind::WallTorch)
        .map(|flat| {
            let (x, y, z) = w.decode(flat);
            Position::new(x, y, z)
        })
        .collect();
    for &from in &torches {
        let state = w.get(from.x, from.y, from.z);
        for &to in &torches {
            if from == to {
                continue;
            }
            let support = torch_support_position(w.get(to.x, to.y, to.z), to).unwrap();
            for direction in HORIZONTAL.into_iter().chain([Facing::Up, Facing::Down]) {
                if from.offset(direction) == support {
                    assert_eq!(
                        power_emitted_toward(state, direction),
                        PowerOutput::INERT,
                        "foreign tap can power another torch support: {from:?} -> {support:?}"
                    );
                }
            }
        }
    }
}

fn matrix_probe_world(
    words: [u8; 4],
    mixed: bool,
    player: bool,
    pitch: i32,
    prune: bool,
    source_delay: u8,
) -> serde_json::Value {
    let shipping_control = !player && std::env::args().any(|a| a == "--shipping-control");
    let typed_player = player
        && std::env::args().any(|a| ["--player-typed", "--shipping-control"].contains(&a.as_str()));
    let certified_control = !player
        && std::env::args().any(|a| {
            ["--certified-control", "--player-driven", "--player-typed"].contains(&a.as_str())
        });
    let build = || {
        if typed_player {
            certified_matrix_impl(words, true)
        } else if player {
            player_matrix(words, pitch, prune, source_delay)
        } else if shipping_control {
            shipping_matrix(words)
        } else if certified_control {
            certified_matrix(words)
        } else {
            matrix(words, mixed)
        }
    };
    let (w, inputs, drivers, lamps) = build();
    let fp = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    assert_eq!(fp(&w), fp(&build().0));
    let fingerprint = fp(&w);
    let internal_truth = if typed_player {
        let result = certified_result(words, true);
        assert_eq!(fingerprint, fp(result.physical().world()));
        Some((
            result.implemented().clone(),
            result
                .implemented_observations()
                .gate_output_positions
                .clone(),
        ))
    } else {
        None
    };
    let mut internal_sites = Vec::new();
    if player && std::env::args().any(|a| a == "--internal") {
        let (sx, sy, sz) = w.size();
        for x in 0..sx {
            for y in 0..sy {
                for z in 0..sz {
                    if matches!(
                        w.get(x, y, z).kind,
                        BlockKind::WallTorch | BlockKind::Repeater
                    ) {
                        internal_sites
                            .push((Position::new(x, y, z), format!("internal{x}_{y}_{z}")));
                    }
                }
            }
        }
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
            )
            .chain(internal_sites),
    );
    let mut phases = Vec::new();
    let mut all_correct = true;
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
                if let Some((netlist, sites)) = &internal_truth {
                    let mut values: std::collections::HashMap<String, bool> = netlist
                        .inputs
                        .iter()
                        .enumerate()
                        .map(|(bit, name)| (name.clone(), address & (1 << bit) != 0))
                        .collect();
                    for index in netlist.combinational_order().unwrap() {
                        let gate = &netlist.gates[index];
                        let expected = gate
                            .kind
                            .evaluate(&gate.inputs.iter().map(|n| values[n]).collect::<Vec<_>>());
                        let &(x, y, z) = &sites[&gate.output];
                        let block = sim.world().get(x, y, z);
                        let actual = if block.kind == BlockKind::RedstoneWire {
                            block.power > 0
                        } else {
                            block.lit
                        };
                        assert_eq!(
                            actual, expected,
                            "internal {}: words={words:?}, from={from}, to={to}, phase={phase}",
                            gate.output
                        );
                        values.insert(gate.output.clone(), expected);
                    }
                }
                let actual_driver = drivers.map(|p| {
                    let b = sim.world().get(p.x, p.y, p.z);
                    if b.kind == BlockKind::RedstoneWire {
                        b.power > 0
                    } else {
                        b.lit
                    }
                });
                let actual_lamp = lamps.map(|p| sim.world().get(p.x, p.y, p.z).lit);
                let expected: [bool; 3] =
                    std::array::from_fn(|bit| words[address] & (1 << bit) != 0);
                let correct = actual_driver == expected && actual_lamp == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,
            "expected":expected,"driver":actual_driver,"lamp":actual_lamp,
            "events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
                if !correct {
                    if shipping_control {
                        all_correct = false;
                        continue;
                    }
                    if player {
                        return json!({"correct":false,"certified":false,"fingerprint":fingerprint,"phases":phases});
                    }
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
    json!({"correct":all_correct,"certified":certified_control || typed_player,"fingerprint":fingerprint,"phases":phases})
}

fn certified_matrix(words: [u8; 4]) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    certified_matrix_impl(words, false)
}

fn certified_matrix_impl(
    words: [u8; 4],
    player: bool,
) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let result = certified_result(words, player);
    let positions = |ps: &[(i32, i32, i32)]| {
        ps.iter()
            .map(|&(x, y, z)| Position::new(x, y, z))
            .collect::<Vec<_>>()
    };
    (
        result.physical().world().clone(),
        positions(result.source_input_positions())
            .try_into()
            .unwrap(),
        positions(result.source_output_driver_positions())
            .try_into()
            .unwrap(),
        positions(result.source_output_positions())
            .try_into()
            .unwrap(),
    )
}

fn certified_result(
    words: [u8; 4],
    player: bool,
) -> reda::compile::fragment_synth::rewrite::CertifiedPhysicalRewrite {
    let source = native_source(words);
    if player {
        reda::compile::fragment_synth::torch_row::compile_player_torch_row(&source, 1_000_000)
    } else {
        reda::compile::fragment_synth::torch_row::compile_torch_row(&source, 1_000_000)
    }
    .unwrap()
}

fn native_source(words: [u8; 4]) -> reda::compile::Netlist {
    use reda::{
        compile::lowering::lower_optimised,
        frontend::{compile_systemverilog, CompileOptions, SourceInput},
    };
    let mut text = String::from("module source_rom(input logic a,b,output logic y0,y1,y2);\n");
    for bit in 0..3 {
        let values = words.map(|word| (word >> bit) & 1);
        text.push_str(&format!(
            "assign y{bit} = b ? (a ? 1'b{} : 1'b{}) : (a ? 1'b{} : 1'b{});\n",
            values[3], values[2], values[1], values[0]
        ));
    }
    text.push_str("endmodule");
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "source_rom.sv",
            text: &text,
        }],
        &CompileOptions::new("source_rom"),
    )
    .unwrap();
    let ports = artifact.output_map();
    let mut source = lower_optimised(&artifact.netlist).unwrap();
    source.outputs = (0..3)
        .map(|bit| ports[&format!("y{bit}")].clone())
        .collect();
    source
}

fn shipping_matrix(words: [u8; 4]) -> (World, [Position; 2], [Position; 3], [Position; 3]) {
    let mut source = native_source(words);
    let public = source.outputs.clone();
    let mut seen = std::collections::BTreeSet::new();
    source.outputs.retain(|n| seen.insert(n.clone()));
    let c = reda::compile::compile(&source).unwrap();
    let position = |(x, y, z)| Position::new(x, y, z);
    let inputs = source
        .inputs
        .iter()
        .map(|n| position(c.input_positions[n]))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let drivers = public
        .iter()
        .map(|n| {
            position(
                *c.gate_output_positions
                    .get(n)
                    .or_else(|| c.input_positions.get(n))
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let lamps = public
        .iter()
        .map(|n| position(c.output_positions[n]))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    (c.world, inputs, drivers, lamps)
}
