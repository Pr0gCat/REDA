//! Manual physical mechanism probe, not stock-planner or Minecraft qualification.
use reda::redstone::{
    simulator::{position::Position, Simulator},
    world::{
        block::{BlockKind, BlockState, Face, Facing},
        storage::World,
    },
};

fn block(kind: BlockKind) -> BlockState {
    let mut b = BlockState::air();
    b.kind = kind;
    b.name = match kind {
        BlockKind::Solid => "minecraft:stone",
        BlockKind::RedstoneWire => "minecraft:redstone_wire",
        BlockKind::Lever => "minecraft:lever",
        BlockKind::Repeater => "minecraft:repeater",
        BlockKind::WallTorch => "minecraft:redstone_wall_torch",
        _ => unreachable!(),
    }
    .into();
    b
}

fn endpoint(weak: bool) -> World {
    let mut w = World::new(12, 4, 11);
    for x in (1..5).chain(6..10) {
        w.set(x, 0, 5, block(BlockKind::Solid));
        w.set(x, 1, 5, block(BlockKind::RedstoneWire));
    }
    for x in [1, 9] {
        let mut lever = block(BlockKind::Lever);
        lever.face = Some(Face::Floor);
        lever.facing = Some(Facing::North);
        w.set(x, 1, 5, lever);
    }
    let mut r = block(BlockKind::Repeater);
    r.facing = Some(Facing::West);
    r.delay = 1;
    w.set(if weak { 3 } else { 4 }, 1, 5, r);
    w.set(5, 1, 5, block(BlockKind::Solid));
    let mut t = block(BlockKind::WallTorch);
    t.facing = Some(Facing::North);
    t.lit = true;
    w.set(5, 1, 4, t);
    w
}

fn main() {
    let mut sims = [
        Simulator::new(endpoint(false)),
        Simulator::new(endpoint(true)),
    ];
    for sim in &mut sims {
        sim.run_until_stable(2000).unwrap();
        sim.attach_observer([
            (Position::new(5, 1, 4), "gate".to_owned()),
            (Position::new(3, 1, 5), "a_previous".to_owned()),
            (Position::new(4, 1, 5), "a_landing".to_owned()),
            (Position::new(6, 1, 5), "b_landing".to_owned()),
        ]);
    }
    let mut witnessed_backfeed = 0;
    let mut mismatches = 0;
    let mut records = Vec::new();
    let mut phases = Vec::new();
    for from in 0..4usize {
        for to in 0..4usize {
            let mut traces = Vec::new();
            for (arm, sim) in sims.iter_mut().enumerate() {
                for (phase, vector) in [from, to].into_iter().enumerate() {
                    for (bit, x) in [1, 9].into_iter().enumerate() {
                        let mut lever = sim.world().get(x, 1, 5).clone();
                        lever.lit = vector & (1 << bit) != 0;
                        sim.world_mut().set(x, 1, 5, lever);
                    }
                    sim.reset_observer();
                    let start = sim.current_tick();
                    let settling = sim.run_until_stable(2000).unwrap();
                    phases.push(serde_json::json!({"arm":arm,"from":from,"to":to,"phase":phase,"vector":vector,"start_tick":start,"settle_ticks":settling,
                        "events":sim.observations().iter().map(|e| serde_json::json!({"label":e.label,"tick":e.tick-start,"value":e.value})).collect::<Vec<_>>() }));
                    assert_eq!(
                        sim.world().get(5, 1, 4).lit,
                        vector == 0,
                        "NOR arm={arm},from={from},to={to},phase={phase}"
                    );
                    let b_wire = sim.world().get(6, 1, 5).power > 0;
                    if arm == 1 {
                        assert_eq!(
                            b_wire,
                            vector & 2 != 0,
                            "weak arm must preserve the independent b wire"
                        );
                    } else if vector == 1 {
                        assert!(
                            b_wire,
                            "control must actually reproduce reverse support power"
                        );
                        witnessed_backfeed += 1;
                    }
                    if phase == 1 {
                        traces.push(
                            sim.observations()
                                .iter()
                                .filter(|e| e.label == "gate")
                                .map(|e| (e.tick - start, e.value))
                                .collect::<Vec<_>>(),
                        );
                    }
                }
            }
            let different = traces[0] != traces[1];
            mismatches += usize::from(different);
            records.push(serde_json::json!({"from":from,"to":to,"control":traces[0],"weak":traces[1],"different":different}));
        }
    }
    assert_eq!(witnessed_backfeed, 8);
    let report = serde_json::json!({"physical_truth_correct":true,"candidate_b_wire_always_correct":true,"control_backfeed_phases":witnessed_backfeed,"different_target_traces":mismatches,"target_transitions_per_arm":16,"setup_phases_per_arm":16,"traces":records,"phases":phases});
    let baseline: serde_json::Value = serde_json::from_str(include_str!(
        "../docs/generator-weak-endpoint-comparison-2026-09-30.json"
    ))
    .unwrap();
    let mut summary = report.clone();
    summary.as_object_mut().unwrap().remove("phases");
    assert_eq!(
        summary, baseline,
        "instrumentation must preserve the independently frozen output events"
    );
    println!("{report}");
}
