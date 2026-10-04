//! Compose independently certified bounded leaves whose actual input supports
//! are disjoint. Shared-support groups remain one leaf and fail closed if large.
use std::collections::{BTreeMap, BTreeSet};

use super::{
    rewrite::{CertifiedPhysicalRewrite, PhysicalRewriteError},
    torch_row,
};
use crate::compile::{equivalence::prove_netlist_rewrite, Netlist};
use crate::redstone::{
    simulator::position::Position,
    world::{block::BlockKind, storage::World},
};

#[derive(Debug)]
pub enum SupportCompositionError {
    UnsupportedBoundary,
    Leaf(torch_row::TorchRowError),
    Original(String),
    UnsupportedLeaf { inputs: usize, outputs: usize },
    Rewrite(PhysicalRewriteError),
}

/// Structural output-cone decomposition; names only identify graph edges.
fn groups(source: &Netlist) -> Option<Vec<(Vec<usize>, BTreeSet<String>, BTreeSet<usize>)>> {
    let order = source.combinational_order()?;
    let mut supports: BTreeMap<String, BTreeSet<String>> = source
        .inputs
        .iter()
        .map(|s| (s.clone(), BTreeSet::from([s.clone()])))
        .collect();
    let mut cones: BTreeMap<String, BTreeSet<usize>> = source
        .inputs
        .iter()
        .map(|s| (s.clone(), BTreeSet::new()))
        .collect();
    for index in order {
        let gate = &source.gates[index];
        let mut support = BTreeSet::new();
        let mut cone = BTreeSet::from([index]);
        for input in &gate.inputs {
            support.extend(supports.get(input)?.iter().cloned());
            cone.extend(cones.get(input)?.iter().copied());
        }
        supports.insert(gate.output.clone(), support);
        cones.insert(gate.output.clone(), cone);
    }
    let mut groups: Vec<(Vec<usize>, BTreeSet<String>, BTreeSet<usize>)> = vec![];
    for (ordinal, output) in source.outputs.iter().enumerate() {
        let mut support = supports.get(output)?.clone();
        let mut cone = cones.get(output)?.clone();
        let mut outputs = vec![ordinal];
        let mut index = 0;
        while index < groups.len() {
            if !groups[index].1.is_disjoint(&support) {
                let (o, s, c) = groups.remove(index);
                outputs.extend(o);
                support.extend(s);
                cone.extend(c);
                index = 0;
            } else {
                index += 1;
            }
        }
        groups.push((outputs, support, cone));
    }
    let mut constants = vec![];
    let mut constant_cones = BTreeSet::new();
    groups.retain(|g| {
        if g.1.is_empty() {
            constants.extend(g.0.iter().copied());
            constant_cones.extend(g.2.iter().copied());
            false
        } else {
            true
        }
    });
    if !constants.is_empty() {
        let first = groups
            .iter_mut()
            .min_by_key(|g| (g.0.len(), *g.0.iter().min().unwrap()))?;
        first.0.extend(constants);
        first.2.extend(constant_cones);
    }
    for group in &mut groups {
        group.0.sort_unstable();
    }
    groups.sort_by_key(|g| g.0[0]);
    Some(groups)
}

/// Cheap eligibility keeps unsupported cases on their original proposal stream.
pub(crate) fn eligible(source: &Netlist) -> bool {
    let Some(groups) = groups(source) else {
        return false;
    };
    groups.len() >= 2
        && groups
            .iter()
            .flat_map(|g| g.1.iter())
            .collect::<BTreeSet<_>>()
            .len()
            == source.inputs.len()
        && groups.iter().all(|g| {
            (g.1.len() == 2 && g.0.len() == 3)
                || ((4..=5).contains(&g.1.len()) && (1..=16).contains(&g.0.len()))
        })
}

