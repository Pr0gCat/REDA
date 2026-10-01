//! Bounded two-input/three-output torch-row synthesis. Recognition evaluates
//! functions, never module, case or gate names. Physical certification remains
//! mandatory and this entry does not choose a winner by size.
use super::rewrite::{realise_physical_rewrite, CertifiedPhysicalRewrite, PhysicalRewriteError};
use crate::compile::{equivalence::prove_netlist_rewrite, Netlist};
use crate::frontend::cover;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::{
    block::{BlockKind, BlockState, Face, Facing},
    storage::World,
};

mod player;

#[derive(Debug)]
pub enum TorchRowError {
    UnsupportedBoundary,
    Rewrite(PhysicalRewriteError),
}

/// Generate and certify a complete ROM-like function using its truth table.
/// This currently qualified family has exactly two inputs and three outputs;
/// stateful/invalid functions and proof-budget exhaustion fail closed.
/// The returned observations describe the implementation, not source gates.
pub fn compile_torch_row(
    source: &Netlist,
    proof_work_limit: u64,
) -> Result<CertifiedPhysicalRewrite, TorchRowError> {
    compile_matrix(source, proof_work_limit, false)
}

/// Experimental player geometry; all existing equivalence and physical checks apply.
/// This entry is never selected by the automatic compiler.
pub fn compile_player_torch_row(
    source: &Netlist,
    proof_work_limit: u64,
) -> Result<CertifiedPhysicalRewrite, TorchRowError> {
    compile_matrix(source, proof_work_limit, true)
}

fn compile_matrix(
    source: &Netlist,
    proof_work_limit: u64,
    player: bool,
) -> Result<CertifiedPhysicalRewrite, TorchRowError> {
    if source.inputs.len() != 2 || source.outputs.len() != 3 {
        return Err(TorchRowError::UnsupportedBoundary);
    }
    // Reuse the existing validator before any evaluation or graph indexing.
    prove_netlist_rewrite(
        source,
        source,
        &source.inputs,
        &source.outputs,
        proof_work_limit,
    )
    .map_err(|e| TorchRowError::Rewrite(PhysicalRewriteError::Rewrite(e)))?;
    let order = source
        .combinational_order()
        .expect("validated combinational graph");
    let words: [u8; 4] = std::array::from_fn(|address| {
        let mut values: std::collections::HashMap<String, bool> = source
            .inputs
            .iter()
            .enumerate()
            .map(|(bit, name)| (name.clone(), address & (1 << bit) != 0))
            .collect();
        for &index in &order {
            let gate = &source.gates[index];
            let inputs: Vec<_> = gate.inputs.iter().map(|name| values[name]).collect();
            values.insert(gate.output.clone(), gate.kind.evaluate(&inputs));
        }
        source
            .outputs
            .iter()
            .enumerate()
            .fold(0, |word, (bit, name)| {
                word | (u8::from(values[name]) << bit)
            })
    });
    let (implemented, _, candidate) = if player {
        player::candidate(words)
    } else {
        typed_matrix_candidate(words)
    };
    realise_physical_rewrite(
        source,
        &implemented,
        &candidate,
        &typed_matrix_library(),
        &implemented.inputs,
        &implemented.outputs,
        proof_work_limit,
    )
    .map_err(TorchRowError::Rewrite)
}

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

