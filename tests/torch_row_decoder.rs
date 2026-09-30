//! Physical probes for programmed rows and literal-rail address decoding.
#[path = "../src/frontend/cover.rs"]
mod cover;
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

fn matrix_four_words(
    words: [u8; 4],
    minimized: bool,
    balanced: bool,
) -> (World, Vec<Position>, Vec<Position>, Vec<Position>) {
    let mut rows = std::collections::BTreeMap::new();
    if minimized {
        for bit in 0..3 {
            let table: Vec<_> = words.iter().map(|w| w & (1 << bit) != 0).collect();
            for cube in cover::cover(&table, true) {
                *rows.entry((cube.bits, cube.mask)).or_insert(0u8) |= 1 << bit;
            }
        }
    } else {
        for (address, word) in words.into_iter().enumerate() {
            rows.insert((address, 3), word);
        }
    }
    if rows.is_empty() {
        rows.insert((0, 0), 0);
    }
    let pitch = if minimized { 3 } else { 4 };
    let last_row = 4 + pitch * (rows.len() as i32 - 1);
    let output_end = last_row + 3;
    let mut w = World::new(28, 8, output_end + 4);
    for x in [3, 6, 9, 12] {
        for z in 1..=last_row {
            wire(&mut w, x, 5, z);
        }
    }
    let mut inputs = Vec::new();
    for x in [3, 9] {
        let mut lever = block(BlockKind::Lever);
        lever.face = Some(Face::Floor);
        lever.facing = Some(Facing::North);
        w.set(x, 5, 1, lever);
        inputs.push(Position::new(x, 5, 1));
        torch(&mut w, x + 1, 4, 1, Facing::East);
        wire(&mut w, x + 2, 4, 1);
    }
    // Restore literal rails before they fan out to all decoder rows.
    for x in [3, 6, 9, 12] {
        let mut r = BlockState::air();
        r.kind = BlockKind::Repeater;
        r.name = "minecraft:repeater".into();
        r.facing = Some(Facing::North);
        r.delay = if balanced && [3, 9].contains(&x) {
            2
        } else {
            1
        };
        w.set(x, 5, 2, r);
    }
    if last_row > 16 {
        for x in [3, 6, 9, 12] {
            let mut r = BlockState::air();
            r.kind = BlockKind::Repeater;
            r.name = "minecraft:repeater".into();
            r.facing = Some(Facing::North);
            r.delay = 1;
            w.set(x, 5, 14, r);
        }
    }
    for (index, ((address, mask), word)) in rows.into_iter().enumerate() {
        let z = 4 + pitch * index as i32;
        for x in 2..25 {
            wire(&mut w, x, 3, z);
        }
        let mut repeater = BlockState::air();
        repeater.kind = BlockKind::Repeater;
        repeater.name = "minecraft:repeater".into();
        repeater.facing = Some(Facing::West);
        repeater.delay = 1;
        w.set(16, 3, z, repeater);
        for (bit, true_x) in [3, 9].into_iter().enumerate() {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let literal_x = if address & (1 << bit) != 0 {
                true_x
            } else {
                true_x + 3
            };
            torch(&mut w, literal_x + 1, 4, z, Facing::East);
        }
        for (bit, x) in [18, 21, 24].into_iter().enumerate() {
            if word & (1 << bit) != 0 {
                torch(&mut w, x, 2, z + 1, Facing::South);
            }
        }
    }
    let mut drivers = Vec::new();
    let mut lamps = Vec::new();
    for x in [18, 21, 24] {
        for z in 5..=output_end {
            wire(&mut w, x, 1, z);
        }
        if output_end > 19 {
            let mut r = BlockState::air();
            r.kind = BlockKind::Repeater;
            r.name = "minecraft:repeater".into();
            r.facing = Some(Facing::North);
            r.delay = 1;
            w.set(x, 1, 18, r);
        }
        w.set(x, 0, output_end + 1, block(BlockKind::Solid));
        w.set(x, 1, output_end + 1, block(BlockKind::Lamp));
        drivers.push(Position::new(x, 1, output_end));
        lamps.push(Position::new(x, 1, output_end + 1));
    }
    (w, inputs, drivers, lamps)
}

