//! Replay every implemented signal of the isolated strictly certified matrix experiment.
use reda::compile::Netlist;
use reda::formats::litematic::{load, save};
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::{position::Position, Simulator};
use reda::redstone::world::{block::BlockKind, storage::World};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};

enum Expected {
    Bit(usize, bool),
    Bad(usize, usize),
    Row(usize),
    Prefix(usize, usize),
    Gate(String, Vec<bool>),
}
impl Expected {
    fn value(&self, address: usize, words: &[u64], rows: &[usize]) -> bool {
        if let Self::Gate(_, values) = self { return values[address]; }
        match *self {
            Self::Gate(_, _) => unreachable!(),
            Self::Bit(bit, inverted) => (address & (1 << bit) != 0) ^ inverted,
            Self::Bad(row, mask) => (address ^ row) & mask != 0,
            Self::Row(row) => address == row,
            Self::Prefix(bit, last) => rows.iter().take(last + 1).any(|row| *row == address)
                && words[address] & (1 << bit) != 0,
        }
    }
    fn description(&self) -> serde_json::Value {
        match *self {
            Self::Gate(ref signal, _) => json!({"kind":"implemented_signal","signal":signal}),
            Self::Bit(bit, inverted) => json!({"kind":"bit","bit":bit,"inverted":inverted}),
            Self::Bad(row, mask) => json!({"kind":"mismatch","row":row,"mask":mask}),
            Self::Row(row) => json!({"kind":"row","row":row}),
            Self::Prefix(bit, last) => json!({"kind":"prefix","bit":bit,"last":last}),
        }
    }
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

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert!(args.len() >= 3 && args[3..].iter().all(|arg| arg == "prune-empty" || arg == "short-output" || arg == "separate-input-floor"),
        "source path, top and optional prune-empty/short-output experiments required");
    let text = std::fs::read_to_string(&args[1]).unwrap();
    let source = compile_systemverilog(&[SourceInput { name: &args[2], text: &text }],
        &CompileOptions::new(&args[2])).unwrap().netlist;
    assert!((4..=6).contains(&source.inputs.len()));
    let states = 1usize << source.inputs.len();
    let words = words(&source);
    let prune_empty = args[3..].iter().any(|arg| arg == "prune-empty");
    let short_output = args[3..].iter().any(|arg| arg == "short-output" || arg == "separate-input-floor");
    let mut rows: Vec<_> = (0..states).filter(|row| !prune_empty || words[*row] != 0).collect();
    if rows.is_empty() { rows.push(0); }
    let compiled = reda::compile::compile(&source).unwrap();
    let rewrite = compiled.physical_rewrite().expect("actual default must select a qualified matrix").clone();
    let explicit = reda::compile::fragment_synth::torch_row::compile_balanced_torch_matrix(&source, 10_000_000).unwrap();
    assert_eq!(reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(rewrite.physical().world()),
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(explicit.physical().world()));
    let world = rewrite.physical().world().clone();
    let to_position = |&(x,y,z): &(i32,i32,i32)| Position::new(x,y,z);
    let inputs: Vec<_> = rewrite.source_input_positions().iter().map(to_position).collect();
    let drivers: Vec<_> = rewrite.source_output_driver_positions().iter().map(to_position).collect();
    let lamps: Vec<_> = rewrite.source_output_positions().iter().map(to_position).collect();
    eprintln!("CERTIFIED_WORLD {}", reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world).as_str());
    let last_row = 4 + 4 * (rows.len() as i32 - 1);
    let end = last_row + 3;
    let fingerprint = reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&world);
    let artifact = std::env::current_dir().unwrap()
        .join(format!("matrix-first-{}.litematic", std::process::id())).display().to_string();
    save(std::path::Path::new(&artifact), &world, "shared-matrix-premise").unwrap();
    let loaded = load(std::path::Path::new(&artifact)).unwrap();
    assert_eq!(fingerprint,
        reda::compile::fragment_synth::benchmark::canonical_world_fingerprint(&loaded));
    let mut sim = Simulator::new(loaded);
    sim.run_until_stable(2000).unwrap();
    let width = source.outputs.len();
    let mut watched: Vec<_> = drivers.iter().chain(&lamps).enumerate()
        .map(|(index, p)| (*p, format!("boundary{index}"), Expected::Prefix(index % width, rows.len() - 1))).collect();
    let last_column = drivers.iter().map(|p| p.x).max().unwrap();
    for (index, &row) in rows.iter().enumerate() {
        watched.push((Position::new(last_column, 3, 4 + 4 * index as i32), format!("row_bad:{row}"), Expected::Bad(row, states - 1)));
    }
    for (bit, &position) in inputs.iter().enumerate() {
        watched.push((position, format!("input:{bit}"), Expected::Bit(bit, false)));
    }
    let implemented = rewrite.implemented();
    let order = implemented.combinational_order().unwrap();
    let evaluations: Vec<_> = (0..states).map(|address| {
        let mut values: HashMap<_,_> = implemented.inputs.iter().enumerate()
            .map(|(bit,name)|(name.clone(),address&(1<<bit)!=0)).collect();
        for &index in &order {
            let gate = &implemented.gates[index];
            let value = gate.kind.evaluate(&gate.inputs.iter().map(|name|values[name]).collect::<Vec<_>>());
            values.insert(gate.output.clone(), value);
        }
        values
    }).collect();
    let mut bindings = BTreeMap::new();
    for gate in &implemented.gates {
        let at = to_position(&rewrite.implemented_observations().gate_output_positions[&gate.output]);
        let label = if let Some((_,label,_)) = watched.iter().find(|(p,_,_)| *p == at) {
            label.clone()
        } else {
            let label = format!("implemented:{}",gate.output);
            watched.push((at,label.clone(),Expected::Gate(gate.output.clone(), evaluations.iter().map(|v|v[&gate.output]).collect())));
            label
        };
        bindings.insert(gate.output.clone(),label);
    }
    let implementation = json!({"inputs":implemented.inputs,"outputs":implemented.outputs,
        "gates":implemented.gates.iter().map(|gate|json!({"output":gate.output,"inputs":gate.inputs,"kind":gate.kind.wire_name()})).collect::<Vec<_>>(),
        "bindings":bindings});
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
        "implementation":implementation,"expanded_certificate_obtained":true,
        "artifact": artifact, "certified": false,"world_fingerprint":fingerprint.as_str(),
        "definitions":definitions,"power_on":power_on,"source_gates":source.gates.iter().map(|gate|
            json!({"inputs":gate.inputs,"output":gate.output,"kind":gate.kind.wire_name()})).collect::<Vec<_>>()}));
    let mut quality = [[0u64; 12]; 2];
    let mut previous = 0usize;
    for from in 0..states { for to in 0..states {
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
    println!("MATRIX_RESULT {}", json!({"quality":quality,"phases":2 * states * states,"certified":false}));
}
