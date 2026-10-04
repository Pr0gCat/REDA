//! Read a decoded player schematic without replacing its physical blocks.
//! JSON input is produced by the documented research NBT inspection script.
use reda::{
    formats::litematic::parse_block_name,
    redstone::{
        simulator::{position::Position, Simulator},
        world::{block::BlockKind, storage::World},
    },
};
use serde_json::{json, Value};
use std::{collections::HashMap, fs};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let j: Value = serde_json::from_slice(&fs::read(&args[1]).unwrap()).unwrap();
    let family = &args[2];
    assert!(["decode002", "decode003", "encode003"].contains(&family.as_str()));
    let size: Vec<_> = j["size"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap() as i32)
        .collect();
    let mut w = World::new(size[0], size[1], size[2]);
    let mut inputs = Vec::new();
    for entry in j["blocks"].as_array().unwrap() {
        let xyz: Vec<_> = entry["at"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap() as i32)
            .collect();
        let at = Position::new(xyz[0], xyz[1], xyz[2]);
        let s = entry["state"].as_str().unwrap();
        let (name, props) = s
            .split_once('[')
            .map_or((s, ""), |(n, p)| (n, p.trim_end_matches(']')));
        let properties: HashMap<_, _> = props
            .split(',')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let b = parse_block_name(name, &properties);
        assert!(
            b.kind != BlockKind::Other
                || reda::redstone::rules::taxonomy::flags_of(&b).can_carry_dust(),
            "unmodeled block {s}"
        );
        if b.kind == BlockKind::Lever {
            inputs.push(at);
        }
        w.set(at.x, at.y, at.z, b);
    }
    // Probe the original 3-wide horizontal decoder independently of its 2-wide neighbor.
    if family == "decode003" {
        inputs.retain(|p| p.x == 37);
    }
    inputs.sort_by_key(|p| (p.y, p.z));
    assert_eq!(inputs.len(), 4);
    let outputs: Vec<_> = match family.as_str() {
        "decode002" => [0, 2, 4, 6]
            .into_iter()
            .map(|x| Position::new(x, 8, 0))
            .collect(),
        "decode003" => [23, 26, 29, 32]
            .into_iter()
            .map(|x| Position::new(x, 1, 0))
            .collect(),
        "encode003" => [0, 2, 4, 6]
            .into_iter()
            .map(|x| Position::new(x, 1, 0))
            .collect(),
        _ => unreachable!(),
    };
    let drivers: Vec<_> = outputs.iter().map(|p| Position::new(p.x, p.y, 1)).collect();
    let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&w);
    let mut sim = Simulator::new(w);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        outputs
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, format!("lamp{i}")))
            .chain(
                drivers
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (*p, format!("driver{i}"))),
            ),
    );
    let mut correct = true;
    let mut phases = Vec::new();
    for from in 0..16 {
        for to in 0..16 {
            for (phase, address) in [from, to].into_iter().enumerate() {
                sim.reset_observer();
                let start = sim.current_tick();
                for (bit, p) in inputs.iter().enumerate() {
                    let mut b = sim.world().get(p.x, p.y, p.z).clone();
                    b.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, b);
                }
                let ticks = sim.run_until_stable(2000).unwrap();
                let actual: Vec<_> = outputs
                    .iter()
                    .map(|p| sim.world().get(p.x, p.y, p.z).lit)
                    .collect();
                let expected: Vec<_> = (0..4)
                    .map(|bit| match family.as_str() {
                        "decode002" => address == 5,
                        "decode003" => address == if bit % 2 == 0 { 5 } else { 10 },
                        "encode003" => address != 0,
                        _ => unreachable!(),
                    })
                    .collect();
                let actual_driver: Vec<_> = drivers
                    .iter()
                    .map(|p| {
                        let b = sim.world().get(p.x, p.y, p.z);
                        if b.kind == BlockKind::RedstoneWire {
                            b.power > 0
                        } else {
                            b.lit
                        }
                    })
                    .collect();
                correct &= actual == expected && actual_driver == expected;
                phases.push(json!({"from":from,"to":to,"phase":phase,"address":address,"ticks":ticks,"expected":expected,"actual":actual,"driver":actual_driver,
            "events":sim.observations().iter().map(|e|json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>()}));
            }
        }
    }
    println!(
        "{}",
        json!({"family":family,"original_schematic_sha256":j["sha256"],"fingerprint":fingerprint,"certified":false,
        "correct":correct,"inputs":inputs.iter().map(|p|[p.x,p.y,p.z]).collect::<Vec<_>>(),
        "outputs":outputs.iter().map(|p|[p.x,p.y,p.z]).collect::<Vec<_>>(),"phases":phases})
    );
    if !correct {
        std::process::exit(1);
    }
}
