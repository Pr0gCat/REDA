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
    matrix_four_words_with_aligned_refresh(words, minimized, balanced, false)
}

fn matrix_four_words_with_aligned_refresh(
    words: [u8; 4],
    minimized: bool,
    balanced: bool,
    align_refresh: bool,
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
            // Put output restoration at the same boundary as the literal
            // restoration: early rows pay the output stage, late rows the
            // literal stage, so every row crosses exactly one stage.
            w.set(x, 1, if align_refresh { 15 } else { 18 }, r);
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
    qualify_all_tables(false);
}

#[test]
fn all_rom_tables_with_aligned_refresh_boundaries_have_correct_truth_and_no_sampled_glitches() {
    qualify_all_tables(true);
}

fn qualify_all_tables(aligned: bool) {
    let mut worst = 0;
    let mut glitches = 0;
    let mut single = 0;
    let mut lamp_glitches = 0;
    for encoding in 0..4096u32 {
        let words = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
        let (world, inputs, drivers, lamps) =
            matrix_four_words_with_aligned_refresh(words, true, true, aligned);
        assert_eq!(
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world),
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                &matrix_four_words_with_aligned_refresh(words, true, true, aligned).0
            )
        );
        let quality = measure_rom(world, inputs, drivers, lamps, &words);
        worst = worst.max(quality["worst_settle_ticks"].as_u64().unwrap());
        glitches += quality["driver_glitches"].as_u64().unwrap();
        single += quality["driver_single_input_glitches"].as_u64().unwrap();
        lamp_glitches += quality["lamp_glitches"].as_u64().unwrap();
        if encoding % 512 == 0 {
            println!("verified ROM encodings through {encoding}: words={words:?}");
        }
    }
    if aligned {
        assert_eq!(lamp_glitches, 0, "lamp hazard regression");
    }
    assert_eq!(single, 0, "single-input hazard regression");
    assert!(
        glitches <= if aligned { 0 } else { 48 },
        "prototype aggregate pulse regression"
    );
    assert!(
        worst <= if aligned { 16 } else { 18 },
        "prototype aggregate settling regression"
    );
    let report = json!({"aligned_refresh_boundaries":aligned,"data_tables":4096,"measured_transitions":65536,"setup_transitions":65536,"driver_glitches":glitches,"lamp_glitches":lamp_glitches,"driver_single_input_glitches":single,"worst_settle_ticks":worst,"all_final_driver_and_lamp_truth_correct":true,"all_worlds_deterministic":true});
    println!("{report}");
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(if aligned {
            "target/torch_row_exhaustive_aligned.json"
        } else {
            "target/torch_row_exhaustive.json"
        }),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn six_row_decoder_refresh_paths_are_balanced_against_frozen_geometry() {
    let fixed: serde_json::Value = serde_json::from_str(include_str!(
        "../docs/generator-torch-row-stage-balance-2026-09-30.json"
    ))
    .unwrap();
    let mut records = Vec::new();
    for encoding in 0..4096u32 {
        let words: [u8; 4] = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
        let mut cubes = std::collections::BTreeSet::new();
        for bit in 0..3 {
            let table: Vec<_> = words.iter().map(|w| w & (1 << bit) != 0).collect();
            cubes.extend(cover::cover(&table, true));
        }
        if cubes.len() != 6 {
            continue;
        }
        let (w, i, d, l) = matrix_four_words(words, true, true);
        let baseline = measure_rom(w, i, d, l, &words);
        let (w, i, d, l) = matrix_four_words_with_aligned_refresh(words, true, true, true);
        assert_eq!(
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&w),
            reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(
                &matrix_four_words_with_aligned_refresh(words, true, true, true).0
            )
        );
        let candidate = measure_rom(w, i, d, l, &words);
        let frozen = fixed["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["encoding"] == encoding)
            .expect("fixed six-row reference exists");
        println!(
            "encoding={encoding} words={words:?} old={} new={}",
            baseline["driver_glitches"], candidate["driver_glitches"]
        );
        for metric in [
            "worst_settle_ticks",
            "total_settle_ticks",
            "driver_glitches",
            "lamp_glitches",
            "driver_single_input_glitches",
            "lamp_single_input_glitches",
            "worst_driver_change_ticks",
            "worst_driver_pulse_ticks",
            "total_driver_pulse_ticks",
        ] {
            assert_eq!(
                baseline[metric], frozen["baseline"][metric],
                "independent baseline drift for {encoding}/{metric}"
            );
            assert!(
                candidate[metric].as_u64().unwrap() <= frozen["baseline"][metric].as_u64().unwrap(),
                "{words:?}: {metric} regression: {} > {}",
                candidate[metric],
                baseline[metric]
            );
        }
        records.push(
            json!({"encoding":encoding,"words":words,"baseline":baseline,"candidate":candidate}),
        );
    }
    assert_eq!(records.len(), 48);
    assert!(records
        .iter()
        .all(|r| r["candidate"]["driver_glitches"] == 0 && r["candidate"]["lamp_glitches"] == 0));
    println!("six-row tables: {}", records.len());
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/torch_row_stage_balance.json"),
        serde_json::to_string_pretty(&records).unwrap(),
    )
    .unwrap();
}

/// Bind every actual torch and wire junction; do not relabel the original
/// frontend gates as though a truth-table rewrite preserved their topology.
fn matrix_logic_binding(
    words: [u8; 4],
) -> (
    reda::compile::Netlist,
    std::collections::BTreeMap<String, Position>,
) {
    use reda::compile::{Gate, Netlist};
    let mut rows = std::collections::BTreeMap::new();
    for bit in 0..3 {
        let table: Vec<_> = words.iter().map(|w| w & (1 << bit) != 0).collect();
        for cube in cover::cover(&table, true) {
            *rows.entry((cube.bits, cube.mask)).or_insert(0u8) |= 1 << bit;
        }
    }
    let mut gates = Vec::new();
    let mut positions = std::collections::BTreeMap::new();
    for (bit, name) in ["a", "b"].into_iter().enumerate() {
        let inverse = format!("inverse{bit}");
        gates.push(Gate::nor(&inverse, &[name]));
        positions.insert(inverse, Position::new(4 + 6 * bit as i32, 4, 1));
    }
    let mut terms: [Vec<String>; 3] = std::array::from_fn(|_| Vec::new());
    let output_end = 4 + 3 * (rows.len().max(1) as i32 - 1) + 3;
    for (row, ((address, mask), word)) in rows.into_iter().enumerate() {
        let z = 4 + 3 * row as i32;
        let mut mismatches = Vec::new();
        for (bit, name) in ["a", "b"].into_iter().enumerate() {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let true_literal = address & (1 << bit) != 0;
            let source = if true_literal {
                name.to_string()
            } else {
                format!("inverse{bit}")
            };
            let mismatch = format!("row{row}_mismatch{bit}");
            gates.push(Gate::nor(&mismatch, &[&source]));
            positions.insert(
                mismatch.clone(),
                Position::new(4 + 6 * bit as i32 + if true_literal { 0 } else { 3 }, 4, z),
            );
            mismatches.push(mismatch);
        }
        let control = if mismatches.is_empty() {
            None
        } else if mismatches.len() == 1 {
            Some(mismatches[0].clone())
        } else {
            let control = format!("row{row}_control");
            let inputs: Vec<_> = mismatches.iter().map(String::as_str).collect();
            gates.push(Gate::merge(&control, &inputs));
            positions.insert(control.clone(), Position::new(15, 3, z));
            Some(control)
        };
        for (bit, bit_terms) in terms.iter_mut().enumerate() {
            if word & (1 << bit) == 0 {
                continue;
            }
            let term = format!("row{row}_bit{bit}");
            let inputs: Vec<_> = control.iter().map(String::as_str).collect();
            gates.push(Gate::nor(&term, &inputs));
            positions.insert(term.clone(), Position::new(18 + 3 * bit as i32, 2, z + 1));
            bit_terms.push(term);
        }
    }
    let mut outputs = Vec::new();
    for (bit, terms) in terms.into_iter().enumerate() {
        let output = if terms.is_empty() {
            let high = format!("constant_high{bit}");
            let low = format!("constant_low{bit}");
            gates.push(Gate::nor(&high, &[]));
            gates.push(Gate::nor(&low, &[&high]));
            positions.insert(high, Position::new(18 + 3 * bit as i32, 4, output_end - 1));
            positions.insert(
                low.clone(),
                Position::new(18 + 3 * bit as i32, 2, output_end),
            );
            low
        } else if terms.len() == 1 {
            terms[0].clone()
        } else {
            let output = format!("output{bit}");
            let inputs: Vec<_> = terms.iter().map(String::as_str).collect();
            gates.push(Gate::merge(&output, &inputs));
            positions.insert(
                output.clone(),
                Position::new(18 + 3 * bit as i32, 1, output_end),
            );
            output
        };
        outputs.push(output);
    }
    (
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs,
            gates,
        },
        positions,
    )
}

