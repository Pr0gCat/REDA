//! Compile SystemVerilog fixtures and check every combinational output
//! against the logical evaluator, reporting time, delay and density.
//!
//! ```text
//! cargo run --release --example construct_probe -- [--dewey | --dewey-place] <file.sv> <top> [...]
//! ```
//!
//! `--dewey` places and routes by the dewey baseline instead of the
//! constructive generator; `--dewey-place` places by dewey and routes by the
//! constructive router. Delay is the most game ticks any input row took
//! to settle after the previous one; density is blocks over the footprint.

use std::time::Instant;

use reda::compile::construct::Method;
use reda::compile::{
    compile_hierarchical, compile_with, compile_with_places, lowering::lower_optimised_with_provenance, PortPlace, Side,
};
use reda::frontend::evaluate::Evaluator;
use reda::frontend::{compile_systemverilog, CompileOptions, SourceInput};
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::BlockKind;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let method = match args.first().map(String::as_str) {
        Some("--dewey") => Method::Dewey,
        Some("--dewey-place") => Method::DeweyPlace,
        _ => Method::Constructive,
    };
    if method != Method::Constructive {
        args.remove(0);
    }
    // `--hier <grain>`: generate along the module tree.
    let grain = if args.first().is_some_and(|arg| arg == "--hier") {
        args.remove(0);
        Some(args.remove(0).parse::<usize>().expect("a grain"))
    } else {
        None
    };
    for pair in args.chunks(2) {
        let (path, top) = (&pair[0], &pair[1]);
        let text = std::fs::read_to_string(path).expect("readable source");
        let artifact = compile_systemverilog(
            &[SourceInput {
                name: top,
                text: &text,
            }],
            &CompileOptions::new(top),
        )
        .expect("frontend");
        let provenanced = lower_optimised_with_provenance(&artifact.netlist).expect("lowering");
        let lowered = provenanced.netlist.clone();
        let sequential = lowered.gates.iter().any(|gate| gate.kind.is_sequential());
        let started = Instant::now();
        let generated = match grain {
            // `PROBE_EDGES=1`: every input on the west edge, every output
            // on the east, as a block's ports are.
            None if std::env::var_os("PROBE_EDGES").is_some() => {
                let mut places = std::collections::BTreeMap::new();
                for name in &lowered.inputs {
                    places.insert(name.clone(), PortPlace::Edge(Side::West));
                }
                for name in &lowered.outputs {
                    places.insert(name.clone(), PortPlace::Edge(Side::East));
                }
                compile_with_places(&lowered, &places)
            }
            None => compile_with(&lowered, method),
            Some(grain) => {
                let debug = &artifact.debug;
                let owner: Vec<usize> = provenanced
                    .provenance
                    .iter()
                    .map(|&source| debug.instance_of_gate(&debug.gates[source]).map_or(0, |instance| instance.0 as usize))
                    .collect();
                let parents: Vec<Option<usize>> =
                    debug.instances.iter().map(|info| info.parent.map(|parent| parent.0 as usize)).collect();
                compile_hierarchical(&lowered, &owner, &parents, grain)
            }
        };
        let compiled = match generated {
            Ok(compiled) => compiled,
            Err(error) => {
                println!(
                    "{top}: gates={} FAIL {:?}ms {error}",
                    lowered.gates.len(),
                    started.elapsed().as_millis()
                );
                continue;
            }
        };
        let millis = started.elapsed().as_millis();
        let size = compiled.world.size();
        // `PROBE_LEVELS=1`: how many dust and repeaters each level holds.
        if std::env::var_os("PROBE_LEVELS").is_some() {
            for y in 0..size.1 {
                let mut wires = 0;
                for x in 0..size.0 {
                    for z in 0..size.2 {
                        if matches!(compiled.world.get(x, y, z).kind, BlockKind::RedstoneWire | BlockKind::Repeater) {
                            wires += 1;
                        }
                    }
                }
                eprintln!("  level {y}: {wires} wire cells");
            }
        }
        let mut blocks = 0usize;
        for y in 0..size.1 {
            for z in 0..size.2 {
                for x in 0..size.0 {
                    if compiled.world.get(x, y, z).kind != BlockKind::Air {
                        blocks += 1;
                    }
                }
            }
        }
        let footprint = size.0 as usize * size.2 as usize;
        let mut verdict = String::from("not checked (sequential)");
        let mut delay = None;
        if !sequential && artifact.netlist.inputs.len() <= 64 {
            let logical = Evaluator::new(&artifact.netlist).expect("evaluator");
            let mut sim = Simulator::new(compiled.bench.clone());
            sim.run_until_stable(4000).expect("settles");
            let mut wrong = 0;
            let mut slowest = 0;
            let mut slowest_rows = (0, 0);
            let mut rows = rows(artifact.netlist.inputs.len());
            // `PROBE_ROWS=<hex>,<hex>,...`: rows to run after the sampled
            // ones, in order -- a known slow transition, say.
            if let Ok(extra) = std::env::var("PROBE_ROWS") {
                rows.extend(extra.split(',').map(|row| u64::from_str_radix(row, 16).expect("a hex row")));
            }
            let mut previous = 0;
            for &row in &rows {
                let inputs = bits(&artifact.netlist.inputs, row);
                for (name, value) in &inputs {
                    let at = compiled.input_positions[name];
                    let mut state = sim.world().get(at.0, at.1, at.2).clone();
                    state.lit = *value;
                    sim.world_mut().set(at.0, at.1, at.2, state);
                }
                let ticks = sim.run_until_stable(4000).expect("settles");
                if ticks > slowest {
                    (slowest, slowest_rows) = (ticks, (previous, row));
                }
                previous = row;
                let expected = logical.evaluate(&inputs).expect("evaluates");
                for (_, signal) in artifact.output_map() {
                    let at = compiled.output_positions[&signal];
                    if sim.world().get(at.0, at.1, at.2).lit != expected[&signal] {
                        if wrong < 3 {
                            eprintln!(
                                "  wrong: row {row:#b} output {signal} at {at:?}, expected {}",
                                expected[&signal]
                            );
                            // The lowered gates' own values, to find the first wrong one.
                            let mut value: std::collections::HashMap<&str, bool> =
                                inputs.iter().map(|(name, v)| (name.as_str(), *v)).collect();
                            for &index in &lowered.combinational_order().unwrap() {
                                let gate = &lowered.gates[index];
                                let operands: Vec<bool> =
                                    gate.inputs.iter().map(|i| value[i.as_str()]).collect();
                                let v = gate.kind.evaluate(&operands);
                                value.insert(&gate.output, v);
                                if let Some(&p) = compiled.gate_output_positions.get(&gate.output) {
                                    let got = sim.world().get(p.0, p.1, p.2);
                                    if high(got) != v {
                                        eprintln!("    gate {} ({:?}) reads {:?} expected {v} got {:?} at {p:?}", gate.output, gate.kind, gate.inputs, high(got));
                                    }
                                    let up: i32 = std::env::var("PROBE_DUMP")
                                        .ok()
                                        .and_then(|up| up.parse().ok())
                                        .unwrap_or(4);
                                    if high(got) != v && std::env::var_os("PROBE_DUMP").is_some() {
                                        for y in (p.1 - 1..=p.1 + up).rev() {
                                            eprintln!("    y={y}");
                                            for z in p.2 - 8..=p.2 + 8 {
                                                let mut line = String::new();
                                                for x in p.0 - 10..=p.0 + 10 {
                                                    let b = sim.world().get(x, y, z);
                                                    let c = match b.kind {
                                                        BlockKind::Air => " .".to_string(),
                                                        BlockKind::RedstoneWire => format!("{:2}", b.power),
                                                        BlockKind::Repeater => format!(" {}", match b.facing { Some(reda::redstone::world::block::Facing::North) => '^', Some(reda::redstone::world::block::Facing::South) => 'v', Some(reda::redstone::world::block::Facing::East) => '>', _ => '<' }),
                                                        BlockKind::WallTorch => if b.lit { " T".into() } else { " t".into() },
                                                        _ => " #".to_string(),
                                                    };
                                                    line.push_str(&c);
                                                }
                                                eprintln!("    z={z:4} {line}");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        wrong += 1;
                    }
                }
            }
            delay = Some(slowest);
            if let Ok(at) = std::env::var("PROBE_TRACE") {
                let at: Vec<i32> = at.split(',').map(|v| v.parse().unwrap()).collect();
                trace(sim.world(), (at[0], at[1], at[2]));
            }
            if std::env::var_os("PROBE_TIMING").is_some() {
                timing(&compiled, &artifact.netlist.inputs, slowest_rows, slowest);
            }
            let every = rows.len() as u64 == 1u64 << artifact.netlist.inputs.len().min(63);
            verdict = if wrong == 0 && every {
                format!("truth table OK ({} rows)", rows.len())
            } else if wrong == 0 {
                format!("sampled OK ({} rows; delay is a lower bound)", rows.len())
            } else {
                format!("WRONG in {wrong} output checks")
            };
        }
        let delay = delay.map_or_else(|| "-".to_string(), |ticks| format!("{ticks}gt"));
        println!(
            "{top}: gates={} {}ms delay={delay} world={size:?} blocks={blocks} footprint={footprint} density={:.3} {verdict}",
            lowered.gates.len(),
            millis,
            blocks as f64 / footprint as f64
        );
    }
}

/// The game tick each gate output last changed at on the slowest row, in
/// order, with the distance from the gate before it: where the delay goes.
fn timing(
    compiled: &reda::compile::CompiledCircuit,
    inputs: &[String],
    (from, row): (u64, u64),
    slowest: u64,
) {
    let mut sim = Simulator::new(compiled.bench.clone());
    let set = |sim: &mut Simulator, row: u64| {
        for (name, value) in bits(inputs, row) {
            let at = compiled.input_positions[&name];
            let mut state = sim.world().get(at.0, at.1, at.2).clone();
            state.lit = value;
            sim.world_mut().set(at.0, at.1, at.2, state);
        }
    };
    set(&mut sim, from);
    sim.run_until_stable(4000).expect("settles");
    set(&mut sim, row);
    // Every gate's output and every output lamp.
    let probes: std::collections::BTreeMap<String, (i32, i32, i32)> = compiled
        .gate_output_positions
        .iter()
        .map(|(name, &at)| (name.clone(), at))
        .chain(compiled.output_positions.iter().map(|(name, &at)| (format!("lamp:{name}"), at)))
        .collect();
    let mut last: std::collections::BTreeMap<&String, (u64, bool)> =
        std::collections::BTreeMap::new();
    let read = |sim: &Simulator, at: &(i32, i32, i32)| high(sim.world().get(at.0, at.1, at.2));
    for (name, at) in &probes {
        last.insert(name, (0, read(&sim, at)));
    }
    for tick in 1..=slowest + 2 {
        sim.step();
        for (name, at) in &probes {
            let now = read(&sim, at);
            let entry = last.get_mut(name).unwrap();
            if now != entry.1 {
                *entry = (tick, now);
            }
        }
    }
    let mut changed: Vec<(u64, &String)> = last
        .iter()
        .filter(|(_, (tick, _))| *tick > 0)
        .map(|(name, (tick, _))| (*tick, *name))
        .collect();
    changed.sort();
    eprintln!("  timing of row {row:#b} ({slowest}gt):");
    let mut previous: Option<(i32, i32, i32)> = None;
    for (tick, name) in changed {
        let at = probes[name];
        let gap = previous.map_or(0, |p| (p.0 - at.0).abs() + (p.2 - at.2).abs());
        eprintln!("    {tick:4}gt {name} at {at:?} manhattan-from-previous {gap}");
        previous = Some(at);
    }
}

/// A probe's value: dust by its power, anything else by whether it is lit.
fn high(block: &reda::redstone::world::block::BlockState) -> bool {
    if block.kind == BlockKind::RedstoneWire {
        block.power > 0
    } else {
        block.lit
    }
}

/// Walk back from the dust at `from` along what really connects in the
/// world -- dust links and repeaters entered from their output side -- and
/// print the farthest cells reached: where a dead input's wire breaks.
fn trace(world: &reda::redstone::world::storage::World, from: (i32, i32, i32)) {
    use reda::redstone::simulator::connectivity::dust_connections;
    use reda::redstone::simulator::position::Position;
    use reda::redstone::world::block::Facing;
    let start = Position::new(from.0, from.1, from.2);
    let mut seen = std::collections::HashMap::from([(start, None::<Position>)]);
    let mut queue = std::collections::VecDeque::from([start]);
    let mut last = start;
    while let Some(here) = queue.pop_front() {
        last = here;
        let block = world.get(here.x, here.y, here.z);
        let mut next = Vec::new();
        match block.kind {
            BlockKind::RedstoneWire => {
                for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                    next.extend(dust_connections(world, here, direction).iter());
                    let side = here.offset(direction);
                    let neighbour = world.get(side.x, side.y, side.z);
                    // A repeater feeding this dust faces away from it.
                    if neighbour.kind == BlockKind::Repeater
                        && neighbour.facing.map(|f| side.offset(f)) != Some(here)
                    {
                        next.push(side);
                    }
                }
                for direction in [
                    Facing::North,
                    Facing::South,
                    Facing::East,
                    Facing::West,
                    Facing::Up,
                    Facing::Down,
                ] {
                    let side = here.offset(direction);
                    let neighbour = world.get(side.x, side.y, side.z);
                    if !matches!(
                        neighbour.kind,
                        BlockKind::Air | BlockKind::RedstoneWire | BlockKind::Repeater
                    ) {
                        eprintln!(
                            "  trace: dust {here:?} touches {:?} {:?} at {side:?} lit={}",
                            neighbour.kind, neighbour.facing, neighbour.lit
                        );
                    }
                }
            }
            BlockKind::Repeater => next.push(here.offset(block.facing.unwrap())),
            other => eprintln!(
                "  trace reached {other:?} at {here:?} power={} lit={}",
                block.power, block.lit
            ),
        }
        for cell in next {
            if let std::collections::hash_map::Entry::Vacant(entry) = seen.entry(cell) {
                entry.insert(Some(here));
                queue.push_back(cell);
            }
        }
    }
    eprintln!(
        "  trace from {from:?}: {} cells; path to the farthest:",
        seen.len()
    );
    eprintln!("  around the far end {last:?}:");
    for y in last.y - 2..=last.y + 2 {
        for z in last.z - 1..=last.z + 3 {
            for x in last.x - 1..=last.x + 1 {
                let block = world.get(x, y, z);
                if block.kind != BlockKind::Air {
                    eprintln!(
                        "    {:?} {:?} {:?} power={} lit={}",
                        (x, y, z),
                        block.kind,
                        block.facing,
                        block.power,
                        block.lit
                    );
                }
            }
        }
    }
    let mut at = Some(last);
    let mut path = Vec::new();
    while let Some(cell) = at {
        path.push(cell);
        at = seen[&cell];
    }
    for cell in path.iter().take(40).rev() {
        let block = world.get(cell.x, cell.y, cell.z);
        eprintln!(
            "    {:?} {:?} {:?} power={}",
            (cell.x, cell.y, cell.z),
            block.kind,
            block.facing,
            block.power
        );
    }
}

/// Every input row up to twelve inputs; beyond that, all-low, all-high and
/// a fixed pseudo-random sample, each row walked from the one before.
fn rows(inputs: usize) -> Vec<u64> {
    if inputs <= 12 {
        return (0..1u64 << inputs).collect();
    }
    let mask = if inputs >= 64 {
        u64::MAX
    } else {
        (1u64 << inputs) - 1
    };
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut rows = vec![0, mask];
    rows.extend((0..510).map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state & mask
    }));
    rows
}

/// One row's input values: input `i` takes bit `i`.
fn bits(inputs: &[String], row: u64) -> std::collections::BTreeMap<String, bool> {
    inputs
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), (row >> index) & 1 == 1))
        .collect()
}