fn measure_rom(
    world: World,
    inputs: Vec<Position>,
    drivers: Vec<Position>,
    lamps: Vec<Position>,
    words: &[u8],
) -> serde_json::Value {
    let mut sim = Simulator::new(world);
    sim.run_until_stable(2000).unwrap();
    sim.attach_observer(
        drivers
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, format!("driver{i}")))
            .chain(
                lamps
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (*p, format!("lamp{i}"))),
            ),
    );
    let mut traces = Vec::new();
    let mut worst = 0;
    let mut sum = 0;
    let mut driver_glitches = 0;
    let mut lamp_glitches = 0;
    for from in 0..words.len() {
        for to in 0..words.len() {
            for (phase, value) in [from, to].into_iter().enumerate() {
                if phase == 1 {
                    sim.reset_observer();
                }
                for (bit, p) in inputs.iter().enumerate() {
                    let mut b = sim.world().get(p.x, p.y, p.z).clone();
                    b.lit = value & (1 << bit) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, b);
                }
                let start = sim.current_tick();
                let ticks = sim.run_until_stable(2000).unwrap();
                if phase == 1 {
                    worst = worst.max(ticks);
                    sum += ticks;
                    for (sites, glitches) in [
                        (&drivers, &mut driver_glitches),
                        (&lamps, &mut lamp_glitches),
                    ] {
                        for (bit, p) in sites.iter().enumerate() {
                            let b = sim.world().get(p.x, p.y, p.z);
                            assert_eq!(
                                if b.kind == BlockKind::RedstoneWire {
                                    b.power > 0
                                } else {
                                    b.lit
                                },
                                words[to] & (1 << bit) != 0,
                                "address {from}->{to}, bit={bit}, site={p:?}"
                            );
                            *glitches += usize::from(
                                sim.observations()
                                    .iter()
                                    .filter(|o| o.position == *p)
                                    .count()
                                    > 1,
                            );
                        }
                    }
                    traces.push(json!({"from":from,"to":to,"ticks":ticks,"changes":sim.observations().iter().map(|o|json!({"label":o.label,"tick":o.tick-start,"value":o.value})).collect::<Vec<_>>()}));
                }
            }
        }
    }
    let mut single = 0;
    let mut lamp_single = 0;
    let mut driver_last = 0;
    let mut pulse_worst = 0;
    let mut pulse_total = 0;
    for trace in &traces {
        for bit in 0..drivers.len() {
            for (prefix, single_count) in [("driver", &mut single), ("lamp", &mut lamp_single)] {
                let label = format!("{prefix}{bit}");
                let events: Vec<_> = trace["changes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["label"] == label)
                    .collect();
                if events.len() > 1
                    && (trace["from"].as_u64().unwrap() ^ trace["to"].as_u64().unwrap())
                        .count_ones()
                        == 1
                {
                    *single_count += 1;
                }
                if prefix == "driver" {
                    if let Some(e) = events.last() {
                        driver_last = driver_last.max(e["tick"].as_u64().unwrap());
                    }
                    let expected = words[trace["to"].as_u64().unwrap() as usize] & (1 << bit) != 0;
                    for window in events.windows(2) {
                        if window[0]["value"].as_bool().unwrap() != expected {
                            let width = window[1]["tick"].as_u64().unwrap()
                                - window[0]["tick"].as_u64().unwrap();
                            pulse_total += width;
                            pulse_worst = pulse_worst.max(width);
                        }
                    }
                }
            }
        }
    }
    json!({"worst_settle_ticks":worst,"total_settle_ticks":sum,"driver_glitches":driver_glitches,"lamp_glitches":lamp_glitches,
        "driver_single_input_glitches":single,"lamp_single_input_glitches":lamp_single,"worst_driver_change_ticks":driver_last,
        "worst_driver_pulse_ticks":pulse_worst,"total_driver_pulse_ticks":pulse_total,"traces":traces})
}