#[test]
fn every_matrix_logic_node_has_a_real_observation_site_and_exact_settled_meaning() {
    let result = verify_matrix_logic_binding([1, 6, 3, 5]);
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/torch_row_logic_bindings.json"),
        serde_json::to_string_pretty(&result).unwrap(),
    )
    .unwrap();
}

fn verify_matrix_logic_binding(words: [u8; 4]) -> serde_json::Value {
    use reda::compile::fragment_synth::instance_graph::InstanceGraph;
    use reda::compile::topology::{GateKind, Library};
    let (netlist, positions) = matrix_logic_binding(words);
    assert_eq!(
        (netlist.clone(), positions.clone()),
        matrix_logic_binding(words)
    );
    let instances = InstanceGraph::one_to_one(&netlist, &Library::default_library()).unwrap();
    instances.validate(&netlist).unwrap();
    for instance in &instances.instances {
        reda::compile::fragment_synth::topology::prove_topology_semantics(
            &instance.expanded.topology,
            netlist.gates[instance.logical_gate.0 as usize].kind,
        )
        .unwrap();
    }
    let (world, inputs, _, _) = matrix_four_words_with_aligned_refresh(words, true, true, true);
    assert_eq!(positions.len(), netlist.gates.len());
    let torch_positions: std::collections::BTreeSet<_> =
        world.positions_of(BlockKind::WallTorch).collect();
    let bound_torches: std::collections::BTreeSet<_> = netlist
        .gates
        .iter()
        .filter(|g| matches!(g.kind, GateKind::Nor(_)))
        .map(|g| {
            let p = positions[&g.output];
            world.index(p.x, p.y, p.z).unwrap()
        })
        .collect();
    assert_eq!(
        torch_positions, bound_torches,
        "no hidden or fictitious logic torch"
    );
    let order = netlist.combinational_order().unwrap();
    let mut sim = Simulator::new(world);
    sim.run_until_stable(2000).unwrap();
    for from in 0..4 {
        for to in 0..4 {
            for address in [from, to] {
                for (bit, p) in inputs.iter().enumerate() {
                    let mut b = sim.world().get(p.x, p.y, p.z).clone();
                    b.lit = address & (1 << bit) != 0;
                    sim.world_mut().set(p.x, p.y, p.z, b);
                }
                sim.run_until_stable(2000).unwrap();
                let mut values: std::collections::BTreeMap<_, _> = netlist
                    .inputs
                    .iter()
                    .enumerate()
                    .map(|(bit, n)| (n.clone(), address & (1 << bit) != 0))
                    .collect();
                for &index in &order {
                    let gate = &netlist.gates[index];
                    let any = gate.inputs.iter().any(|n| values[n]);
                    let expected = match gate.kind {
                        GateKind::Nor(_) => !any,
                        GateKind::Or(_) => any,
                        _ => unreachable!(),
                    };
                    let p = positions[&gate.output];
                    let b = sim.world().get(p.x, p.y, p.z);
                    assert_eq!(
                        b.kind,
                        if matches!(gate.kind, GateKind::Nor(_)) {
                            BlockKind::WallTorch
                        } else {
                            BlockKind::RedstoneWire
                        }
                    );
                    assert_eq!(
                        if b.kind == BlockKind::RedstoneWire {
                            b.power > 0
                        } else {
                            b.lit
                        },
                        expected,
                        "{from}->{to}, address={address}, node={}",
                        gate.output
                    );
                    values.insert(gate.output.clone(), expected);
                }
                for (bit, n) in netlist.outputs.iter().enumerate() {
                    assert_eq!(values[n], words[address] & (1 << bit) != 0);
                }
            }
        }
    }
    let result = json!({"netlist":format!("{netlist:?}"),"gate_count":netlist.gates.len(),"physical_torches":bound_torches.len(),"instances":instances.instances.len(),"all_internal_settled_truth_correct":true,"no_unmapped_torches":true,
        "bindings":positions.iter().map(|(n,p)|json!({"net":n,"x":p.x,"y":p.y,"z":p.z})).collect::<Vec<_>>()});
    result
}