pub fn compile_disjoint_support(
    source: &Netlist,
    proof_work_limit: u64,
) -> Result<CertifiedPhysicalRewrite, SupportCompositionError> {
    use SupportCompositionError::*;
    prove_netlist_rewrite(
        source,
        source,
        &source.inputs,
        &source.outputs,
        proof_work_limit,
    )
    .map_err(|e| Rewrite(PhysicalRewriteError::Rewrite(e)))?;
    let groups = groups(source).ok_or(UnsupportedBoundary)?;
    if groups.len() < 2 {
        return Err(UnsupportedBoundary);
    }
    let covered: BTreeSet<_> = groups.iter().flat_map(|g| g.1.iter().cloned()).collect();
    if covered.len() != source.inputs.len() {
        return Err(UnsupportedBoundary);
    }
    let mut leaves = vec![];
    for (outputs, support, cone) in &groups {
        let leaf = Netlist {
            inputs: source
                .inputs
                .iter()
                .filter(|s| support.contains(*s))
                .cloned()
                .collect(),
            outputs: outputs.iter().map(|&i| source.outputs[i].clone()).collect(),
            gates: source
                .gates
                .iter()
                .enumerate()
                .filter(|(i, _)| cone.contains(i))
                .map(|(_, g)| g.clone())
                .collect(),
        };
        let rewrite = compile_original_leaf(&leaf, proof_work_limit)?;
        leaves.push(rewrite);
    }
    // Eight air cells separate occupied extents: no coupling or parent routes
    // can be introduced by translation of independent supports.
    let width = leaves
        .iter()
        .try_fold(0i32, |width, leaf| {
            width
                .checked_add(leaf.physical().world().size().0)?
                .checked_add(8)
        })
        .ok_or(UnsupportedBoundary)?;
    let height = leaves
        .iter()
        .map(|l| l.physical().world().size().1)
        .max()
        .unwrap();
    let depth = leaves
        .iter()
        .map(|l| l.physical().world().size().2)
        .max()
        .unwrap();
    let mut world = World::new(width, height, depth);
    let mut implemented = Netlist {
        inputs: source.inputs.clone(),
        outputs: vec![String::new(); source.outputs.len()],
        gates: vec![],
    };
    let mut inputs = BTreeMap::new();
    let mut lamps = vec![Position::new(0, 0, 0); source.outputs.len()];
    let mut positions = BTreeMap::new();
    let mut offset = 0;
    for (ordinal, ((outputs, support, _), leaf)) in groups.iter().zip(&leaves).enumerate() {
        let child = leaf.implemented();
        let source_inputs: Vec<_> = source
            .inputs
            .iter()
            .filter(|s| support.contains(*s))
            .collect();
        let rename = |name: &String| -> String {
            child.inputs.iter().position(|s| s == name).map_or_else(
                || format!("component{ordinal}_{name}"),
                |i| source_inputs[i].clone(),
            )
        };
        let shifted = |p: (i32, i32, i32)| Position::new(p.0 + offset, p.1, p.2);
        let old = leaf.physical().world();
        let (x, y, z) = old.size();
        for iy in 0..y {
            for iz in 0..z {
                for ix in 0..x {
                    let state = old.get(ix, iy, iz);
                    if state.kind != BlockKind::Air {
                        world.set(ix + offset, iy, iz, state.clone());
                    }
                }
            }
        }
        for (name, at) in child.inputs.iter().zip(leaf.source_input_positions()) {
            inputs.insert(rename(name), shifted(*at));
        }
        for (local, &root) in outputs.iter().enumerate() {
            implemented.outputs[root] = rename(&child.outputs[local]);
            lamps[root] = shifted(leaf.source_output_positions()[local]);
        }
        for gate in &child.gates {
            let mut gate = gate.clone();
            gate.name = format!("component{ordinal}_{}", gate.name);
            gate.inputs = gate.inputs.iter().map(&rename).collect();
            let original_output = gate.output.clone();
            gate.output = rename(&original_output);
            positions.insert(
                gate.output.clone(),
                shifted(leaf.implemented_observations().gate_output_positions[&original_output]),
            );
            implemented.gates.push(gate);
        }
        offset += x + 8;
    }
    let library = torch_row::typed_matrix_library();
    let size = world.size();
    let expected_world = super::benchmark::canonical_world_fingerprint(&world);
    let input_positions = implemented.inputs.iter().map(|s| inputs[s]).collect();
    let (implemented, _, candidate) = torch_row::typed_world_candidate_with_library(
        implemented,
        world,
        input_positions,
        lamps,
        positions,
        true,
        &library,
    );
    let result = super::rewrite::realise_physical_rewrite_with_size(
        source,
        &implemented,
        &candidate,
        &library,
        &implemented.inputs,
        &implemented.outputs,
        proof_work_limit,
        Some(size),
    )
    .map_err(Rewrite)?;
    assert_eq!(
        expected_world,
        super::benchmark::canonical_world_fingerprint(result.physical().world()),
        "sealing must preserve all composed leaf cells"
    );
    Ok(result)
}

/// Isolated experiment only: preserve an existing original leaf exactly.
pub fn compile_original_leaf(leaf: &Netlist, proof_work_limit: u64) -> Result<CertifiedPhysicalRewrite, SupportCompositionError> {
        // Preserve the existing original physical strategy. Only translation is
        // introduced here; no unconditional truth-table decoding is performed.
        let compiled = crate::compile::compile(&leaf).map_err(|e| SupportCompositionError::Original(format!("{e:?}")))?;
        let gate_positions = compiled.gate_output_positions.iter().map(|(name, &(x,y,z))| (name.clone(), Position::new(x,y,z))).collect();
        let lamp_positions = compiled.output_occurrences().iter().map(|(_, (x,y,z))| Position::new(*x,*y,*z)).collect();
        let input_positions = leaf.inputs.iter().map(|name| { let (x,y,z)=compiled.input_positions[name]; Position::new(x,y,z) }).collect();
        let original_fingerprint = super::benchmark::canonical_world_fingerprint(&compiled.world);
        let library = torch_row::typed_matrix_library();
        let size = compiled.world.size();
        let (implemented, _, candidate) = torch_row::typed_world_candidate_with_library(
            leaf.clone(), compiled.world, input_positions, lamp_positions,
            gate_positions, true, &library,
        );
        let rewrite = super::rewrite::realise_physical_rewrite_with_size(
            &leaf, &implemented, &candidate, &library,
            &implemented.inputs, &implemented.outputs, proof_work_limit, Some(size),
        ).map_err(SupportCompositionError::Rewrite)?;
        assert_eq!(original_fingerprint, super::benchmark::canonical_world_fingerprint(rewrite.physical().world()));
        Ok(rewrite)
}

/// Exact cone sources and root public-output ordinals used by composition.
pub fn source_leaf_groups(source: &Netlist) -> Vec<(Netlist, Vec<usize>)> {
    groups(source).unwrap().into_iter().map(|(outputs,support,cone)| {
        let leaf=Netlist {
            inputs: source.inputs.iter().filter(|s|support.contains(*s)).cloned().collect(),
            outputs: outputs.iter().map(|&i|source.outputs[i].clone()).collect(),
            gates: source.gates.iter().enumerate().filter(|(i,_)|cone.contains(i)).map(|(_,g)|g.clone()).collect(),
        };
        (leaf, outputs)
    }).collect()
}
