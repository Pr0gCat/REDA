//! Physical premise experiment only. No certification or compiler integration.
use reda::compile::Netlist;
use reda::formats::litematic::{load, save};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::{position::Position, Simulator};
use reda::redstone::world::{block::{BlockKind, BlockState, Face, Facing}, storage::World};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};

enum Expected {
    Bit(usize, bool),
    Bad(usize, usize),
    Row(usize),
    Prefix(usize, usize),
}
impl Expected {
    fn value(&self, address: usize, words: &[u64], rows: &[usize]) -> bool {
        match *self {
            Self::Bit(bit, inverted) => (address & (1 << bit) != 0) ^ inverted,
            Self::Bad(row, mask) => (address ^ row) & mask != 0,
            Self::Row(row) => address == row,
            Self::Prefix(bit, last) => rows.iter().take(last + 1).any(|row| *row == address)
                && words[address] & (1 << bit) != 0,
        }
    }
    fn description(&self) -> serde_json::Value {
        match *self {
            Self::Bit(bit, inverted) => json!({"kind":"bit","bit":bit,"inverted":inverted}),
            Self::Bad(row, mask) => json!({"kind":"mismatch","row":row,"mask":mask}),
            Self::Row(row) => json!({"kind":"row","row":row}),
            Self::Prefix(bit, last) => json!({"kind":"prefix","bit":bit,"last":last}),
        }
    }
}

fn block(kind: BlockKind) -> BlockState {
    let mut state = BlockState::air();
    state.kind = kind;
    state.name = match kind {
        BlockKind::Solid => "minecraft:stone",
        BlockKind::RedstoneWire => "minecraft:redstone_wire",
        BlockKind::WallTorch => "minecraft:redstone_wall_torch",
        BlockKind::Repeater => "minecraft:repeater",
        BlockKind::Lever => "minecraft:lever",
        BlockKind::Lamp => "minecraft:redstone_lamp",
        _ => unreachable!(),
    }.into();
    state
}
fn wire(world: &mut World, x: i32, y: i32, z: i32) {
    world.set(x, y - 1, z, block(BlockKind::Solid));
    world.set(x, y, z, block(BlockKind::RedstoneWire));
}
fn torch(world: &mut World, x: i32, y: i32, z: i32, facing: Facing) {
    let mut state = block(BlockKind::WallTorch);
    state.facing = Some(facing);
    state.lit = true;
    world.set(x, y, z, state);
}
fn repeater(world: &mut World, x: i32, y: i32, z: i32, facing: Facing, delay: u8) {
    let mut state = block(BlockKind::Repeater);
    state.facing = Some(facing);
    state.delay = delay;
    world.set(x, y, z, state);
}
fn signal(world: &World, position: Position) -> bool {
    let state = world.get(position.x, position.y, position.z);
    if state.kind == BlockKind::RedstoneWire { state.power > 0 } else { state.lit }
}
fn words(source: &Netlist) -> Vec<u64> {
    let order = source.combinational_order().unwrap();
    (0..1usize << source.inputs.len()).map(|address| {
        let mut values: HashMap<_, _> = source.inputs.iter().enumerate()
            .map(|(bit, name)| (name.clone(), address & (1 << bit) != 0)).collect();
        for &index in &order {
            let gate = &source.gates[index];
            let operands: Vec<_> = gate.inputs.iter().map(|name| values[name]).collect();
            values.insert(gate.output.clone(), gate.kind.evaluate(&operands));
        }
        source.outputs.iter().enumerate().fold(0, |word, (bit, name)|
            word | (u64::from(values[name]) << bit))
    }).collect()
}

