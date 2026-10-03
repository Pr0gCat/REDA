//! Balanced, refreshed four-input matrix with source and physical bindings.
use super::*;
use crate::compile::{topology::GateKind, Gate};
use std::collections::BTreeMap;
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
    }
    .into();
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

// The initial polarity-balancing stage and the recurring supply stages must
// have identical physical positions and implementation observations.
fn literal_refresh_positions(last_row: i32) -> impl Iterator<Item = i32> {
    std::iter::once(2)
        .chain((14..=last_row).step_by(16))
        .filter(move |z| *z <= last_row)
}
fn matrix(
    words: &[u64],
    rows: &[usize],
    outputs: usize,
    short_output: bool,
) -> (World, Vec<Position>, Vec<Position>, Vec<Position>) {
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
            for z in 1..=last_row {
                wire(&mut world, column, 5, z);
            }
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
        for z in literal_refresh_positions(last_row) {
            for (column, initial_delay) in [(x, 2 + extra), (x + 3, 1 + extra)] {
                let delay = if z == 2 { initial_delay } else { 1 };
                repeater(&mut world, column, 5, z, Facing::North, delay);
            }
        }
    }
    for (index, &address) in rows.iter().enumerate() {
        let word = words[address];
        let z = 4 + 4 * index as i32;
        for x in 2..=last_column {
            wire(&mut world, x, 3, z);
        }
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
        for z in 5..=end {
            wire(&mut world, x, 1, z);
        }
        for z in (15..=end).step_by(16) {
            repeater(&mut world, x, 1, z, Facing::North, 1);
        }
        let delay = (if x < 41 { 2 } else { 1 }) - u8::from(short_output);
        let driver_z = if delay == 0 { end } else { end + 1 };
        if delay > 0 {
            world.set(x, 0, driver_z, block(BlockKind::Solid));
            repeater(&mut world, x, 1, driver_z, Facing::North, delay);
        }
        // A separate output contact avoids sharing the repeater's input
        // landing with its lamp-delivery terminal. It adds no delay stage.
        let needs_contact = world.get(x, 1, driver_z).kind == BlockKind::Repeater;
        let lamp_z = driver_z + 1 + i32::from(needs_contact);
        if needs_contact {
            wire(&mut world, x, 1, driver_z + 1);
        }
        world.set(x, 0, lamp_z, block(BlockKind::Solid));
        world.set(x, 1, lamp_z, block(BlockKind::Lamp));
        drivers.push(Position::new(x, 1, driver_z));
        lamps.push(Position::new(x, 1, lamp_z));
    }
    (world, inputs, drivers, lamps)
}

