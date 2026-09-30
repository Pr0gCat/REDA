//! One address bit drives a real direct/inverted pair of programmed rows.
use reda::compile::{compile, lowering::lower_optimised};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::{position::Position, Simulator};
use reda::redstone::world::{
    block::{BlockKind, BlockState, Face, Facing},
    storage::World,
};
use serde_json::json;

fn block(kind: BlockKind) -> BlockState {
    let mut b = BlockState::air();
    b.kind = kind;
    b.name = match kind {
        BlockKind::Solid => "minecraft:stone",
        BlockKind::RedstoneWire => "minecraft:redstone_wire",
        BlockKind::WallTorch => "minecraft:redstone_wall_torch",
        BlockKind::Lever => "minecraft:lever",
        BlockKind::Lamp => "minecraft:redstone_lamp",
        _ => unreachable!(),
    }
    .into();
    b
}
fn wire(w: &mut World, x: i32, y: i32, z: i32) {
    w.set(x, y - 1, z, block(BlockKind::Solid));
    w.set(x, y, z, block(BlockKind::RedstoneWire));
}
fn torch(w: &mut World, x: i32, y: i32, z: i32, facing: Facing) {
    let mut b = block(BlockKind::WallTorch);
    b.facing = Some(facing);
    b.lit = true;
    w.set(x, y, z, b);
}
fn matrix(fold_constants: bool) -> (World, Position, Vec<Position>) {
    let mut w = World::new(12, 6, 14);
    for z in [3, 9] {
        for x in 1..10 {
            wire(&mut w, x, 3, z);
        }
    }
    let mut lever = block(BlockKind::Lever);
    lever.face = Some(Face::Floor);
    lever.facing = Some(Facing::North);
    w.set(1, 3, 3, lever);
    torch(&mut w, 0, 2, 3, Facing::West);
    for z in 4..9 {
        wire(&mut w, 0, 2, z);
    }
    wire(&mut w, 0, 3, 9);
    for x in [3, 6, 9] {
        for z in 4..12 {
            wire(&mut w, x, 1, z);
        }
    }
    // Keep the same output indicator load as the existing compiler.
    for x in [3, 6, 9] {
        w.set(x, 0, 12, block(BlockKind::Solid));
        w.set(x, 1, 12, block(BlockKind::Lamp));
    }
    let words = [0b101u8, 0b110];
    let always_high = words.iter().fold(0b111, |a, w| a & w);
    for (z, word) in [3, 9].into_iter().zip(words) {
        for (bit, x) in [3, 6, 9].into_iter().enumerate() {
            if word & (1 << bit) != 0 && !(fold_constants && always_high & (1 << bit) != 0) {
                torch(&mut w, x, 2, z + 1, Facing::South);
            }
        }
    }
    if fold_constants {
        for (bit, x) in [3, 6, 9].into_iter().enumerate() {
            if always_high & (1 << bit) != 0 {
                w.set(x, 2, 12, block(BlockKind::Solid));
                torch(&mut w, x, 2, 11, Facing::North);
            }
        }
    }
    (
        w,
        Position::new(1, 3, 3),
        [3, 6, 9]
            .into_iter()
            .map(|x| Position::new(x, 1, 11))
            .collect(),
    )
}
fn measure(world: World, input: Position, outputs: Vec<Position>) -> serde_json::Value {
    let mut sim = Simulator::new(world);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        outputs
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, format!("bit{i}"))),
    );
    let mut traces = Vec::new();
    let mut worst = 0;
    let mut sum = 0;
    let mut glitches = 0;
    for from in 0..2 {
        for to in 0..2 {
            for (phase, value) in [from, to].into_iter().enumerate() {
                if phase == 1 {
                    sim.reset_observer();
                }
                let mut b = sim.world().get(input.x, input.y, input.z).clone();
                b.lit = value != 0;
                sim.world_mut().set(input.x, input.y, input.z, b);
                let start = sim.current_tick();
                let ticks = sim.run_until_stable(2000).unwrap();
                if phase == 1 {
                    worst = worst.max(ticks);
                    sum += ticks;
                    let word = if to == 0 { 0b101 } else { 0b110 };
                    for (i, p) in outputs.iter().enumerate() {
                        let b = sim.world().get(p.x, p.y, p.z);
                        let actual = if b.kind == BlockKind::RedstoneWire {
                            b.power > 0
                        } else {
                            b.lit
                        };
                        assert_eq!(actual, word & (1 << i) != 0, "{from}->{to} bit{i}");
                        glitches += usize::from(
                            sim.observations()
                                .iter()
                                .filter(|o| o.position == *p)
                                .count()
                                > 1,
                        );
                    }
                    traces.push(json!({"from":from,"to":to,"ticks":ticks,"changes":sim.observations().iter().map(|o|json!({"bit":o.label,"tick":o.tick-start,"value":o.value})).collect::<Vec<_>>()}));
                }
            }
        }
    }
    json!({"worst_settle_ticks":worst,"total_settle_ticks":sum,"glitched_column_transitions":glitches,"traces":traces})
}
#[test]
fn decoded_rows_match_rom_truth_and_record_hazards_against_existing_generator() {
    let source="module rom2(input logic addr, output logic y0,y1); assign y0=~addr; assign y1=addr; endmodule";
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "rom2.sv",
            text: source,
        }],
        &CompileOptions::new("rom2"),
    )
    .unwrap();
    let outputs = artifact.output_map();
    let mut c = compile(&lower_optimised(&artifact.netlist).unwrap()).unwrap();
    let p = c.input_positions["addr"];
    let mut drivers: Vec<_> = ["y0", "y1"]
        .into_iter()
        .map(|n| {
            let p = c.gate_output_positions[&outputs[n]];
            Position::new(p.0, p.1, p.2)
        })
        .collect();
    // Frontend cannot export a constant; add an isolated physical constant
    // reference rather than changing the frontend or disguising it as logic.
    let (sx, sy, sz) = c.world.size();
    let constant = Position::new(sx - 2, sy - 2, sz - 2);
    assert_eq!(
        c.world.get(constant.x, constant.y, constant.z).kind,
        BlockKind::Air
    );
    assert_eq!(
        c.world.get(constant.x, constant.y, constant.z - 1).kind,
        BlockKind::Air
    );
    c.world.set(
        constant.x,
        constant.y,
        constant.z - 1,
        block(BlockKind::Solid),
    );
    torch(
        &mut c.world,
        constant.x,
        constant.y,
        constant.z,
        Facing::South,
    );
    drivers.push(constant);
    let baseline = measure(c.world, Position::new(p.0, p.1, p.2), drivers);
    let (world, input, outputs) = matrix(false);
    let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world);
    assert_eq!(
        fingerprint,
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&matrix(false).0)
    );
    let candidate = measure(world, input, outputs);
    assert_eq!(candidate["glitched_column_transitions"], 1);
    assert_eq!(baseline["glitched_column_transitions"], 0);
    assert_eq!(baseline["worst_settle_ticks"], 8);
    assert_eq!(baseline["total_settle_ticks"], 14);
    let (world, input, outputs) = matrix(true);
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&matrix(true).0)
    );
    let folded = measure(world, input, outputs);
    assert_eq!(folded["glitched_column_transitions"], 0);
    assert!(
        folded["worst_settle_ticks"].as_u64().unwrap()
            <= baseline["worst_settle_ticks"].as_u64().unwrap()
    );
    assert!(
        folded["total_settle_ticks"].as_u64().unwrap()
            <= baseline["total_settle_ticks"].as_u64().unwrap()
    );
    let result = json!({"baseline":baseline,"candidate":candidate,"folded":folded,"candidate_fingerprint":fingerprint});
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/torch_row_decoder.json"),
        serde_json::to_string_pretty(&result).unwrap(),
    )
    .unwrap();
}