#[test]
fn four_word_rom_with_physical_literal_decoder_records_full_transition_quality() {
    let words = [1, 6, 3, 5];
    let source="module rom4(input logic a,b,output logic y0,y1,y2); assign y0=~a|b; assign y1=a^b; assign y2=a; endmodule";
    let a = compile_systemverilog(
        &[SourceInput {
            name: "rom4.sv",
            text: source,
        }],
        &CompileOptions::new("rom4"),
    )
    .unwrap();
    let outputs = a.output_map();
    let netlist = lower_optimised(&a.netlist).unwrap();
    let c = compile(&netlist).unwrap();
    let position = |p: (i32, i32, i32)| Position::new(p.0, p.1, p.2);
    let inputs = ["a", "b"]
        .into_iter()
        .map(|n| position(c.input_positions[n]))
        .collect();
    let drivers = ["y0", "y1", "y2"]
        .into_iter()
        .map(|n| position(c.gate_output_positions[&outputs[n]]))
        .collect();
    let lamps = ["y0", "y1", "y2"]
        .into_iter()
        .map(|n| position(c.output_positions[&outputs[n]]))
        .collect();
    let baseline = measure_rom(c.world, inputs, drivers, lamps, &words);
    let (world, inputs, drivers, lamps) = matrix_four_words(words, false, false);
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
            &matrix_four_words(words, false, false).0
        )
    );
    let candidate = measure_rom(world, inputs, drivers, lamps, &words);
    let (world, inputs, drivers, lamps) = matrix_four_words(words, true, false);
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
            &matrix_four_words(words, true, false).0
        )
    );
    let covered = measure_rom(world, inputs, drivers, lamps, &words);
    let (world, inputs, drivers, lamps) = matrix_four_words(words, true, true);
    assert_eq!(
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
            &matrix_four_words(words, true, true).0
        )
    );
    let balanced = measure_rom(world, inputs, drivers, lamps, &words);
    for (metric, expected) in [
        ("worst_settle_ticks", 20),
        ("total_settle_ticks", 187),
        ("driver_glitches", 2),
        ("lamp_glitches", 2),
        ("driver_single_input_glitches", 0),
        ("lamp_single_input_glitches", 0),
        ("worst_driver_change_ticks", 16),
        ("worst_driver_pulse_ticks", 4),
        ("total_driver_pulse_ticks", 8),
    ] {
        assert_eq!(
            baseline[metric], expected,
            "fixed reference drift: {metric}"
        );
        assert!(balanced[metric].as_u64().unwrap() <= expected);
    }
    assert_eq!(balanced["driver_glitches"], 0);
    assert_eq!(balanced["lamp_glitches"], 0);
    let result = json!({"words":words,"baseline":baseline,"candidate":candidate,"covered":covered,"balanced":balanced});
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/torch_row_four_words.json"),
        serde_json::to_string_pretty(&result).unwrap(),
    )
    .unwrap();
}

#[test]
fn all_two_address_three_output_roms_have_correct_physical_truth() {
    let mut worst = 0;
    let mut glitches = 0;
    let mut single = 0;
    for encoding in 0..4096u32 {
        let words = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
        let (world, inputs, drivers, lamps) = matrix_four_words(words, true, true);
        assert_eq!(
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world),
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                &matrix_four_words(words, true, true).0
            )
        );
        let quality = measure_rom(world, inputs, drivers, lamps, &words);
        worst = worst.max(quality["worst_settle_ticks"].as_u64().unwrap());
        glitches += quality["driver_glitches"].as_u64().unwrap();
        single += quality["driver_single_input_glitches"].as_u64().unwrap();
        if encoding % 512 == 0 {
            println!("verified ROM encodings through {encoding}: words={words:?}");
        }
    }
    assert_eq!(single, 0, "single-input hazard regression");
    assert!(glitches <= 48, "prototype aggregate pulse regression");
    assert!(worst <= 18, "prototype aggregate settling regression");
    let report = json!({"data_tables":4096,"measured_transitions":65536,"setup_transitions":65536,"driver_glitches":glitches,"driver_single_input_glitches":single,"worst_settle_ticks":worst,"all_final_driver_and_lamp_truth_correct":true,"all_worlds_deterministic":true});
    println!("{report}");
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/torch_row_exhaustive.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}