struct Binding {
    gates: Vec<Gate>,
    positions: BTreeMap<String, Position>,
}
impl Binding {
    fn gate(&mut self, name: String, kind: GateKind, inputs: Vec<String>, at: Position) -> String {
        self.gates.push(Gate {
            name: name.clone(),
            output: name.clone(),
            kind,
            inputs,
        });
        assert!(self.positions.insert(name.clone(), at).is_none());
        name
    }
    fn buffer(&mut self, name: String, input: String, at: Position) -> String {
        self.gate(name, GateKind::Buf, vec![input], at)
    }
    fn merge(&mut self, name: String, inputs: Vec<String>, at: Position) -> String {
        assert!(!inputs.is_empty());
        self.gate(name, GateKind::Or(inputs.len()), inputs, at)
    }
}
pub fn compile(source: &Netlist, limit: u64) -> Result<CertifiedPhysicalRewrite, TorchRowError> {
    if source.inputs.len() != 4 || !(1..=7).contains(&source.outputs.len()) {
        return Err(TorchRowError::UnsupportedBoundary);
    }
    prove_netlist_rewrite(source, source, &source.inputs, &source.outputs, limit)
        .map_err(|e| TorchRowError::Rewrite(PhysicalRewriteError::Rewrite(e)))?;
    let order = source.combinational_order().unwrap();
    let words: Vec<u64> = (0..16)
        .map(|address| {
            let mut values: std::collections::HashMap<_, _> = source
                .inputs
                .iter()
                .enumerate()
                .map(|(bit, name)| (name.clone(), address & (1 << bit) != 0))
                .collect();
            for &index in &order {
                let gate = &source.gates[index];
                values.insert(
                    gate.output.clone(),
                    gate.kind
                        .evaluate(&gate.inputs.iter().map(|n| values[n]).collect::<Vec<_>>()),
                );
            }
            source
                .outputs
                .iter()
                .enumerate()
                .fold(0, |word, (bit, name)| {
                    word | (u64::from(values[name]) << bit)
                })
        })
        .collect();
    let mut rows: Vec<_> = (0..16).filter(|&row| words[row] != 0).collect();
    if rows.is_empty() {
        rows.push(0);
    }
    let (mut world, mut inputs, mut drivers, mut lamps) =
        matrix(&words, &rows, source.outputs.len(), true);
    for p in &mut inputs {
        let lever = world.get(p.x, p.y, p.z).clone();
        world.set(p.x, p.y, p.z, block(BlockKind::RedstoneWire));
        world.set(p.x, p.y - 1, 0, block(BlockKind::Solid));
        world.set(p.x, p.y, 0, lever);
        p.z = 0;
    }
    let mut binding = Binding {
        gates: vec![],
        positions: BTreeMap::new(),
    };
    let last_row = 4 + 4 * (rows.len() as i32 - 1);
    let last_column = 30 + 3 * (source.outputs.len() as i32 - 1);
    let mut rails: BTreeMap<(usize, bool), Vec<(i32, String)>> = BTreeMap::new();
    for bit in 0..4 {
        let x = 3 + 6 * bit as i32;
        let input = format!("input{bit}");
        let inverse = binding.gate(
            format!("inverse{bit}"),
            GateKind::Nor(1),
            vec![input.clone()],
            Position::new(x + 1, 4, 1),
        );
        for (column, inverted, mut previous) in [(x, false, input), (x + 3, true, inverse)] {
            for z in literal_refresh_positions(last_row) {
                previous = binding.buffer(
                    format!("literal{bit}_{inverted}_{z}"),
                    previous,
                    Position::new(column, 5, z),
                );
                rails
                    .entry((bit, inverted))
                    .or_default()
                    .push((z, previous.clone()));
            }
        }
    }
    let mut terms: Vec<Vec<(i32, String)>> = vec![vec![]; source.outputs.len()];
    for (index, &address) in rows.iter().enumerate() {
        let z = 4 + 4 * index as i32;
        let mut mismatches = vec![];
        for bit in 0..4 {
            let inverted = address & (1 << bit) == 0;
            let literal = rails[&(bit, inverted)]
                .iter()
                .rev()
                .find(|(stage, _)| *stage < z)
                .unwrap()
                .1
                .clone();
            mismatches.push(binding.gate(
                format!("row{address}_bad{bit}"),
                GateKind::Nor(1),
                vec![literal],
                Position::new(4 + 6 * bit as i32 + if inverted { 3 } else { 0 }, 4, z),
            ));
        }
        let pair = binding.merge(
            format!("row{address}_pair"),
            mismatches[..2].to_vec(),
            Position::new(12, 3, z),
        );
        let pair = binding.buffer(
            format!("row{address}_buffer14"),
            pair,
            Position::new(14, 3, z),
        );
        let merge = binding.merge(
            format!("row{address}_merge"),
            vec![pair, mismatches[2].clone(), mismatches[3].clone()],
            Position::new(27, 3, z),
        );
        let early = binding.buffer(
            format!("row{address}_buffer28"),
            merge,
            Position::new(28, 3, z),
        );
        let late = if last_column >= 41 {
            Some(binding.buffer(
                format!("row{address}_buffer41"),
                early.clone(),
                Position::new(41, 3, z),
            ))
        } else {
            None
        };
        for (bit, column) in terms.iter_mut().enumerate() {
            if words[address] & (1 << bit) == 0 {
                continue;
            }
            let x = 30 + 3 * bit as i32;
            let control = if x < 41 {
                early.clone()
            } else {
                late.as_ref().unwrap().clone()
            };
            let term = binding.gate(
                format!("row{address}_bit{bit}"),
                GateKind::Nor(1),
                vec![control],
                Position::new(x, 2, z + 1),
            );
            column.push((z + 1, term));
        }
    }
    let end = last_row + 3;
    let mut outputs = vec![];
    for (bit, column) in terms.iter().enumerate() {
        let x = 30 + 3 * bit as i32;
        if column.is_empty() {
            // A real unpowered support drives a constant-high torch; its
            // isolated output powers the support of a constant-low inverter.
            // Replace the empty collector, including every unused repeater.
            for z in 5..world.size().2 {
                world.set(x, 1, z, BlockState::air());
                world.set(x, 0, z, BlockState::air());
            }
            let high_at = Position::new(x, 4, end - 1);
            world.set(x - 1, 4, end - 1, block(BlockKind::Solid));
            torch(&mut world, x, 4, end - 1, Facing::East);
            wire(&mut world, x, 3, end - 1);
            let low_at = Position::new(x, 2, end);
            torch(&mut world, x, 2, end, Facing::South);
            wire(&mut world, x, 1, end);
            world.set(x, 0, end + 1, block(BlockKind::Solid));
            world.set(x, 1, end + 1, block(BlockKind::Lamp));
            let high = binding.gate(
                format!("constant_high{bit}"),
                GateKind::Nor(0),
                vec![],
                high_at,
            );
            let low = binding.gate(
                format!("constant_low{bit}"),
                GateKind::Nor(1),
                vec![high],
                low_at,
            );
            drivers[bit] = low_at;
            lamps[bit] = Position::new(x, 1, end + 1);
            outputs.push(low);
            continue;
        }
        let first = column[0].0;
        // No active driver exists before the first programmed torch.
        for z in 5..first {
            world.set(x, 1, z, BlockState::air());
            world.set(x, 0, z, BlockState::air());
        }
        let mut previous = None;
        let mut lower = first;
        for z in (15..=end).step_by(16).filter(|z| *z >= first) {
            let mut contributions: Vec<_> = column
                .iter()
                .filter(|(at, _)| *at >= lower && *at < z)
                .map(|(_, n)| n.clone())
                .collect();
            if let Some(p) = previous.take() {
                contributions.push(p);
            }
            let merged = binding.merge(
                format!("output{bit}_merge{z}"),
                contributions,
                Position::new(x, 1, z - 1),
            );
            previous = Some(binding.buffer(
                format!("output{bit}_buffer{z}"),
                merged,
                Position::new(x, 1, z),
            ));
            lower = z + 1;
        }
        let mut contributions: Vec<_> = column
            .iter()
            .filter(|(at, _)| *at >= lower)
            .map(|(_, n)| n.clone())
            .collect();
        if let Some(p) = previous {
            contributions.push(p);
        }
        let output = if world.get(x, 1, end).kind == BlockKind::Repeater {
            assert_eq!(contributions.len(), 1);
            contributions[0].clone()
        } else {
            binding.merge(
                format!("output{bit}_final"),
                contributions,
                Position::new(x, 1, end),
            )
        };
        let output = if drivers[bit].z > end {
            binding.buffer(format!("output{bit}_driver"), output, drivers[bit])
        } else {
            output
        };
        outputs.push(output);
    }
    let implemented = Netlist {
        inputs: (0..4).map(|bit| format!("input{bit}")).collect(),
        outputs,
        gates: binding.gates,
    };
    let library = typed_matrix_library();
    let size = world.size();
    let expected = super::super::benchmark::canonical_world_fingerprint(&world);
    let (implemented, _, candidate) = typed_world_candidate_with_library(
        implemented,
        world,
        inputs,
        lamps,
        binding.positions,
        true,
        &library,
    );
    let result = super::super::rewrite::realise_physical_rewrite_with_size(
        source,
        &implemented,
        &candidate,
        &library,
        &implemented.inputs,
        &implemented.outputs,
        limit,
        Some(size),
    )
    .map_err(TorchRowError::Rewrite)?;
    assert_eq!(
        expected,
        super::super::benchmark::canonical_world_fingerprint(result.physical().world()),
        "certified emission must preserve the entire prototype world"
    );
    Ok(result)
}