fn matrix(words: &[u64], rows: &[usize], outputs: usize, short_output: bool) -> (World, Vec<Position>, Vec<Position>, Vec<Position>) {
    assert_eq!(words.len(), 16);
    assert!(!rows.is_empty() && rows.iter().all(|row| *row < words.len()));
    assert!((1..=7).contains(&outputs));
    let last_row = 4 + 4 * (rows.len() as i32 - 1);
    let end = last_row + 3;
    let last_column = 30 + 3 * (outputs as i32 - 1);
    let mut world = World::new(last_column + 4, 8, end + 4);
    let mut inputs = Vec::new();
    for bit in 0..4 {
        let x = 3 + 6 * bit;
        for column in [x, x + 3] {
            for z in 1..=last_row { wire(&mut world, column, 5, z); }
        }
        let mut lever = block(BlockKind::Lever);
        lever.face = Some(Face::Floor);
        lever.facing = Some(Facing::North);
        world.set(x, 5, 1, lever);
        inputs.push(Position::new(x, 5, 1));
        torch(&mut world, x + 1, 4, 1, Facing::East);
        wire(&mut world, x + 2, 4, 1);
        // Both polarities share arrival time. Later address columns skip one
        // row refresh and therefore receive one extra input-buffer stage.
        let extra = u8::from(bit >= 2);
        repeater(&mut world, x, 5, 2, Facing::North, 2 + extra);
        repeater(&mut world, x + 3, 5, 2, Facing::North, 1 + extra);
        for z in (14..=last_row).step_by(16) {
            for column in [x, x + 3] {
                repeater(&mut world, column, 5, z, Facing::North, 1);
            }
        }
    }
    for (index, &address) in rows.iter().enumerate() {
        let word = words[address];
        let z = 4 + 4 * index as i32;
        for x in 2..=last_column { wire(&mut world, x, 3, z); }
        for x in [14, 28, 41].into_iter().filter(|x| *x <= last_column) {
            repeater(&mut world, x, 3, z, Facing::West, 1);
        }
        for bit in 0..4 {
            let x = 3 + 6 * bit as i32;
            let literal = if address & (1 << bit) != 0 { x } else { x + 3 };
            torch(&mut world, literal + 1, 4, z, Facing::East);
        }
        for bit in 0..outputs {
            if word & (1 << bit) != 0 {
                torch(&mut world, 30 + 3 * bit as i32, 2, z + 1, Facing::South);
            }
        }
    }
    let mut drivers = Vec::new();
    let mut lamps = Vec::new();
    for bit in 0..outputs {
        let x = 30 + 3 * bit as i32;
        for z in 5..=end { wire(&mut world, x, 1, z); }
        for z in (15..=end).step_by(16) {
            repeater(&mut world, x, 1, z, Facing::North, 1);
        }
        let delay = (if x < 41 { 2 } else { 1 }) - u8::from(short_output);
        let driver_z = if delay == 0 { end } else { end + 1 };
        if delay > 0 {
            world.set(x, 0, driver_z, block(BlockKind::Solid));
            repeater(&mut world, x, 1, driver_z, Facing::North, delay);
        }
        world.set(x, 0, driver_z + 1, block(BlockKind::Solid));
        world.set(x, 1, driver_z + 1, block(BlockKind::Lamp));
        drivers.push(Position::new(x, 1, driver_z));
        lamps.push(Position::new(x, 1, driver_z + 1));
    }
    (world, inputs, drivers, lamps)
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(args.len() >= 3 && args[3..].iter().all(|arg| arg == "prune-empty" || arg == "short-output"),
        "source path, top and optional prune-empty/short-output experiments required");
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let source = compile_systemverilog(&[SourceInput { name: &args[2], text: &text }],
        &CompileOptions::new(&args[2])).unwrap().netlist;
    assert_eq!(source.inputs.len(), 4);
    let words = words(&source);
    let prune_empty = args[3..].iter().any(|arg| arg == "prune-empty");
    let short_output = args[3..].iter().any(|arg| arg == "short-output");
    let mut rows: Vec<_> = (0..16).filter(|row| !prune_empty || words[*row] != 0).collect();
    if rows.is_empty() { rows.push(0); }
    let (world, inputs, drivers, lamps) = matrix(&words, &rows, source.outputs.len(), short_output);
    let last_row = 4 + 4 * (rows.len() as i32 - 1);
    let end = last_row + 3;
    let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world);
    let artifact = format!("/tmp/reda-shared-matrix-{}.litematic", std::process::id());
    save(std::path::Path::new(&artifact), &world, "shared-matrix-premise").unwrap();
    let loaded = load(std::path::Path::new(&artifact)).unwrap();
    assert_eq!(fingerprint,
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&loaded));
    let mut sim = Simulator::new(loaded);
    sim.run_until_stable(2000).unwrap();
    let width = source.outputs.len();
    let mut watched: Vec<_> = drivers.iter().chain(&lamps).enumerate()
        .map(|(index, p)| (*p, format!("boundary{index}"), Expected::Prefix(index % width, rows.len() - 1))).collect();
    let last_column = 30 + 3 * (source.outputs.len() as i32 - 1);
    for (index, &row) in rows.iter().enumerate() {
        watched.push((Position::new(last_column, 3, 4 + 4 * index as i32), format!("row_bad:{row}"), Expected::Bad(row, 15)));
    }
    for bit in 0..4 {
        let x = 3 + 6 * bit as i32;
        watched.push((inputs[bit], format!("input:{bit}"), Expected::Bit(bit, false)));
        watched.push((Position::new(x + 1, 4, 1), format!("input_inverse:{bit}"), Expected::Bit(bit, true)));
        for (column, inverted) in [(x, false), (x + 3, true)] {
            let sign = if inverted { "negative" } else { "positive" };
            for z in [2, 14, 30, 46, 62].into_iter().filter(|z| *z <= last_row) {
                watched.push((Position::new(column, 5, z), format!("literal_buffer:{bit}:{sign}:{z}"), Expected::Bit(bit, inverted)));
            }
        }
    }
    for (index, &row) in rows.iter().enumerate() {
        let word = words[row];
        let z = 4 + 4 * index as i32;
        watched.push((Position::new(13, 3, z), format!("row_pair_bad:{row}"), Expected::Bad(row, 3)));
        watched.push((Position::new(27, 3, z), format!("row_merge_bad:{row}"), Expected::Bad(row, 15)));
        for bit in 0..4 {
            let x = 3 + 6 * bit as i32;
            let literal = if row & (1 << bit) != 0 { x } else { x + 3 };
            watched.push((Position::new(literal + 1, 4, z), format!("row_literal_bad:{row}:{bit}"), Expected::Bad(row, 1 << bit)));
        }
        for x in [14, 28, 41].into_iter().filter(|x| *x <= last_column) {
            watched.push((Position::new(x, 3, z), format!("row_buffer:{row}:{x}"), Expected::Bad(row, if x == 14 { 3 } else { 15 })));
        }
        for bit in 0..width {
            if word & (1 << bit) != 0 {
                watched.push((Position::new(30 + 3 * bit as i32, 2, z + 1), format!("program:{row}:{bit}"), Expected::Row(row)));
            }
        }
    }
    for bit in 0..width {
        let x = 30 + 3 * bit as i32;
        for z in [15, 31, 47, 63].into_iter().filter(|z| *z <= end) {
            if drivers[bit] == Position::new(x, 1, z) {
                assert_eq!((z - 6) as usize / 4, rows.len() - 1);
                continue; // The public driver already observes this exact final buffer.
            }
            watched.push((Position::new(x, 1, z), format!("output_buffer:{bit}:{z}"), Expected::Prefix(bit, (z - 6) as usize / 4)));
        }
        if drivers[bit] != Position::new(x, 1, end) {
            watched.push((Position::new(x, 1, end), format!("output_or:{bit}"), Expected::Prefix(bit, rows.len() - 1)));
        }
    }
    let positions: BTreeSet<_> = watched.iter().map(|(p, _, _)| (p.x, p.y, p.z)).collect();
    assert_eq!(positions.len(), watched.len(), "each observed implementation site is unique");
    for (flat, &palette) in world.cells().iter().enumerate() {
        let state = world.palette().get(palette).unwrap();
        if matches!(state.kind, BlockKind::Lever | BlockKind::WallTorch | BlockKind::Repeater | BlockKind::Lamp) {
            assert!(positions.contains(&world.decode(flat)), "missing implementation primitive observation");
        }
    }
    let definitions: BTreeMap<_, _> = watched.iter().map(|(p, label, expected)|
        (label.clone(), json!({"position":[p.x,p.y,p.z],"block_kind":format!("{:?}",world.get(p.x,p.y,p.z).kind),
            "oracle":expected.description()}))).collect();
    let power_on: BTreeMap<_, _> = watched.iter().map(|(p, label, expected)| {
        let value = signal(sim.world(), *p);
        assert_eq!(value, expected.value(0, &words, &rows), "power-on implementation {label}");
        (label.clone(), value)
    }).collect();
    sim.attach_observer(watched.iter().map(|(p, label, _)| (*p, label.clone())));
    println!("MATRIX_SOURCE {}", json!({"inputs": source.inputs, "outputs": source.outputs,
        "words": words,"rows":rows,"prune_empty":prune_empty,"short_output":short_output,"world_size":world.size(),
        "artifact": artifact, "certified": false,"world_fingerprint":fingerprint.as_str(),
        "definitions":definitions,"power_on":power_on,"source_gates":source.gates.iter().map(|gate|
            json!({"inputs":gate.inputs,"output":gate.output,"kind":gate.kind.wire_name()})).collect::<Vec<_>>()}));
    let mut quality = [[0u64; 12]; 2];
    let mut previous = 0usize;
    for from in 0..16 { for to in 0..16 {
        for (phase, address) in [from, to].into_iter().enumerate() {
            sim.reset_observer();
            let start = sim.current_tick();
            for (bit, p) in inputs.iter().enumerate() {
                let mut lever = sim.world().get(p.x, p.y, p.z).clone();
                lever.lit = address & (1 << bit) != 0;
                sim.world_mut().set(p.x, p.y, p.z, lever);
            }
            let ticks = sim.run_until_stable(2000).unwrap();
            let expected: Vec<_> = (0..source.outputs.len()).map(|bit| words[address] & (1 << bit) != 0).collect();
            let driver: Vec<_> = drivers.iter().map(|p| signal(sim.world(), *p)).collect();
            let lamp: Vec<_> = lamps.iter().map(|p| signal(sim.world(), *p)).collect();
            let row_bad: Vec<_> = rows.iter().enumerate().map(|(index, _)|
                signal(sim.world(), Position::new(last_column, 3, 4 + 4 * index as i32))).collect();
            let events: Vec<_> = sim.observations().iter().map(|event|
                json!({"tick": event.tick - start, "label": event.label, "value": event.value})).collect();
            let internal: BTreeMap<_, _> = watched.iter().map(|(p, label, expected)| {
                let value = signal(sim.world(), *p);
                assert_eq!(value, expected.value(address, &words, &rows), "implementation {label} at address {address}");
                (label.clone(), value)
            }).collect();
            println!("MATRIX_PHASE {}", json!({"from":from,"to":to,"phase":phase,
                "address":address,"ticks":ticks,"expected":expected,"driver":driver,"lamp":lamp,
                "row_bad":row_bad,"events":events,"internal":internal}));
            assert_eq!(row_bad, rows.iter().map(|row| *row != address).collect::<Vec<_>>(), "row decoder truth");
            assert_eq!(driver, expected, "driver truth");
            assert_eq!(lamp, expected, "lamp truth");
            let metrics = &mut quality[phase];
            metrics[0] = metrics[0].max(ticks);
            metrics[1] += ticks;
            for index in 0..2 * source.outputs.len() {
                let bit = index % source.outputs.len();
                let label = format!("boundary{index}");
                let events: Vec<_> = sim.observations().iter().filter(|event| event.label == label).collect();
                let offset = if index < source.outputs.len() { 2 } else { 7 };
                metrics[offset] += u64::from(events.len() > 1);
                metrics[offset + 1] += u64::from(events.len() > 1 && (previous ^ address).count_ones() == 1);
                metrics[offset + 2] = metrics[offset + 2].max(events.last().map_or(0, |event| event.tick - start));
                for pair in events.windows(2) {
                    if pair[0].value != expected[bit] {
                        let width = pair[1].tick - pair[0].tick;
                        metrics[offset + 3] += width;
                        metrics[offset + 4] = metrics[offset + 4].max(width);
                    }
                }
            }
            previous = address;
        }
    }}
    println!("MATRIX_RESULT {}", json!({"quality":quality,"phases":512,"certified":false}));
}