#[test]
fn all_nonconstant_matrix_functions_have_complete_logic_bindings() {
    let mut verified = 0;
    for encoding in 0..4096u32 {
        let words: [u8; 4] = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
        if (0..3).any(|bit| {
            words.iter().all(|w| w & (1 << bit) == 0) || words.iter().all(|w| w & (1 << bit) != 0)
        }) {
            continue;
        }
        let _ = verify_matrix_logic_binding(words);
        verified += 1;
        if verified % 512 == 0 {
            println!("verified complete bindings for {verified} data tables");
        }
    }
    assert_eq!(verified, 2744);
    let report = json!({"nonconstant_data_tables":verified,"measured_transitions":verified*16,"setup_transitions":verified*16,"all_internal_settled_truth_correct":true,"all_torches_bound_one_to_one":true,"all_instance_graphs_valid":true,"all_selected_topologies_symbolically_proved":true,"all_binding_reconstructions_deterministic":true,"full_physical_candidate_certification":false});
    println!("{report}");
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/torch_row_logic_bindings_exhaustive.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

fn typed_matrix_library() -> reda::compile::topology::Library {
    use reda::compile::topology::{
        GateKind, Library, LibraryEntry, Primitive, Template, TemplateNode,
    };
    let default = Library::default_library();
    let mut entries = std::collections::BTreeMap::new();
    for kind in [
        GateKind::Nor(0),
        GateKind::Nor(1),
        GateKind::Or(2),
        GateKind::Or(3),
    ] {
        let entry = default.entry_at(kind, 0).unwrap();
        entries.insert(
            kind,
            vec![LibraryEntry {
                name: entry.name,
                template: Template {
                    nodes: entry.template.nodes.clone(),
                    internal_edges: entry.template.internal_edges.clone(),
                    inputs: entry.template.inputs.clone(),
                    output: entry.template.output,
                    embedding_hints: entry.template.embedding_hints.clone(),
                },
            }],
        );
    }
    entries.insert(
        GateKind::Buf,
        vec![LibraryEntry {
            name: "matrix-repeater-buffer",
            template: Template {
                nodes: vec![(TemplateNode::Torch, Primitive::Repeater)],
                internal_edges: vec![],
                inputs: vec![TemplateNode::Torch],
                output: Some(TemplateNode::Torch),
                embedding_hints: vec![],
            },
        }],
    );
    Library::new(entries)
}

// Make each real output-restoration repeater a declared BUF node, with
// independent OR regions before and after it. Never hide it inside an OR.
fn bind_matrix_refresh_stages(
    netlist: &mut reda::compile::Netlist,
    positions: &mut std::collections::BTreeMap<String, Position>,
    world: &mut World,
) {
    use reda::compile::{topology::GateKind, Gate};
    for bit in 0..3 {
        let x = 18 + 3 * bit as i32;
        if world.get(x, 1, 15).kind != BlockKind::Repeater {
            continue;
        }
        let old_output = netlist.outputs[bit].clone();
        let old_gate = netlist
            .gates
            .iter()
            .find(|g| g.output == old_output)
            .unwrap();
        let terms = if matches!(old_gate.kind, GateKind::Or(_)) {
            old_gate.inputs.clone()
        } else {
            vec![old_output.clone()]
        };
        let (early, late): (Vec<_>, Vec<_>) = terms.into_iter().partition(|n| positions[n].z < 15);
        if early.is_empty() {
            // A late-only output has no signal to restore in its empty prefix.
            let first = late.iter().map(|n| positions[n].z).min().unwrap();
            for z in 5..first {
                world.set(x, 1, z, BlockState::air());
                world.set(x, 0, z, BlockState::air());
            }
            continue;
        }
        if matches!(old_gate.kind, GateKind::Or(_)) {
            netlist.gates.retain(|g| g.output != old_output);
            positions.remove(&old_output);
        }
        let pre = if early.len() == 1 {
            early[0].clone()
        } else {
            let name = format!("early_output{bit}");
            let inputs: Vec<_> = early.iter().map(String::as_str).collect();
            netlist.gates.push(Gate::merge(&name, &inputs));
            positions.insert(name.clone(), Position::new(x, 1, 13));
            name
        };
        let buffer = format!("refresh_output{bit}");
        netlist.gates.push(Gate {
            name: buffer.clone(),
            output: buffer.clone(),
            inputs: vec![pre],
            kind: GateKind::Buf,
        });
        positions.insert(buffer.clone(), Position::new(x, 1, 15));
        netlist.outputs[bit] = if late.is_empty() {
            buffer
        } else {
            let name = format!("output{bit}");
            let mut inputs = late;
            inputs.push(buffer);
            let inputs: Vec<_> = inputs.iter().map(String::as_str).collect();
            netlist.gates.push(Gate::merge(&name, &inputs));
            let end = world.size().2 - 4;
            positions.insert(name.clone(), Position::new(x, 1, end));
            name
        };
    }
}

fn typed_matrix_candidate(
    words: [u8; 4],
) -> (
    reda::compile::Netlist,
    World,
    reda::compile::fragment_synth::candidate::ExpandedPhysicalCandidate,
) {
    use reda::compile::fragment_synth::{
        candidate::*, identity::*, instance_graph::*, topology::*,
    };
    use reda::compile::{
        geometry::{Anchor, CellFacing},
        planner::PortPlacements,
    };
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let anchor = |p: Position| Anchor {
        x: p.x,
        y: p.y,
        z: p.z,
    };
    let (mut netlist, mut positions) = matrix_logic_binding(words);
    let (mut world, mut inputs, _, lamps) =
        matrix_four_words_with_aligned_refresh(words, true, true, true);
    // A constant-one row has no active control rail. Retain only the
    // actual supports of its zero-input NOR data torches.
    let row_zs: std::collections::BTreeSet<_> = world
        .positions_of(BlockKind::Repeater)
        .map(|flat| world.decode(flat))
        .filter(|&(x, y, _)| x == 16 && y == 3)
        .map(|(_, _, z)| z)
        .collect();
    for z in row_zs {
        if positions
            .values()
            .any(|p| p.y == 4 && p.z == z && p.x <= 13)
        {
            continue;
        }
        for x in 2..25 {
            world.set(x, 3, z, BlockState::air());
            if world.get(x, 2, z + 1).kind != BlockKind::WallTorch {
                world.set(x, 2, z, BlockState::air());
            }
        }
    }
    // A real high driver and inverter replace each empty output column.
    for bit in 0..3 {
        if let Some(p) = positions.get(&format!("constant_high{bit}")) {
            world.set(p.x - 1, p.y, p.z, block(BlockKind::Solid));
            torch(&mut world, p.x, p.y, p.z, Facing::East);
            wire(&mut world, p.x, p.y - 1, p.z);
            let low = positions[&format!("constant_low{bit}")];
            torch(&mut world, low.x, low.y, low.z, Facing::South);
        }
    }
    bind_matrix_refresh_stages(&mut netlist, &mut positions, &mut world);
    // Unused address inverters have no observable function but can burn
    // out under rapid input histories. Delete their actual gate and torch;
    // the rail pruning below then retains only real taps/inverter inputs.
    for bit in 0..2 {
        let inverse = format!("inverse{bit}");
        if netlist.gates.iter().any(|g| g.inputs.contains(&inverse)) {
            continue;
        }
        netlist.gates.retain(|g| g.output != inverse);
        if let Some(p) = positions.remove(&inverse) {
            world.set(p.x, p.y, p.z, BlockState::air());
        }
    }
    let graph = InstanceGraph::one_to_one(&netlist, &typed_matrix_library()).unwrap();
    let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
    // A conductor above the inverter support makes the direct lever/support
    // contact an explicit route terminal; retain the original fixture as control.
    for p in &mut inputs {
        let lever = world.get(p.x, p.y, p.z).clone();
        world.set(p.x, p.y, p.z, block(BlockKind::RedstoneWire));
        world.set(p.x, p.y - 1, 0, block(BlockKind::Solid));
        world.set(p.x, p.y, 0, lever);
        p.z = 0;
    }
    // Retain each address rail only through its last real decoder tap.
    // True rails still feed the global inverter; unused inverse rails have
    // no conductor load. Keep the untrimmed prototype as independent control.
    for x in [3, 6, 9, 12] {
        let last_tap = positions
            .values()
            .filter(|p| p.x == x + 1 && p.y == 4 && p.z >= 4)
            .map(|p| p.z)
            .max()
            .unwrap_or(
                if positions
                    .values()
                    .any(|p| p.x == x + 1 && p.y == 4 && p.z == 1)
                {
                    1
                } else {
                    0
                },
            );
        for z in (last_tap + 1)..world.size().2 {
            world.set(x, 5, z, BlockState::air());
            world.set(x, 4, z, BlockState::air());
        }
        if last_tap == 0 && [6, 12].contains(&x) {
            world.set(x - 1, 4, 1, BlockState::air());
            world.set(x - 1, 3, 1, BlockState::air());
        }
    }
    let placed = |p: Position| PlacedBlock {
        at: anchor(p),
        state: world.get(p.x, p.y, p.z).clone(),
    };
    let mut claimed = BTreeSet::new();
    for instance in &candidate.instances.instances {
        let p = positions[&netlist.gates[instance.logical_gate.0 as usize].output];
        let obs = VerifiedObservation {
            site: ObservationSite {
                id: ObservationId::InstanceOutput(instance.id),
                at: anchor(p),
                logical_owner: Some(instance.id),
                display_label: None,
            },
            state: world.get(p.x, p.y, p.z).clone(),
        };
        candidate.observations.insert(obs.site.id, obs.clone());
        if let OutputSpec::Primitive(id) = instance.expanded.topology.output {
            let state = world.get(p.x, p.y, p.z);
            let is_repeater = state.kind == BlockKind::Repeater;
            let support = if is_repeater {
                p.down()
            } else {
                p.offset(state.facing.unwrap().opposite())
            };
            let facing = match state.facing.unwrap() {
                Facing::North => CellFacing::NORTH,
                Facing::South => CellFacing::SOUTH,
                Facing::East => CellFacing::EAST,
                Facing::West => CellFacing::WEST,
                _ => unreachable!(),
            };
            let blocks = vec![placed(support), placed(p)];
            for b in &blocks {
                assert!(claimed.insert(b.at));
            }
            candidate.placements.insert(
                id,
                PrimitivePlacement {
                    id,
                    variant: facing.index() as u16,
                    facing,
                    anchor: anchor(if is_repeater { p } else { support }),
                    delayed: is_repeater.then_some(DelayedComponent {
                        at: anchor(p),
                        owner: DelayedOwner::Primitive(id),
                    }),
                    blocks,
                },
            );
            let mut obs = obs;
            obs.site.id = ObservationId::PrimitiveOutput(id);
            candidate.observations.insert(obs.site.id, obs);
        } else {
            let mut obs = obs;
            obs.site.id = ObservationId::JunctionOutput(instance.id);
            candidate.observations.insert(obs.site.id, obs);
            candidate.junctions.insert(
                instance.id,
                RealisedJunction {
                    id: instance.id,
                    at: anchor(p),
                    facing: CellFacing::SOUTH,
                    contributors: Vec::new(),
                    cells: Vec::new(),
                },
            );
        }
    }
    for (port, p) in inputs.into_iter().enumerate() {
        let id = PortId(port as u32);
        let endpoint = PhysicalEndpointId::PrimaryInput(id);
        let blocks = vec![placed(p.down()), placed(p)];
        for b in &blocks {
            assert!(claimed.insert(b.at));
        }
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: None,
                blocks,
            },
        );
        candidate.observations.insert(
            ObservationId::PrimaryInput(id),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::PrimaryInput(id),
                    at: anchor(p),
                    logical_owner: None,
                    display_label: None,
                },
                state: world.get(p.x, p.y, p.z).clone(),
            },
        );
    }
    for (port, p) in lamps.into_iter().enumerate() {
        let id = PortId(port as u32);
        let endpoint = PhysicalEndpointId::DeclaredOutput(id);
        let blocks = vec![placed(p.down()), placed(p)];
        for b in &blocks {
            assert!(claimed.insert(b.at));
        }
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: None,
                blocks,
            },
        );
        candidate.observations.insert(
            ObservationId::DeclaredOutput(id),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::DeclaredOutput(id),
                    at: anchor(p),
                    logical_owner: None,
                    display_label: None,
                },
                state: world.get(p.x, p.y, p.z).clone(),
            },
        );
    }
    let endpoint = |d: &PhysicalDriver| match d {
        PhysicalDriver::PrimaryInput(p) => PhysicalEndpointId::PrimaryInput(*p),
        PhysicalDriver::Instance(InstanceDriver::Primitive { terminals, .. }) => {
            PhysicalEndpointId::PrimitiveOutput(terminals[0])
        }
        PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
            PhysicalEndpointId::Junction(*logical_owner)
        }
    };
    let observation = |e: PhysicalEndpointId| match e {
        PhysicalEndpointId::PrimaryInput(p) => ObservationId::PrimaryInput(p),
        PhysicalEndpointId::PrimitiveOutput(p) => ObservationId::PrimitiveOutput(p),
        PhysicalEndpointId::Junction(p) => ObservationId::JunctionOutput(p),
        PhysicalEndpointId::DeclaredOutput(p) => ObservationId::DeclaredOutput(p),
        _ => unreachable!(),
    };
    let mut work = Vec::new();
    for instance in &candidate.instances.instances {
        for spec in &instance.expanded.topology.connections {
            let ConnectionSource::ExternalInput { input_index } = spec.source else {
                panic!("unexpected internal connection");
            };
            let assignment = candidate
                .instances
                .assignments
                .iter()
                .find(|a| {
                    a.sink
                        == PhysicalSink::InstanceInput {
                            instance: instance.id,
                            input_index,
                        }
                })
                .unwrap();
            let source = endpoint(&assignment.driver);
            let target = match spec.target {
                ConnectionTarget::Primitive(p) => PhysicalEndpointId::PrimitiveOutput(p),
                ConnectionTarget::Junction(j) => PhysicalEndpointId::Junction(j),
            };
            work.push((
                source,
                RouteTarget::Connection(spec.id),
                target,
                Some(spec.id),
            ));
        }
    }
    for assignment in &candidate.instances.assignments {
        if let PhysicalSink::DeclaredOutput(p) = assignment.sink {
            work.push((
                endpoint(&assignment.driver),
                RouteTarget::DeclaredOutput(p),
                PhysicalEndpointId::DeclaredOutput(p),
                None,
            ));
        }
    }
    let mut route_ids = BTreeMap::new();
    for (source, target, landing, connection) in work {
        let next = RouteId(route_ids.len() as u32);
        let route = *route_ids.entry(source).or_insert(next);
        let at = candidate.observations[&observation(source)].site.at;
        let p = Position::new(at.x, at.y, at.z);
        let root = if world.get(p.x, p.y, p.z).kind == BlockKind::Repeater {
            p.offset(world.get(p.x, p.y, p.z).facing.unwrap().opposite())
        } else if world.get(p.x, p.y, p.z).kind == BlockKind::RedstoneWire {
            p
        } else if world.get(p.x, p.y - 1, p.z).kind == BlockKind::RedstoneWire {
            p.down()
        } else {
            [Facing::North, Facing::South, Facing::East, Facing::West]
                .into_iter()
                .map(|f| p.offset(f))
                .find(|p| world.get(p.x, p.y, p.z).kind == BlockKind::RedstoneWire)
                .unwrap()
        };
        let end = match landing {
            PhysicalEndpointId::PrimitiveOutput(id) => {
                let support = candidate.placements[&id].anchor;
                let placement = &candidate.placements[&id];
                let output = placement
                    .blocks
                    .iter()
                    .find(|b| b.state.kind == BlockKind::Repeater);
                if let Some(output) = output {
                    Position::new(output.at.x, output.at.y, output.at.z)
                } else {
                    Position::new(support.x, support.y + 1, support.z)
                }
            }
            PhysicalEndpointId::Junction(_) => root,
            PhysicalEndpointId::DeclaredOutput(_) => {
                let at = candidate.observations[&observation(landing)].site.at;
                Position::new(at.x, at.y, at.z - 1)
            }
            _ => unreachable!(),
        };
        let mut parent = BTreeMap::new();
        let mut queue = VecDeque::from([root]);
        parent.insert(root, root);
        while let Some(p) = queue.pop_front() {
            if p == end {
                break;
            }
            let state = world.get(p.x, p.y, p.z);
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                if state.kind == BlockKind::Repeater
                    && Some(direction) != state.facing.map(Facing::opposite)
                {
                    continue;
                }
                let mut next: Vec<_> =
                    reda::redstone::simulator::connectivity::dust_connections(&world, p, direction)
                        .iter()
                        .collect();
                let n = p.offset(direction);
                let nstate = world.get(n.x, n.y, n.z);
                if nstate.kind == BlockKind::Repeater && nstate.facing == Some(direction.opposite())
                {
                    next.push(n);
                }
                for n in next {
                    if let std::collections::btree_map::Entry::Vacant(e) = parent.entry(n) {
                        e.insert(p);
                        queue.push_back(n);
                    }
                }
            }
        }
        assert!(
            parent.contains_key(&end),
            "no real conductor path {root:?}->{end:?}"
        );
        let mut path = vec![end];
        while *path.last().unwrap() != root {
            path.push(parent[path.last().unwrap()]);
        }
        path.reverse();
        let target_repeater = match landing {
            PhysicalEndpointId::PrimitiveOutput(id)
                if world.get(end.x, end.y, end.z).kind == BlockKind::Repeater =>
            {
                Some(id)
            }
            _ => None,
        };
        let repeaters = path
            .iter()
            .filter(|p| {
                world.get(p.x, p.y, p.z).kind == BlockKind::Repeater
                    && !(**p == end && target_repeater.is_some())
            })
            .count() as u64;
        let tree = candidate.routes.entry(route).or_insert(RealisedRouteTree {
            id: route,
            source,
            cells: Vec::new(),
            floors: Vec::new(),
            branches: Vec::new(),
        });
        let sink = RoutedSinkId {
            route,
            ordinal: tree.branches.len() as u16,
        };
        tree.branches.push(RealisedRouteBranch {
            sink,
            target,
            root: anchor(root),
            path: path.into_iter().map(anchor).collect(),
            terminal: TerminalRecord {
                sink,
                at: anchor(end),
                state: world.get(end.x, end.y, end.z).clone(),
                kind: if target_repeater.is_some() {
                    reda::compile::routing::RouteTerminalKind::RepeaterIntoSupport
                } else if matches!(landing, PhysicalEndpointId::Junction(_)) {
                    reda::compile::routing::RouteTerminalKind::BareMergeDust
                } else {
                    reda::compile::routing::RouteTerminalKind::DirectedDustIntoSupport
                },
                repeaters,
                delayed_owner: target_repeater.map(DelayedOwner::Primitive),
            },
        });
        if let Some(id) = connection {
            candidate.connections.insert(
                id,
                ConnectionBinding {
                    id,
                    source,
                    landing: PhysicalEndpointId::Landing(id),
                    route,
                    sink,
                },
            );
            if let PhysicalEndpointId::Junction(j) = landing {
                candidate
                    .junctions
                    .get_mut(&j)
                    .unwrap()
                    .contributors
                    .push(PhysicalEndpointId::Landing(id));
            }
        }
    }
    // The junction's observed cell belongs to the junction even when an
    // outgoing branch starts on it. Contributor landings stay route-owned.
    for j in candidate.junctions.values_mut() {
        assert!(claimed.insert(j.at));
        j.cells.push(placed(Position::new(j.at.x, j.at.y, j.at.z)));
    }
    // Assign real branch cells first; merge landings retain their route owner.
    for tree in candidate.routes.values_mut() {
        for at in tree.branches.iter().flat_map(|b| b.path.iter()).copied() {
            if claimed.insert(at) {
                tree.cells.push(placed(Position::new(at.x, at.y, at.z)));
            }
        }
    }
    // This first adapter owns remaining connected wire cells by their actual
    // route/junction component, then owns the support below each conductor.
    for flat in world.positions_of(BlockKind::RedstoneWire) {
        let (x, y, z) = world.decode(flat);
        let at = Anchor { x, y, z };
        if claimed.contains(&at) {
            continue;
        }
        let j = candidate
            .junctions
            .iter()
            .find(|(_, j)| {
                (j.at.y == y && j.at.z == z && j.at.y == 3 && x <= 15)
                    || (j.at.y == y
                        && j.at.x == x
                        && j.at.y == 1
                        && (!candidate.placements.values().any(|p| {
                            p.blocks.iter().any(|b| {
                                b.state.kind == BlockKind::Repeater && b.at.x == x && b.at.y == 1
                            })
                        }) || (z < 15) == (j.at.z < 15)))
            })
            .map(|(&id, _)| id);
        if let Some(j) = j {
            candidate
                .junctions
                .get_mut(&j)
                .unwrap()
                .cells
                .push(placed(Position::new(x, y, z)));
            claimed.insert(at);
        } else {
            let route = candidate
                .routes
                .iter()
                .filter(|(_, r)| {
                    r.branches.iter().any(|b| {
                        b.path
                            .iter()
                            .any(|a| a.y == y && if y == 3 { a.z == z } else { a.x == x })
                    })
                })
                .min_by_key(|(_, r)| {
                    !(y == 3
                        && r.branches
                            .iter()
                            .any(|b| b.path.iter().any(|a| a.x == 16 && a.y == 3 && a.z == z)))
                })
                .map(|(&id, _)| id)
                .expect("wire must have a route owner");
            candidate
                .routes
                .get_mut(&route)
                .unwrap()
                .cells
                .push(placed(Position::new(x, y, z)));
            claimed.insert(at);
        }
    }
    for j in candidate.junctions.values_mut() {
        if claimed.insert(j.at) {
            j.cells.push(placed(Position::new(j.at.x, j.at.y, j.at.z)));
        }
    }
    for tree in candidate.routes.values_mut() {
        for b in &tree.cells {
            let p = Position::new(b.at.x, b.at.y - 1, b.at.z);
            if world.get(p.x, p.y, p.z).kind == BlockKind::Solid && claimed.insert(anchor(p)) {
                tree.floors.push(placed(p));
            }
        }
    }
    for j in candidate.junctions.values_mut() {
        let floors: Vec<_> = j
            .cells
            .iter()
            .map(|b| Position::new(b.at.x, b.at.y - 1, b.at.z))
            .filter(|p| {
                world.get(p.x, p.y, p.z).kind == BlockKind::Solid && claimed.insert(anchor(*p))
            })
            .map(placed)
            .collect();
        j.cells.extend(floors);
    }
    for assignment in &candidate.instances.assignments {
        if let PhysicalSink::DeclaredOutput(port) = assignment.sink {
            let owner = match &assignment.driver {
                PhysicalDriver::PrimaryInput(_) => None,
                PhysicalDriver::Instance(InstanceDriver::Primitive { logical_owner, .. })
                | PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
                    Some(*logical_owner)
                }
            };
            candidate
                .observations
                .get_mut(&ObservationId::DeclaredOutput(port))
                .unwrap()
                .site
                .logical_owner = owner;
        }
    }
    candidate.bind_pin_contracts(&netlist).unwrap();
    (netlist, world, candidate)
}

