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
fn run(comparator: bool, input_bit: usize, mutation: Option<&str>) -> serde_json::Value {
    let mut w = World::new(8, 4, 8);
    for x in 0..8 {
        for z in 0..8 {
            set(&mut w, Position::new(x, 0, z), BlockKind::Solid, None);
        }
    }
    let mut input = Position::new(3, 2, 2);
    set(&mut w, Position::new(3, 1, 2), BlockKind::Solid, None);
    set(&mut w, input, BlockKind::Lever, Some(Facing::North));
    let (driver, lamp) = if comparator {
        set(
            &mut w,
            Position::new(3, 1, 3),
            BlockKind::Comparator,
            Some(Facing::East),
        );
        set(
            &mut w,
            Position::new(4, 1, 3),
            BlockKind::WallTorch,
            Some(Facing::West),
        );
        set(&mut w, Position::new(5, 1, 3), BlockKind::Solid, None);
        set(
            &mut w,
            Position::new(2, 1, 3),
            BlockKind::RedstoneWire,
            None,
        );
        set(&mut w, Position::new(1, 1, 3), BlockKind::Lamp, None);
        (Position::new(3, 1, 3), Position::new(1, 1, 3))
    } else {
        set(
            &mut w,
            Position::new(3, 1, 3),
            BlockKind::Repeater,
            Some(Facing::North),
        );
        set(&mut w, Position::new(3, 1, 4), BlockKind::Solid, None);
        set(
            &mut w,
            Position::new(2, 1, 4),
            BlockKind::WallTorch,
            Some(Facing::West),
        );
        set(
            &mut w,
            Position::new(1, 1, 4),
            BlockKind::RedstoneWire,
            None,
        );
        set(&mut w, Position::new(0, 1, 4), BlockKind::Lamp, None);
        (Position::new(2, 1, 4), Position::new(0, 1, 4))
    };
    if let Some(mutation) = mutation {
        assert!(comparator);
        match mutation {
            "compare_mode" => {
                let mut b = w.get(3, 1, 3).clone();
                b.extra_properties.insert("mode".into(), "compare".into());
                w.set(3, 1, 3, b);
            }
            "direct_side_lever" => {
                w.set(input.x, input.y, input.z, BlockState::air());
                input = Position::new(3, 1, 2);
                set(&mut w, input, BlockKind::Lever, Some(Facing::North));
            }
            "missing_rear" => w.set(4, 1, 3, BlockState::air()),
            "reversed_facing" => {
                let mut b = w.get(3, 1, 3).clone();
                b.facing = Some(Facing::West);
                w.set(3, 1, 3, b);
            }
            _ => panic!("unknown mutation"),
        }
    }
    let mut correct = true;
    let mut sim = Simulator::new(w);
    sim.run_until_stable(2000).unwrap();
    let public_driver = if comparator {
        Position::new(2, 1, 3)
    } else {
        Position::new(1, 1, 4)
    };
    sim.attach_observer([
        (public_driver, "driver".to_string()),
        (lamp, "lamp".to_string()),
    ]);
    let mut phases = Vec::new();
    for from in 0..4 {
        for to in 0..4 {
            for phase in 0..2 {
                sim.reset_observer();
                let start = sim.current_tick();
                let address = if phase == 0 { from } else { to };
                let high = address & (1 << input_bit) != 0;
                let mut state = sim.world().get(input.x, input.y, input.z).clone();
                state.lit = high;
                sim.world_mut().set(input.x, input.y, input.z, state);
                let ticks = sim.run_until_stable(2000).unwrap();
                let d = sim.world().get(driver.x, driver.y, driver.z);
                let actual = if comparator { d.power > 0 } else { d.lit };
                let lamp_value = sim.world().get(lamp.x, lamp.y, lamp.z).lit;
                correct &= actual == !high && lamp_value == !high;
                phases.push(json!({"from":from,"to":to,"phase":phase,"ticks":ticks,"driver":actual,"lamp":lamp_value,"events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    json!({"comparator":comparator,"input_bit":input_bit,"correct":correct,"mutation":mutation,"phases":phases})
}
fn main() {
    for bit in 0..2 {
        let control = run(false, bit, None);
        let candidate = run(true, bit, None);
        assert_eq!(control["correct"], true);
        assert_eq!(candidate["correct"], true);
        let mutations: Vec<_> = [
            "compare_mode",
            "direct_side_lever",
            "missing_rear",
            "reversed_facing",
        ]
        .into_iter()
        .map(|m| {
            let result = run(true, bit, Some(m));
            assert_eq!(
                result["correct"], false,
                "mutation {m} must violate inverter truth"
            );
            result
        })
        .collect();
        println!(
            "{}",
            json!({"control":control,"candidate":candidate,"mutations":mutations})
        );
    }
}
