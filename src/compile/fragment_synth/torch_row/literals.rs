//! Truth-recognized direct and inverted literals with distinct public terminals.
use super::*;
use crate::compile::{topology::GateKind, Gate};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Copy, Debug)]
enum Literal {
    Constant(bool),
    Input(usize, bool),
}

pub fn compile(source: &Netlist, limit: u64) -> Result<CertifiedPhysicalRewrite, TorchRowError> {
    if !(4..=5).contains(&source.inputs.len())
        || !(1..=16).contains(&source.outputs.len())
        || source.gates.iter().any(|g| g.kind.is_sequential())
    {
        return Err(TorchRowError::UnsupportedBoundary);
    }
    prove_netlist_rewrite(source, source, &source.inputs, &source.outputs, limit)
        .map_err(|e| TorchRowError::Rewrite(PhysicalRewriteError::Rewrite(e)))?;
    let order = source.combinational_order().unwrap();
    let states = 1usize << source.inputs.len();
    let truths: Vec<Vec<bool>> = (0..states)
        .map(|address| {
            let mut v: HashMap<_, _> = source
                .inputs
                .iter()
                .enumerate()
                .map(|(bit, n)| (n.as_str(), address & (1 << bit) != 0))
                .collect();
            for &g in &order {
                let g = &source.gates[g];
                let args = g.inputs.iter().map(|n| v[n.as_str()]).collect::<Vec<_>>();
                v.insert(g.output.as_str(), g.kind.evaluate(&args));
            }
            source.outputs.iter().map(|n| v[n.as_str()]).collect()
        })
        .collect();
    let mut literals = Vec::new();
    for output in 0..source.outputs.len() {
        let table: Vec<_> = truths.iter().map(|v| v[output]).collect();
        let literal = if table.iter().all(|&v| v == table[0]) {
            Literal::Constant(table[0])
        } else {
            let Some((bit, inverse)) = (0..source.inputs.len())
                .flat_map(|bit| [false, true].map(move |inv| (bit, inv)))
                .find(|&(bit, inv)| {
                    table
                        .iter()
                        .enumerate()
                        .all(|(a, &v)| v == ((a & (1 << bit) != 0) ^ inv))
                })
            else {
                return Err(TorchRowError::UnsupportedBoundary);
            };
            Literal::Input(bit, inverse)
        };
        literals.push(literal);
    }
    let mut world = World::new(
        12 * source.inputs.len() as i32 + 44,
        8,
        4 * source.outputs.len() as i32 + 20,
    );
    let mut gates = Vec::new();
    let mut positions = BTreeMap::new();
    let mut inputs = Vec::new();
    let mut rails: BTreeMap<(usize, bool), Vec<(i32, String)>> = BTreeMap::new();
    for bit in 0..source.inputs.len() {
        let x = 4 + 10 * bit as i32;
        let p = Position::new(x, 2, 2);
        world.set(x, 1, 2, block(BlockKind::Solid));
        let mut lever = block(BlockKind::Lever);
        lever.face = Some(Face::Floor);
        lever.facing = Some(Facing::North);
        world.set(x, 2, 2, lever);
        inputs.push(p);
        for inverse in [false, true] {
            let count = literals
                .iter()
                .filter(|l| matches!(l,Literal::Input(b,i) if *b==bit&&*i==inverse))
                .count();
            if count == 0
                && (inverse
                    || !literals
                        .iter()
                        .any(|l| matches!(l,Literal::Input(b,true) if *b==bit)))
            {
                continue;
            }
            let (column, y, mut previous, start) = if inverse {
                let torch_at = Position::new(x - 1, 1, 4);
                torch(&mut world, torch_at.x, torch_at.y, torch_at.z, Facing::West);
                let name = format!("negative{bit}");
                gates.push(Gate::nor(&name, &[&format!("positive{bit}")]));
                positions.insert(name.clone(), torch_at);
                wire(&mut world, x - 2, 1, 4);
                (x - 2, 1, name, 4)
            } else {
                let at = Position::new(x, 2, 3);
                world.set(x, 1, 3, block(BlockKind::Solid));
                let mut repeater = block(BlockKind::Repeater);
                repeater.facing = Some(Facing::North);
                repeater.delay = 1;
                world.set(x, 2, 3, repeater);
                let name = format!("positive{bit}");
                gates.push(Gate {
                    name: name.clone(),
                    output: name.clone(),
                    kind: GateKind::Buf,
                    inputs: vec![format!("input{bit}")],
                });
                positions.insert(name.clone(), at);
                (x, 2, name, 3)
            };
            rails
                .entry((bit, inverse))
                .or_default()
                .push((start, previous.clone()));
            for z in
                start + 1..=(if inverse { 6 } else { 4 }) + 2 * (count.saturating_sub(1) as i32)
            {
                if z >= 13 && (z - 13) % 14 == 0 {
                    world.set(column, y - 1, z, block(BlockKind::Solid));
                    let mut r = block(BlockKind::Repeater);
                    r.facing = Some(Facing::North);
                    r.delay = 1;
                    world.set(column, y, z, r);
                    let name = format!("rail{bit}_{inverse}_{z}");
                    gates.push(Gate {
                        name: name.clone(),
                        output: name.clone(),
                        kind: GateKind::Buf,
                        inputs: vec![previous],
                    });
                    positions.insert(name.clone(), Position::new(column, y, z));
                    previous = name;
                    rails
                        .entry((bit, inverse))
                        .or_default()
                        .push((z, previous.clone()));
                } else {
                    wire(&mut world, column, y, z);
                }
            }
        }
    }
    let mut counts: BTreeMap<(usize, bool), i32> = BTreeMap::new();
    let mut lamps = Vec::new();
    let mut outputs = Vec::new();
    for (output, literal) in literals.into_iter().enumerate() {
        let (lamp, name) = match literal {
            Literal::Constant(high) => super::player::constant_output(
                &mut world,
                &mut gates,
                &mut positions,
                output,
                high,
                12 * source.inputs.len() as i32 + 16,
                6 + 3 * output as i32,
            ),
            Literal::Input(bit, inverse) => {
                let count = counts.entry((bit, inverse)).or_default();
                let z = (if inverse { 6 } else { 4 }) + 2 * (*count);
                *count += 1;
                let x = 4 + 10 * bit as i32;
                if inverse {
                    wire(&mut world, x - 3, 1, z);
                    (
                        Position::new(x - 4, 1, z),
                        rails[&(bit, inverse)]
                            .iter()
                            .rev()
                            .find(|(stage, _)| *stage < z)
                            .unwrap()
                            .1
                            .clone(),
                    )
                } else {
                    wire(&mut world, x + 1, 2, z);
                    (
                        Position::new(x + 2, 2, z),
                        rails[&(bit, inverse)]
                            .iter()
                            .rev()
                            .find(|(stage, _)| *stage < z)
                            .unwrap()
                            .1
                            .clone(),
                    )
                }
            }
        };
        world.set(lamp.x, lamp.y - 1, lamp.z, block(BlockKind::Solid));
        world.set(lamp.x, lamp.y, lamp.z, block(BlockKind::Lamp));
        lamps.push(lamp);
        outputs.push(name);
    }
    let implemented = Netlist {
        inputs: (0..source.inputs.len())
            .map(|b| format!("input{b}"))
            .collect(),
        outputs,
        gates,
    };
    let size = world.size();
    let library = typed_matrix_library();
    let expected = super::super::benchmark::canonical_world_fingerprint(&world);
    let (implemented, _, candidate) = typed_world_candidate_with_library(
        implemented,
        world,
        inputs,
        lamps,
        positions,
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
        super::super::benchmark::canonical_world_fingerprint(result.physical().world())
    );
    Ok(result)
}