#[test]
fn typed_matrix_adapter_records_real_routes_and_runs_existing_certifiers() {
    use reda::compile::fragment_synth::{
        realise::realise_and_verify_expanded, verify::certify_expanded_structure,
    };
    let (netlist, world, candidate) = typed_matrix_candidate([1, 6, 3, 5]);
    let (_, _, repeated) = typed_matrix_candidate([1, 6, 3, 5]);
    assert_eq!(candidate.fingerprint(), repeated.fingerprint());
    candidate.validate_shape().unwrap();
    candidate.validate_route_timing().unwrap();
    let library = typed_matrix_library();
    certify_expanded_structure(&candidate, &netlist, &library).unwrap();
    let replay = candidate.emit_world(world.size()).unwrap();
    for y in 0..world.size().1 {
        for z in 0..world.size().2 {
            for x in 0..world.size().0 {
                assert_eq!(
                    world.get(x, y, z),
                    replay.get(x, y, z),
                    "owned replay at ({x},{y},{z})"
                );
            }
        }
    }
    let certified = realise_and_verify_expanded(&candidate, &netlist, &library).unwrap();
    let emitted = certified.world();
    for y in 0..world.size().1 {
        for z in 0..world.size().2 {
            for x in 0..world.size().0 {
                assert_eq!(
                    world.get(x, y, z),
                    emitted.get(x, y, z),
                    "typed emission at ({x},{y},{z})"
                );
            }
        }
    }
    let views = candidate.compatibility_views(&netlist).unwrap();
    let position = |p: (i32, i32, i32)| Position::new(p.0, p.1, p.2);
    let inputs = netlist
        .inputs
        .iter()
        .map(|n| position(views.input_positions[n]))
        .collect();
    let drivers = netlist
        .outputs
        .iter()
        .map(|n| position(views.gate_output_positions[n]))
        .collect();
    let lamps = netlist
        .outputs
        .iter()
        .map(|n| position(views.output_positions[n]))
        .collect();
    let quality = measure_rom(
        certified.world().clone(),
        inputs,
        drivers,
        lamps,
        &[1, 6, 3, 5],
    );
    let frozen: serde_json::Value = serde_json::from_str(include_str!(
        "../docs/generator-torch-row-four-word-2026-09-30.json"
    ))
    .unwrap();
    for (metric, value) in quality.as_object().unwrap() {
        if metric == "traces" {
            continue;
        }
        assert!(
            value.as_u64().unwrap() <= frozen["balanced"][metric].as_u64().unwrap(),
            "certified geometry quality regression: {metric}"
        );
    }
    let report = json!({"structurally_certified":true,"strict_physically_certified":true,"exact_owned_and_durable_replay":true,"candidate_fingerprint":candidate.fingerprint(),"route_count":candidate.routes.len(),"quality":quality});
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/torch_row_typed_certification.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn typed_matrix_certifiers_reject_forged_regions_missing_contributors_and_reversed_repeaters() {
    use reda::compile::{
        fragment_synth::{
            candidate::PlacedBlock,
            realise::{realise_and_verify_expanded, CertificationError},
            verify::certify_expanded_structure,
        },
        geometry::Anchor,
        verification::ExpandedPhysicalError,
    };
    let (netlist, _, candidate) = typed_matrix_candidate([1, 6, 3, 5]);
    let library = typed_matrix_library();
    let mut missing = candidate.clone();
    missing
        .junctions
        .values_mut()
        .next()
        .unwrap()
        .contributors
        .pop();
    assert!(certify_expanded_structure(&missing, &netlist, &library).is_err());
    let mut forged = candidate.clone();
    let junction = forged.junctions.values_mut().next().unwrap();
    junction.cells.push(PlacedBlock {
        at: Anchor { x: 26, y: 1, z: 1 },
        state: block(BlockKind::RedstoneWire),
    });
    assert!(matches!(
        realise_and_verify_expanded(&forged, &netlist, &library),
        Err(CertificationError::Physical(
            ExpandedPhysicalError::DisconnectedJunctionCell { .. }
        ))
    ));
    let mut reversed = candidate.clone();
    let repeater = reversed
        .routes
        .values_mut()
        .flat_map(|r| r.cells.iter_mut())
        .find(|b| b.state.kind == BlockKind::Repeater)
        .unwrap();
    repeater.state.facing = repeater.state.facing.map(Facing::opposite);
    assert!(matches!(
        realise_and_verify_expanded(&reversed, &netlist, &library),
        Err(CertificationError::Physical(
            ExpandedPhysicalError::WrongRepeaterAxis { .. }
        ))
    ));
}

fn matrix_frontend_text(words: [u8; 4]) -> String {
    let mut source = String::from("module source_rom(input logic a,b,output logic y0,y1,y2);\n");
    for bit in 0..3 {
        let values: Vec<_> = words
            .iter()
            .map(|w| format!("1'b{}", (w >> bit) & 1))
            .collect();
        source.push_str(&format!(
            "assign y{bit} = b ? (a ? {} : {}) : (a ? {} : {});\n",
            values[3], values[2], values[1], values[0]
        ));
    }
    source.push_str("endmodule");
    source
}

fn matrix_frontend_source(words: [u8; 4]) -> reda::compile::Netlist {
    let source = matrix_frontend_text(words);
    let artifact = compile_systemverilog(
        &[SourceInput {
            name: "source_rom.sv",
            text: &source,
        }],
        &CompileOptions::new("source_rom"),
    )
    .unwrap();
    let ports = artifact.output_map();
    let mut lowered = lower_optimised(&artifact.netlist).unwrap();
    // Materialize the original public boundary, including aliases, rather
    // than pretending the normalized matrix retains frontend gate identities.
    lowered.outputs = (0..3)
        .map(|bit| ports[&format!("y{bit}")].clone())
        .collect();
    lowered
}

fn verify_typed_matrix_internal_truth(
    candidate: &reda::compile::fragment_synth::candidate::ExpandedPhysicalCandidate,
    netlist: &reda::compile::Netlist,
    world: World,
) {
    use reda::compile::fragment_synth::identity::ObservationId;
    use reda::compile::topology::GateKind;
    let views = candidate.compatibility_views(netlist).unwrap();
    let order = netlist.combinational_order().unwrap();
    for instance in &candidate.instances.instances {
        reda::compile::fragment_synth::topology::prove_topology_semantics(
            &instance.expanded.topology,
            netlist.gates[instance.logical_gate.0 as usize].kind,
        )
        .unwrap();
    }
    let sites: std::collections::BTreeMap<_, _> = candidate
        .instances
        .instances
        .iter()
        .map(|i| {
            (
                i.logical_gate.0 as usize,
                candidate.observations[&ObservationId::InstanceOutput(i.id)]
                    .site
                    .at,
            )
        })
        .collect();
    let mut sim = Simulator::new(world);
    sim.run_until_stable(2000).unwrap();
    for from in 0..4 {
        for to in 0..4 {
            for address in [from, to] {
                let mut values = std::collections::BTreeMap::new();
                for (bit, name) in netlist.inputs.iter().enumerate() {
                    let (x, y, z) = views.input_positions[name];
                    let mut b = sim.world().get(x, y, z).clone();
                    b.lit = address & (1 << bit) != 0;
                    values.insert(name.clone(), b.lit);
                    sim.world_mut().set(x, y, z, b);
                }
                sim.run_until_stable(2000).unwrap();
                for &index in &order {
                    let gate = &netlist.gates[index];
                    let any = gate.inputs.iter().any(|n| values[n]);
                    let expected = match gate.kind {
                        GateKind::Nor(_) => !any,
                        GateKind::Or(_) | GateKind::Buf => any,
                        _ => unreachable!(),
                    };
                    let p = sites[&index];
                    let b = sim.world().get(p.x, p.y, p.z);
                    let actual = if b.kind == BlockKind::RedstoneWire {
                        b.power > 0
                    } else {
                        b.lit
                    };
                    assert_eq!(actual, expected, "address={address}, node={}", gate.output);
                    values.insert(gate.output.clone(), expected);
                }
            }
        }
    }
}

fn verify_typed_matrix_family(include_constants: bool) {
    let mut verified = 0;
    let mut improved = 0;
    let mut control_sum = 0u64;
    let mut candidate_sum = 0u64;
    let mut worst = 0u64;
    let mut rewrite_proof_fingerprints = Vec::new();
    let prior: serde_json::Value = serde_json::from_str(include_str!(
        "../docs/generator-torch-row-prior-nonconstant-caps-2026-09-30.json"
    ))
    .unwrap();
    let prior_caps: std::collections::BTreeMap<_, _> = prior["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row[0].as_u64().unwrap(),
                row.as_array().unwrap()[1..].to_vec(),
            )
        })
        .collect();

    for encoding in 0..4096u32 {
        let words: [u8; 4] = std::array::from_fn(|i| ((encoding >> (3 * i)) & 7) as u8);
        if !include_constants
            && (0..3).any(|bit| {
                words.iter().all(|w| w & (1 << bit) == 0)
                    || words.iter().all(|w| w & (1 << bit) != 0)
            })
        {
            continue;
        }
        let (netlist, original, candidate) = typed_matrix_candidate(words);
        let source = matrix_frontend_source(words);
        let result =
            reda::compile::fragment_synth::torch_row::compile_torch_row(&source, 1_000_000)
                .unwrap_or_else(|e| panic!("words={words:?}, encoding={encoding}: {e:?}"));
        let rewrite = result.rewrite_certificate();
        let physical = result.equivalence_certificate();
        assert_eq!(result.source(), &source);
        assert_eq!(result.implemented(), &netlist);
        assert_eq!(result.source_input_positions().len(), source.inputs.len());
        assert_eq!(result.source_output_positions().len(), source.outputs.len());
        assert_eq!(
            result.source_output_driver_positions().len(),
            source.outputs.len()
        );
        assert_eq!(
            rewrite.rewritten_netlist_hash(),
            &physical.lowered_netlist_hash
        );
        assert_eq!(physical.candidate_fingerprint, candidate.fingerprint());
        rewrite_proof_fingerprints.push(rewrite.fingerprint().clone());
        if words == [1, 6, 3, 5] {
            std::fs::write(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("target/torch_row_frontend_rewrite_example.json"),
                serde_json::to_string_pretty(&json!({"rewrite":rewrite,"physical":physical}))
                    .unwrap(),
            )
            .unwrap();
        }
        let (_, _, repeated) = typed_matrix_candidate(words);
        assert_eq!(candidate.fingerprint(), repeated.fingerprint());
        let certified = result.physical();
        for y in 0..original.size().1 {
            for z in 0..original.size().2 {
                for x in 0..original.size().0 {
                    assert_eq!(
                        original.get(x, y, z),
                        certified.world().get(x, y, z),
                        "emission mismatch words={words:?} at ({x},{y},{z})"
                    );
                }
            }
        }
        verify_typed_matrix_internal_truth(&candidate, &netlist, certified.world().clone());
        let views = result.implemented_observations();
        assert_eq!(views, &candidate.compatibility_views(&netlist).unwrap());
        let position = |p: (i32, i32, i32)| Position::new(p.0, p.1, p.2);
        let measured = measure_rom(
            certified.world().clone(),
            netlist
                .inputs
                .iter()
                .map(|n| position(views.input_positions[n]))
                .collect(),
            netlist
                .outputs
                .iter()
                .map(|n| position(views.gate_output_positions[n]))
                .collect(),
            netlist
                .outputs
                .iter()
                .map(|n| position(views.output_positions[n]))
                .collect(),
            &words,
        );
        let (w, inputs, drivers, lamps) =
            matrix_four_words_with_aligned_refresh(words, true, true, true);
        let control = measure_rom(w, inputs, drivers, lamps, &words);
        for (metric, value) in measured.as_object().unwrap() {
            if metric == "traces" {
                continue;
            }
            assert!(
                value.as_u64().unwrap() <= control[metric].as_u64().unwrap(),
                "quality regression words={words:?}: {metric}, candidate={value}, control={}",
                control[metric]
            );
        }
        if let Some(caps) = prior_caps.get(&u64::from(encoding)) {
            for (metric, cap) in prior["metrics"].as_array().unwrap().iter().zip(caps) {
                let metric = metric.as_str().unwrap();
                assert!(
                    measured[metric].as_u64().unwrap() <= cap.as_u64().unwrap(),
                    "prior accepted adapter regression words={words:?}: {metric}"
                );
            }
        }
        control_sum += control["total_settle_ticks"].as_u64().unwrap();
        candidate_sum += measured["total_settle_ticks"].as_u64().unwrap();
        improved += usize::from(measured["total_settle_ticks"] != control["total_settle_ticks"]);
        worst = worst.max(measured["worst_settle_ticks"].as_u64().unwrap());
        verified += 1;
    }
    assert_eq!(verified, if include_constants { 4096 } else { 2744 });
    let report = json!({"data_tables":verified,"measured_transitions_per_arm":verified*16,
        "strict_certification":true,"exact_emission":true,"deterministic_candidates":true,
        "frontend_source_rewrites_proved":rewrite_proof_fingerprints.len(),
        "frontend_to_physical_proof_chain":true,
        "rewrite_proof_set_fingerprint":reda::compile::metrics::canonical_fingerprint(
            &serde_json::to_vec(&rewrite_proof_fingerprints).unwrap()),
        "all_internal_setup_and_target_truth_correct":true,"all_topologies_symbolically_proved":true,
        "all_nine_metrics_preserve_per_table_controls":true,"all_prior_nonconstant_caps_preserved":true,"control_total_settle_ticks":control_sum,
        "candidate_total_settle_ticks":candidate_sum,"improved_tables":improved,"worst_settle_ticks":worst});
    println!("{report}");
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(if include_constants {
            "target/torch_row_typed_all_functions.json"
        } else {
            "target/torch_row_typed_exhaustive.json"
        }),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn typed_matrix_refresh_rejects_missing_delay_owner_and_foreign_output_wire() {
    use reda::compile::fragment_synth::{
        candidate::PlacedBlock,
        realise::{realise_and_verify_expanded, CertificationError},
    };
    use reda::compile::{geometry::Anchor, verification::ExpandedPhysicalError};
    let (netlist, world, candidate) = typed_matrix_candidate([3, 6, 6, 1]);
    let library = typed_matrix_library();
    let mut missing = candidate.clone();
    let branch = missing
        .routes
        .values_mut()
        .flat_map(|r| r.branches.iter_mut())
        .find(|b| b.terminal.delayed_owner.is_some())
        .unwrap();
    branch.terminal.delayed_owner = None;
    assert!(realise_and_verify_expanded(&missing, &netlist, &library).is_err());
    let mut shorted = candidate;
    let repeater = shorted
        .placements
        .values()
        .find(|p| p.delayed.is_some())
        .unwrap();
    let at = Anchor {
        x: repeater.anchor.x - 1,
        y: repeater.anchor.y,
        z: repeater.anchor.z + 1,
    };
    assert_eq!(world.get(at.x, at.y, at.z).kind, BlockKind::Air);
    let route = shorted
        .routes
        .values_mut()
        .find(|r| {
            matches!(
                r.source,
                reda::compile::fragment_synth::identity::PhysicalEndpointId::PrimaryInput(_)
            )
        })
        .unwrap();
    route.cells.push(PlacedBlock {
        at,
        state: block(BlockKind::RedstoneWire),
    });
    route.floors.push(PlacedBlock {
        at: Anchor { y: at.y - 1, ..at },
        state: block(BlockKind::Solid),
    });
    assert!(matches!(
        realise_and_verify_expanded(&shorted, &netlist, &library),
        Err(CertificationError::Physical(
            ExpandedPhysicalError::CrossRouteConnectivity { .. }
        ))
    ));
}