fn matrix_logic_binding(
    words: [u8; 4],
) -> (
    crate::compile::Netlist,
    std::collections::BTreeMap<String, Position>,
) {
    use crate::compile::{Gate, Netlist};
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

fn typed_matrix_library() -> crate::compile::topology::Library {
    use crate::compile::topology::{
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

fn bind_matrix_refresh_stages(
    netlist: &mut crate::compile::Netlist,
    positions: &mut std::collections::BTreeMap<String, Position>,
    world: &mut World,
) {
    use crate::compile::{topology::GateKind, Gate};
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
    crate::compile::Netlist,
    World,
    crate::compile::fragment_synth::candidate::ExpandedPhysicalCandidate,
) {
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
    typed_world_candidate(netlist, world, inputs, lamps, positions, false)
}

fn typed_world_candidate(
    netlist: Netlist,
    world: World,
    inputs: Vec<Position>,
    lamps: Vec<Position>,
    positions: std::collections::BTreeMap<String, Position>,
    player: bool,
) -> (Netlist, World, super::candidate::ExpandedPhysicalCandidate) {
    use crate::compile::fragment_synth::{
        candidate::*, identity::*, instance_graph::*, topology::*,
    };
    use crate::compile::{
        geometry::{Anchor, CellFacing},
        planner::PortPlacements,
    };
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    let anchor = |p: Position| Anchor {
        x: p.x,
        y: p.y,
        z: p.z,
    };
    let graph = InstanceGraph::one_to_one(&netlist, &typed_matrix_library()).unwrap();
    let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
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
        let root = if player && world.get(p.x, p.y, p.z).kind == BlockKind::Lever {
            let direct = if let PhysicalEndpointId::PrimitiveOutput(id) = landing {
                candidate.placements[&id]
                    .blocks
                    .iter()
                    .find(|b| b.state.kind == BlockKind::Repeater)
                    .map(|b| Position::new(b.at.x, b.at.y, b.at.z))
                    .filter(|r| r.x.abs_diff(p.x) + r.y.abs_diff(p.y) + r.z.abs_diff(p.z) == 1)
            } else {
                None
            };
            direct.unwrap_or_else(|| {
                [Facing::North, Facing::South, Facing::East, Facing::West]
                    .into_iter()
                    .map(|f| p.offset(f))
                    .find(|n| world.get(n.x, n.y, n.z).kind == BlockKind::RedstoneWire)
                    .unwrap()
            })
        } else if world.get(p.x, p.y, p.z).kind == BlockKind::Repeater {
            if player {
                p
            } else {
                p.offset(world.get(p.x, p.y, p.z).facing.unwrap().opposite())
            }
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
            PhysicalEndpointId::Junction(_) => {
                if player && world.get(root.x, root.y, root.z).kind == BlockKind::Repeater {
                    let forward =
                        root.offset(world.get(root.x, root.y, root.z).facing.unwrap().opposite());
                    if world.get(forward.x, forward.y, forward.z).kind == BlockKind::RedstoneWire {
                        forward
                    } else {
                        forward.down()
                    }
                } else {
                    root
                }
            }
            PhysicalEndpointId::DeclaredOutput(_) => {
                let at = candidate.observations[&observation(landing)].site.at;
                if player {
                    [Facing::North, Facing::West, Facing::East, Facing::South]
                        .into_iter()
                        .map(|f| Position::new(at.x, at.y, at.z).offset(f))
                        .find(|n| world.get(n.x, n.y, n.z).kind == BlockKind::RedstoneWire)
                        .unwrap()
                } else {
                    Position::new(at.x, at.y, at.z - 1)
                }
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
                let mut next: Vec<_> = crate::redstone::simulator::connectivity::dust_connections(
                    &world, p, direction,
                )
                .iter()
                .collect();
                let n = p.offset(direction);
                let nstate = world.get(n.x, n.y, n.z);
                if nstate.kind == BlockKind::Repeater && nstate.facing == Some(direction.opposite())
                {
                    next.push(n);
                }
                if player && state.kind == BlockKind::RedstoneWire {
                    let floor = p.down();
                    let r = floor.offset(direction);
                    let rs = world.get(r.x, r.y, r.z);
                    if world.get(floor.x, floor.y, floor.z).kind == BlockKind::Solid
                        && rs.kind == BlockKind::Repeater
                        && rs.facing == Some(direction.opposite())
                    {
                        next.push(r);
                    }
                }
                if player
                    && state.kind == BlockKind::Repeater
                    && nstate.kind == BlockKind::Solid
                    && world.get(n.x, n.y - 1, n.z).kind == BlockKind::RedstoneWire
                {
                    next.push(n.down());
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
        if player && path.len() >= 2 {
            let mut contracts = Vec::new();
            if let PhysicalEndpointId::PrimitiveOutput(id) = source {
                if world.get(root.x, root.y, root.z).kind == BlockKind::Repeater
                    && path[1].y == root.y - 1
                {
                    let p =
                        root.offset(world.get(root.x, root.y, root.z).facing.unwrap().opposite());
                    contracts.push(RepeaterHandover {
                        primitive: id,
                        support: anchor(p),
                        kind: RepeaterHandoverKind::StrongFloorOutput,
                    });
                }
            }
            if let Some(id) = target_repeater {
                if path[path.len() - 2].y == end.y + 1 {
                    let p = end.offset(world.get(end.x, end.y, end.z).facing.unwrap());
                    contracts.push(RepeaterHandover {
                        primitive: id,
                        support: anchor(p),
                        kind: RepeaterHandoverKind::WeakFloorInput,
                    });
                }
            }
            if !contracts.is_empty() {
                candidate.repeater_handovers.insert(sink, contracts);
            }
        }
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
                    crate::compile::routing::RouteTerminalKind::RepeaterIntoSupport
                } else if matches!(landing, PhysicalEndpointId::Junction(_)) {
                    crate::compile::routing::RouteTerminalKind::BareMergeDust
                } else {
                    crate::compile::routing::RouteTerminalKind::DirectedDustIntoSupport
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
    if player {
        let landings: BTreeSet<_> = candidate
            .connections
            .values()
            .filter_map(|b| {
                candidate
                    .junctions
                    .values()
                    .any(|j| j.contributors.contains(&b.landing))
                    .then(|| {
                        candidate.routes[&b.route]
                            .branches
                            .iter()
                            .find(|r| r.sink == b.sink)
                            .unwrap()
                            .terminal
                            .at
                    })
            })
            .collect();
        let mut regions = BTreeMap::new();
        for (&id, j) in &candidate.junctions {
            regions.insert(j.at, id);
        }
        let mut queue: VecDeque<_> = regions.keys().copied().collect();
        while let Some(at) = queue.pop_front() {
            let id = regions[&at];
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                for n in crate::redstone::simulator::connectivity::dust_connections(
                    &world,
                    Position::new(at.x, at.y, at.z),
                    direction,
                )
                .iter()
                {
                    match regions.entry(anchor(n)) {
                        std::collections::btree_map::Entry::Vacant(e) => {
                            e.insert(id);
                            queue.push_back(anchor(n));
                        }
                        std::collections::btree_map::Entry::Occupied(e) => {
                            assert_eq!(*e.get(), id, "distinct junction regions must not join")
                        }
                    }
                }
            }
        }
        for (at, id) in regions {
            if !landings.contains(&at) && claimed.insert(at) {
                candidate
                    .junctions
                    .get_mut(&id)
                    .unwrap()
                    .cells
                    .push(placed(Position::new(at.x, at.y, at.z)));
            }
        }
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
    if player {
        #[derive(Clone, Copy)]
        enum ConductorOwner {
            Junction(InstanceId),
            Route(RouteId),
        }
        let mut domains = BTreeMap::new();
        for (&id, j) in &candidate.junctions {
            domains.insert(j.at, ConductorOwner::Junction(id));
        }
        for (&id, tree) in &candidate.routes {
            for b in &tree.cells {
                if b.state.kind == BlockKind::RedstoneWire {
                    domains.entry(b.at).or_insert(ConductorOwner::Route(id));
                }
            }
        }
        let mut queue: VecDeque<_> = domains.keys().copied().collect();
        while let Some(at) = queue.pop_front() {
            let p = Position::new(at.x, at.y, at.z);
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                for n in
                    crate::redstone::simulator::connectivity::dust_connections(&world, p, direction)
                        .iter()
                {
                    let owner = domains[&at];
                    if let std::collections::btree_map::Entry::Vacant(e) = domains.entry(anchor(n))
                    {
                        e.insert(owner);
                        queue.push_back(anchor(n));
                    }
                }
            }
        }
        for flat in world.positions_of(BlockKind::RedstoneWire) {
            let (x, y, z) = world.decode(flat);
            let at = Anchor { x, y, z };
            if !claimed.insert(at) {
                continue;
            }
            match domains
                .get(&at)
                .expect("wire needs an actual connected owner")
            {
                ConductorOwner::Junction(id) => candidate
                    .junctions
                    .get_mut(id)
                    .unwrap()
                    .cells
                    .push(placed(Position::new(x, y, z))),
                ConductorOwner::Route(id) => candidate
                    .routes
                    .get_mut(id)
                    .unwrap()
                    .cells
                    .push(placed(Position::new(x, y, z))),
            }
        }
    } else {
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
                                    b.state.kind == BlockKind::Repeater
                                        && b.at.x == x
                                        && b.at.y == 1
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
    }
    for j in candidate.junctions.values_mut() {
        if claimed.insert(j.at) {
            j.cells.push(placed(Position::new(j.at.x, j.at.y, j.at.z)));
        }
    }
    for (&sink, contracts) in &candidate.repeater_handovers {
        for contract in contracts {
            if claimed.insert(contract.support) {
                let p = contract.support;
                candidate
                    .routes
                    .get_mut(&sink.route)
                    .unwrap()
                    .floors
                    .push(placed(Position::new(p.x, p.y, p.z)));
            }
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