#[test]
fn matrix_rewrite_chain_checks_fresh_yosys_and_rejects_wrong_source_data() {
    use reda::compile::equivalence::{prove_netlist_rewrite, NetlistRewriteError};
    for words in [[1, 6, 3, 5], [3, 6, 6, 1]] {
        let text = matrix_frontend_text(words);
        let (source, ports) = reda::frontend::synthesize_verilog(&text, "source_rom").unwrap();
        let mut source = lower_optimised(&source).unwrap();
        source.outputs = (0..3)
            .map(|bit| ports[&format!("y{bit}")].clone())
            .collect();
        let (normalized, _, candidate) = typed_matrix_candidate(words);
        let result =
            reda::compile::fragment_synth::torch_row::compile_torch_row(&source, 1_000_000)
                .unwrap();
        assert_eq!(result.implemented(), &normalized);
        let rewrite = result.rewrite_certificate();
        let physical = result.equivalence_certificate();
        assert_eq!(
            rewrite.rewritten_netlist_hash(),
            &physical.lowered_netlist_hash
        );
        assert_eq!(physical.candidate_fingerprint, candidate.fingerprint());
    }
    let wrong_source = matrix_frontend_source([1, 6, 3, 4]);
    let (normalized, _, _) = typed_matrix_candidate([1, 6, 3, 5]);
    assert!(matches!(
        prove_netlist_rewrite(
            &wrong_source,
            &normalized,
            &wrong_source.inputs,
            &normalized.outputs,
            1_000_000
        ),
        Err(NetlistRewriteError::OutputMismatch {
            input_vector: 3,
            ..
        })
    ));
}

#[test]
fn all_nonconstant_matrix_tables_run_strict_typed_certification() {
    verify_typed_matrix_family(false);
}

#[test]
fn all_matrix_tables_including_constants_run_strict_typed_certification() {
    verify_typed_matrix_family(true);
}

#[test]
fn typed_constant_support_rejects_a_foreign_owned_wire() {
    use reda::compile::fragment_synth::{
        candidate::PlacedBlock,
        identity::{ObservationId, PhysicalEndpointId},
        realise::{realise_and_verify_expanded, CertificationError},
    };
    use reda::compile::{geometry::Anchor, verification::ExpandedPhysicalError};
    let (netlist, world, mut candidate) = typed_matrix_candidate([0; 4]);
    let route = *candidate
        .routes
        .iter()
        .find(|(_, r)| {
            let PhysicalEndpointId::PrimitiveOutput(id) = r.source else {
                return false;
            };
            let at = candidate.observations[&ObservationId::PrimitiveOutput(id)]
                .site
                .at;
            at.x == 21 && at.y == 2
        })
        .unwrap()
        .0;
    let at = Anchor { x: 17, y: 4, z: 5 };
    assert_eq!(world.get(at.x, at.y, at.z).kind, BlockKind::Air);
    let route = candidate.routes.get_mut(&route).unwrap();
    route.cells.push(PlacedBlock {
        at,
        state: block(BlockKind::RedstoneWire),
    });
    route.floors.push(PlacedBlock {
        at: Anchor { y: at.y - 1, ..at },
        state: block(BlockKind::Solid),
    });
    // A second cell makes this a north/south run aimed into the support,
    // rather than an isolated dot whose horizontal output is world-dependent.
    let north = Anchor { z: at.z - 1, ..at };
    route.cells.push(PlacedBlock {
        at: north,
        state: block(BlockKind::RedstoneWire),
    });
    route.floors.push(PlacedBlock {
        at: Anchor {
            y: north.y - 1,
            ..north
        },
        state: block(BlockKind::Solid),
    });
    let actual = realise_and_verify_expanded(&candidate, &netlist, &typed_matrix_library());
    assert!(
        matches!(
            &actual,
            Err(CertificationError::Physical(
                ExpandedPhysicalError::CrossComponentCoupling { .. }
            ))
        ),
        "unexpected certification error: {:?}",
        actual.err()
    );
}

#[test]
fn physical_rewrite_preserves_ordered_boundaries_and_rejects_partial_proofs() {
    use reda::compile::equivalence::NetlistRewriteError;
    use reda::compile::fragment_synth::rewrite::{realise_physical_rewrite, PhysicalRewriteError};
    let (normalized, _, candidate) = typed_matrix_candidate([1, 6, 3, 5]);
    let library = typed_matrix_library();
    let mut source = matrix_frontend_source([1, 6, 3, 5]);
    source.inputs.reverse();
    source.outputs.reverse();
    let mut inputs = normalized.inputs.clone();
    inputs.reverse();
    let mut outputs = normalized.outputs.clone();
    outputs.reverse();
    let result = realise_physical_rewrite(
        &source,
        &normalized,
        &candidate,
        &library,
        &inputs,
        &outputs,
        1_000_000,
    )
    .unwrap();
    let views = result.implemented_observations();
    for (i, name) in inputs.iter().enumerate() {
        assert_eq!(
            result.source_input_positions()[i],
            views.input_positions[name]
        );
    }
    for (i, name) in outputs.iter().enumerate() {
        assert_eq!(
            result.source_output_positions()[i],
            views.output_positions[name]
        );
        assert_eq!(
            result.source_output_driver_positions()[i],
            views.gate_output_positions[name]
        );
    }
    assert!(matches!(
        realise_physical_rewrite(
            &source,
            &normalized,
            &candidate,
            &library,
            &inputs,
            &outputs,
            0
        ),
        Err(PhysicalRewriteError::Rewrite(
            NetlistRewriteError::ProofExhausted { .. }
        ))
    ));
    assert!(matches!(
        realise_physical_rewrite(
            &source,
            &normalized,
            &candidate,
            &library,
            &inputs,
            &outputs[..2],
            1_000_000
        ),
        Err(PhysicalRewriteError::Rewrite(
            NetlistRewriteError::InvalidPortBindings
        ))
    ));
    let mut broken = candidate.clone();
    broken
        .junctions
        .values_mut()
        .next()
        .unwrap()
        .contributors
        .pop();
    assert!(realise_physical_rewrite(
        &source,
        &normalized,
        &broken,
        &library,
        &inputs,
        &outputs,
        1_000_000
    )
    .is_err());
    let wrong = matrix_frontend_source([1, 6, 3, 4]);
    assert!(matches!(
        realise_physical_rewrite(
            &wrong,
            &normalized,
            &candidate,
            &library,
            &normalized.inputs,
            &normalized.outputs,
            1_000_000
        ),
        Err(PhysicalRewriteError::Rewrite(
            NetlistRewriteError::OutputMismatch {
                input_vector: 3,
                ..
            }
        ))
    ));
}

#[test]
fn torch_row_recognition_uses_functions_and_refuses_invalid_or_unqualified_sources() {
    use reda::compile::fragment_synth::torch_row::{
        compile_player_torch_row, compile_torch_row, TorchRowError,
    };
    for compile_torch_row in [compile_torch_row, compile_player_torch_row] {
        let mut source = matrix_frontend_source([1, 6, 3, 5]);
        let rename = |name: &str| format!("arbitrary_{name}");
        for gate in &mut source.gates {
            gate.name = rename(&gate.name);
            gate.output = rename(&gate.output);
            gate.inputs = gate.inputs.iter().map(|s| rename(s)).collect();
        }
        source.inputs = source.inputs.iter().map(|s| rename(s)).collect();
        source.outputs = source.outputs.iter().map(|s| rename(s)).collect();
        source.inputs.reverse();
        source.outputs.reverse();
        let result = compile_torch_row(&source, 1_000_000).unwrap();
        assert_eq!(result.source(), &source);
        assert_eq!(result.source_input_positions().len(), 2);
        assert_eq!(result.source_output_positions().len(), 3);
        let repeated = compile_torch_row(&source, 1_000_000).unwrap();
        assert_eq!(
            result.equivalence_certificate(),
            repeated.equivalence_certificate()
        );
        assert!(compile_torch_row(&source, 0).is_err());
        let mut omitted = source.clone();
        omitted.outputs.pop();
        assert!(matches!(
            compile_torch_row(&omitted, 1_000_000),
            Err(TorchRowError::UnsupportedBoundary)
        ));
        let mut invalid = source.clone();
        invalid.gates[0].inputs.push("undriven".into());
        assert!(compile_torch_row(&invalid, 1_000_000).is_err());
        let mut stateful = source.clone();
        stateful.gates[0].kind = reda::compile::topology::GateKind::DffPosedge;
        assert!(compile_torch_row(&stateful, 1_000_000).is_err());
    }
}

#[test]
fn torch_row_entry_records_quality_against_current_shipping_layouts() {
    let mut cases = Vec::new();
    for words in [
        [1, 6, 3, 5],
        [3, 6, 6, 1],
        [0, 1, 2, 3],
        [0, 0, 0, 7],
        [0; 4],
        [7; 4],
    ] {
        let source = matrix_frontend_source(words);
        // Frontend artifacts keep public aliases in output_map, while the
        // shipping physical netlist has one declaration per driven signal.
        let mut shipping_netlist = source.clone();
        let mut seen = std::collections::BTreeSet::new();
        shipping_netlist
            .outputs
            .retain(|name| seen.insert(name.clone()));
        let shipping = compile(&shipping_netlist).unwrap();
        let row = reda::compile::fragment_synth::torch_row::compile_torch_row(&source, 1_000_000)
            .unwrap();
        let position = |p: (i32, i32, i32)| Position::new(p.0, p.1, p.2);
        let current = measure_rom(
            shipping.world.clone(),
            source
                .inputs
                .iter()
                .map(|n| position(shipping.input_positions[n]))
                .collect(),
            source
                .outputs
                .iter()
                .map(|n| position(shipping.gate_output_positions[n]))
                .collect(),
            source
                .outputs
                .iter()
                .map(|n| position(shipping.output_positions[n]))
                .collect(),
            &words,
        );
        let candidate = measure_rom(
            row.physical().world().clone(),
            row.source_input_positions()
                .iter()
                .copied()
                .map(position)
                .collect(),
            row.source_output_driver_positions()
                .iter()
                .copied()
                .map(position)
                .collect(),
            row.source_output_positions()
                .iter()
                .copied()
                .map(position)
                .collect(),
            &words,
        );
        cases.push(json!({"words":words,"shipping":current,"torch_row":candidate}));
    }
    // Preserve the independent pre-experiment shipping controls per metric;
    // one improved case cannot compensate for a different ROM regression.
    let frozen: serde_json::Value = serde_json::from_str(include_str!(
        "../docs/generator-torch-row-shipping-comparison-2026-09-30.json"
    ))
    .unwrap();
    for case in &cases {
        let baseline = frozen["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["words"] == case["words"])
            .unwrap();
        for metric in [
            "driver_glitches",
            "driver_single_input_glitches",
            "lamp_glitches",
            "lamp_single_input_glitches",
            "total_driver_pulse_ticks",
            "total_settle_ticks",
            "worst_driver_change_ticks",
            "worst_driver_pulse_ticks",
            "worst_settle_ticks",
        ] {
            assert!(
                case["shipping"][metric].as_u64().unwrap()
                    <= baseline["shipping"][metric].as_u64().unwrap(),
                "ROM {:?}: frozen shipping {metric} regression",
                case["words"]
            );
        }
    }
    let report = json!({"comparison_commit":"5d9d64c","measured_transitions_per_arm":96,"cases":cases,
        "automatic_selection_enabled":false});
    println!("{report}");
    std::fs::write(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/torch_row_shipping_comparison.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[test]
fn player_matrix_certifies_native_and_fresh_yosys_sources() {
    use reda::compile::equivalence::prove_netlist_rewrite;
    use reda::compile::fragment_synth::{
        benchmark::canonical_world_fingerprint, torch_row::compile_player_torch_row,
    };
    for words in [
        [0, 0, 0, 1], // Comparator AND with one selected variable output.
        [0, 0, 0, 3], // Comparator AND shares its condition across two outputs.
        [2, 2, 2, 7], // Comparator AND and constant high output.
        [1, 1, 1, 7], // Constant first port preserves ordered AND outputs.
        [0, 7, 0, 0], // Direct subtract cube: a AND NOT b with shared output fanout.
        [0, 0, 7, 0], // Reverse the comparator rear/side input roles.
        [2, 7, 2, 2], // Mixed-sign cube coexists with constant high output.
        [1, 1, 7, 1], // Reversed cube and constants preserve ordered source ports.
        [1, 2, 1, 2], // One primary boundary feeds both polarities of a.
        [1, 1, 2, 2], // Same dual-polarity layout for b.
        [5, 6, 1, 2], // Both polarities of a coexist with negative b.
        [5, 1, 6, 2], // Both polarities of b coexist with negative a.
        [2, 3, 0, 1], // Positive a and negative b use separate short output domains.
        [2, 0, 3, 1], // Positive b and negative a exchange cluster roles.
        [3, 2, 1, 0], // Independent negative a/b outputs plus constant low.
        [7, 6, 5, 4], // Independent negative a/b outputs plus constant high.
        [1, 6, 3, 5],
        [1, 1, 0, 0], // Direct primary input to a noninverting tap.
        [7, 1, 2, 0], // Three shared repeater-only rows without input buffers.
        [0, 1, 0, 1], // Positive a through noninverting control and data repeaters.
        [0, 0, 4, 4], // Positive b reaches the final output row extension.
        [0, 1, 1, 1], // Two positive literals merge without torch reconvergence.
        [2, 7, 7, 1], // The last zero tap formerly overwrote the source observation.
        [2, 5, 5, 6], // Six rows require two independently isolated buffers.
        [3, 6, 6, 1],
        [0, 0, 0, 7],
        [0; 4],
        [7; 4],
        [1; 4],
    ] {
        let native = matrix_frontend_source(words);
        let text = matrix_frontend_text(words);
        let (yosys, ports) = reda::frontend::synthesize_verilog(&text, "source_rom").unwrap();
        let mut yosys = lower_optimised(&yosys).unwrap();
        yosys.outputs = (0..3)
            .map(|bit| ports[&format!("y{bit}")].clone())
            .collect();
        let a = compile_player_torch_row(&native, 1_000_000).unwrap();
        let b = compile_player_torch_row(&yosys, 1_000_000).unwrap();
        assert_eq!(a.implemented(), b.implemented());
        assert_eq!(
            canonical_world_fingerprint(a.physical().world()),
            canonical_world_fingerprint(b.physical().world())
        );
        assert_eq!(
            a.equivalence_certificate().candidate_fingerprint,
            b.equivalence_certificate().candidate_fingerprint
        );
        assert_eq!(
            a.rewrite_certificate().rewritten_netlist_hash(),
            &a.equivalence_certificate().lowered_netlist_hash
        );
        assert_eq!(a.source_output_positions().len(), 3);
        assert!(a.source_output_positions().windows(2).all(|p| p[0] != p[1]));
    }
    let intended =
        compile_player_torch_row(&matrix_frontend_source([1, 6, 3, 5]), 1_000_000).unwrap();
    let wrong = matrix_frontend_source([1, 6, 3, 4]);
    assert!(prove_netlist_rewrite(
        &wrong,
        intended.implemented(),
        &intended.implemented().inputs,
        &intended.implemented().outputs,
        1_000_000
    )
    .is_err());
}
