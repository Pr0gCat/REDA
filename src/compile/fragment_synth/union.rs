#![allow(dead_code)] // Task 12 is the first production caller of this seam.

//! Dissolving a parent's compiled blocks into ONE flat candidate.
//!
//! [`crate::compile::fragment_synth::seed::plan_parent_with_services`] stops
//! with each block's body held as a single opaque placeholder
//! `PrimitivePlacement` and the parent's wires routed to the block's lever
//! and lamp cells. This module finishes the job: it translates and
//! renumbers each compiled block into the parent's numbering space, drops
//! the placeholder, and splices the parent's routes onto the block's own so
//! that what comes out is indistinguishable from a flat compile -- one gate
//! per flat instance, ordinary routes, and NOT ONE block-specific object
//! left anywhere. The existing verifier, equivalence proof and simulator
//! then run over it unchanged.
//!
//! Three facts shape the whole implementation, and each of them is a place
//! where the code had to overrule the plan:
//!
//! * **The renumbered block graph is discarded, never merged.** A
//!   renumbered `InstanceGraph` carries a stale `ValidatedTopology`
//!   fingerprint (see [`relocate::renumber`]'s doc comment), and `verify.rs`
//!   compares that field structurally. The union's graph is therefore built
//!   fresh with [`InstanceGraph::one_to_one_with_implementations`] over the
//!   flattened netlist, carrying over only each instance's
//!   `ImplementationKey` so the rebuilt topology matches the physical cells
//!   that were actually placed.
//!
//! * **The placeholder cannot survive.** `validate_physical_ownership`
//!   refuses a `PrimitivePlacement` with `delayed: None` whose blocks
//!   contain a repeater, and a block body is full of repeaters. The
//!   placeholder is dissolved into the block's own per-primitive placements,
//!   not kept alongside them.
//!
//! * **`PrimitiveId { instance: block, node: TopologyNodeId(0) }` is
//!   retired.** In the planned parent it is both the placeholder's key in
//!   `placements` and block output 0's driver identity. After stamping
//!   neither meaning survives: the placeholder is gone, and the block's
//!   output is named by the real gate primitive that drives it. The whole
//!   ghost id space is dropped from the union.
//!
//! The splice itself, in one sentence per direction. At an input, the
//! parent's route ends with an exact repeater standing on the block's lever
//! cell and the block's own route from that input starts one cell east, so
//! the two become one tree whose branches are the parent's path followed by
//! the block's; the boundary repeater stops being a delivery terminal and
//! becomes an ordinary counted mid-route refresh. At an output, the block's
//! route ends with its output terminal repeater one cell west of the lamp,
//! the lamp itself is dropped (the parent's route already owns that cell as
//! its root dust), and the parent's branches are re-rooted onto the block's
//! output branch. Every output splice runs before every input splice, so a
//! chained "block A out -> parent wire -> block B in" input splice still
//! finds its parent branch after A's tree absorbed it.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::blocks::CompiledBlock;
use crate::compile::fragment_synth::candidate::{
    CandidateError, ConnectionBinding, ExpandedPhysicalCandidate, PlacedBlock,
    RealisedRouteBranch, RealisedRouteTree, RouteTarget,
};
use crate::compile::fragment_synth::identity::{
    ConnectionId, ImplementationKey, InstanceId, ObservationId, PhysicalEndpointId, PortId,
    PrimitiveId, RouteId, RoutedSinkId, TopologyNodeId,
};
use crate::compile::fragment_synth::instance_graph::{
    BlockSpec, InstanceDriver, InstanceGraph, PhysicalDriver, PhysicalSink, SynthesisError,
};
use crate::compile::fragment_synth::relocate::{self, IdMap, Offset, RelocateError};
use crate::compile::fragment_synth::seed::{refresh_exact_route_delays, PlannedParent};
use crate::compile::geometry::Anchor;
use crate::compile::hierarchy::{GatePath, LoweredHierarchy, PortBinding};
use crate::compile::routing::DelayedOwner;
use crate::compile::topology::{GateKind, Library};
use crate::compile::{Gate, Netlist};

/// Everything the union needs: the planned parent, the compiled blocks it
/// stamps, and the flattened netlist (plus one [`GatePath`] per flat gate)
/// that the result must look like it was compiled from.
pub(crate) struct UnionInput<'a> {
    pub parent: &'a PlannedParent,
    pub blocks: &'a [CompiledBlock],
    /// Flattened lowered netlist of this module.
    pub flat: &'a Netlist,
    /// One per flat gate.
    pub paths: &'a [GatePath],
    pub library: &'a Library,
}

#[derive(Debug, Error)]
pub(crate) enum UnionError {
    #[error("relocating a block into the parent failed: {0}")]
    Relocate(#[from] RelocateError),
    #[error("the flat instance graph could not be rebuilt: {0}")]
    InstanceGraph(#[from] SynthesisError),
    #[error("block {block:?} port `{port}` has no route to splice")]
    MissingRoute { block: InstanceId, port: String },
    #[error("the union is not a well-formed flat candidate: {0}")]
    Shape(#[from] CandidateError),
    #[error("the union is internally incomplete: {0}")]
    Incomplete(&'static str),
}

/// One block instantiation as the parent's planning netlist describes it.
pub(crate) struct BlockSpecOwned {
    pub name: String,
    pub block: u32,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

impl BlockSpecOwned {
    pub(crate) fn as_spec(&self) -> BlockSpec<'_> {
        BlockSpec {
            name: &self.name,
            block: self.block,
            inputs: &self.inputs,
            outputs: &self.outputs,
        }
    }
}

/// The parent's lowered own gates followed by one `Buf` gate per block
/// output, so `signal_table` gives every block output a `LogicalSignalId`.
///
/// The planning netlist's inputs are the module's real inputs only (block
/// outputs are driven by the synthetic gates) and its outputs the module's
/// real outputs.
pub(crate) fn planning_netlist(
    lowered: &LoweredHierarchy,
    module: &str,
    ordered_blocks: &[CompiledBlock],
) -> (Netlist, Vec<BlockSpecOwned>) {
    let parent = &lowered.modules[module];
    let mut gates = parent.gates.clone();
    let mut specs = Vec::new();
    for instance in &parent.instances {
        let block_index = ordered_blocks
            .iter()
            .position(|block| block.module == instance.module)
            .expect("every instantiated module was compiled");
        let compiled = &ordered_blocks[block_index];
        let signal = |port: &str| match &instance.ports[port] {
            PortBinding::Signal(signal) => signal.clone(),
            _ => unreachable!("constants were specialised away"),
        };
        let inputs: Vec<String> = compiled.lowered.inputs.iter().map(|p| signal(p)).collect();
        let outputs: Vec<String> = compiled.lowered.outputs.iter().map(|p| signal(p)).collect();
        for (port, output) in compiled.lowered.outputs.iter().zip(&outputs) {
            gates.push(Gate {
                name: format!("{}.{port}", instance.name),
                inputs: inputs.clone(),
                output: output.clone(),
                kind: GateKind::Buf,
            });
        }
        specs.push(BlockSpecOwned {
            name: instance.name.clone(),
            block: block_index as u32,
            inputs,
            outputs,
        });
    }
    (
        Netlist {
            inputs: parent.inputs.clone(),
            outputs: parent.outputs.clone(),
            gates,
        },
        specs,
    )
}

/// One block, translated and renumbered into the parent's space, with its
/// routes lifted out so the splice can consume them one at a time.
struct Piece {
    /// The planning-space id the parent knew this block by.
    planning: InstanceId,
    /// Where that id was parked while renumbering the parent, so it can
    /// never collide with a real flat instance.
    ghost: InstanceId,
    candidate: ExpandedPhysicalCandidate,
    routes: BTreeMap<RouteId, RealisedRouteTree>,
    /// Input port names in declared order, with their lever cell in parent
    /// coordinates.
    inputs: Vec<(String, Anchor)>,
    /// Output port names in declared order.
    outputs: Vec<String>,
}

pub(crate) fn union_candidate(
    input: UnionInput<'_>,
) -> Result<ExpandedPhysicalCandidate, UnionError> {
    let parent_candidate = &input.parent.candidate;
    let block_instances = parent_candidate.instances.blocks.clone();

    let flat_len =
        u32::try_from(input.flat.gates.len()).map_err(|_| UnionError::Incomplete("flat width"))?;
    if input.paths.len() != input.flat.gates.len() {
        return Err(UnionError::Incomplete("one gate path per flat gate"));
    }

    // ---- 1. Which flat instance is which planning / block-local one? ----
    //
    // `flatten` emits the parent's own gates first, in the parent's own gate
    // order, then each instance's subtree in instance order; `planning_netlist`
    // keeps the parent's gates in exactly that order too, with the synthetic
    // block-output rows appended after them. So the k-th empty-path flat gate
    // is planning instance k, and the k-th flat gate under instance `name` is
    // that block's own instance k.
    let mut parent_instances = BTreeMap::<InstanceId, InstanceId>::new();
    let mut block_locals = BTreeMap::<String, BTreeMap<InstanceId, InstanceId>>::new();
    let mut parent_seen = 0u32;
    let mut block_seen = BTreeMap::<String, u32>::new();
    for (index, path) in input.paths.iter().enumerate() {
        let flat = InstanceId(
            u32::try_from(index).map_err(|_| UnionError::Incomplete("flat instance width"))?,
        );
        match path.path.first() {
            None => {
                parent_instances.insert(InstanceId(parent_seen), flat);
                parent_seen += 1;
            }
            Some(name) => {
                let seen = block_seen.entry(name.clone()).or_insert(0);
                block_locals
                    .entry(name.clone())
                    .or_default()
                    .insert(InstanceId(*seen), flat);
                *seen += 1;
            }
        }
    }

    // ---- 2. A fresh flat graph, keeping the implementations actually built.
    let mut implementations = BTreeMap::<InstanceId, ImplementationKey>::new();
    for instance in &parent_candidate.instances.instances {
        let flat = *parent_instances
            .get(&instance.id)
            .ok_or(UnionError::Incomplete("parent instance has no flat gate"))?;
        implementations.insert(flat, instance.implementation);
    }
    for block in &block_instances {
        let compiled = compiled_of(&input, block.block)?;
        let name = block
            .path
            .first()
            .ok_or(UnionError::Incomplete("block instance path"))?;
        let locals = block_locals
            .get(name)
            .ok_or(UnionError::Incomplete("block instance has no flat gates"))?;
        for instance in &compiled.candidate.instances.instances {
            let flat = *locals
                .get(&instance.id)
                .ok_or(UnionError::Incomplete("block instance has no flat gate"))?;
            implementations.insert(flat, instance.implementation);
        }
    }
    let flat_graph = InstanceGraph::one_to_one_with_implementations(
        input.flat,
        input.library,
        &implementations,
    )?;

    // ---- 3. The parent, renumbered; blocks parked on ghost ids. ----
    let mut parent_map = IdMap {
        instances: parent_instances,
        route_offset: 0,
    };
    let mut ghosts = BTreeSet::<InstanceId>::new();
    let mut ghost_of = BTreeMap::<InstanceId, InstanceId>::new();
    for (index, block) in block_instances.iter().enumerate() {
        let ghost = InstanceId(
            flat_len
                .checked_add(
                    u32::try_from(index).map_err(|_| UnionError::Incomplete("block count"))?,
                )
                .ok_or(UnionError::Incomplete("ghost identity width"))?,
        );
        parent_map.instances.insert(block.id, ghost);
        ghost_of.insert(block.id, ghost);
        ghosts.insert(ghost);
    }
    let mut union = parent_candidate.clone();
    relocate::renumber(&mut union, &parent_map)?;
    // The placeholder bodies go now: `validate_physical_ownership` refuses a
    // `delayed: None` placement holding repeaters, and the real per-primitive
    // placements arriving below are what owns those cells from here on.
    union.placements.retain(|id, _| !ghosts.contains(&id.instance));

    // ---- 4. Each block, translated and renumbered into flat ids. ----
    let mut next_route = union.routes.keys().map(|id| id.0 + 1).max().unwrap_or(0);
    let mut pieces = Vec::new();
    for block in &block_instances {
        let compiled = compiled_of(&input, block.block)?;
        let name = block
            .path
            .first()
            .ok_or(UnionError::Incomplete("block instance path"))?;
        let locals = block_locals
            .get(name)
            .ok_or(UnionError::Incomplete("block instance has no flat gates"))?
            .clone();
        let offset = *input
            .parent
            .block_offsets
            .get(&block.id)
            .ok_or(UnionError::Incomplete("planned block offset"))?;
        let mut candidate = compiled.candidate.clone();
        relocate::translate(&mut candidate, offset);
        let span = candidate.routes.keys().map(|id| id.0 + 1).max().unwrap_or(0);
        let route_offset = next_route;
        next_route = next_route
            .checked_add(span)
            .ok_or(UnionError::Incomplete("route identity width"))?;
        relocate::renumber(
            &mut candidate,
            &IdMap {
                instances: locals,
                route_offset,
            },
        )?;
        let routes = std::mem::take(&mut candidate.routes);
        let inputs = compiled
            .lowered
            .inputs
            .iter()
            .map(|port| {
                let cell = compiled
                    .inputs
                    .get(port)
                    .ok_or(UnionError::Incomplete("compiled block input port"))?
                    .cell;
                Ok((port.clone(), shift(cell, offset)))
            })
            .collect::<Result<Vec<_>, UnionError>>()?;
        pieces.push(Piece {
            planning: block.id,
            ghost: ghost_of[&block.id],
            candidate,
            routes,
            inputs,
            outputs: compiled.lowered.outputs.clone(),
        });
    }

    // ---- 5. Every output splice, then every input splice. ----
    for piece in &mut pieces {
        splice_outputs(&mut union, piece)?;
    }
    for piece in &mut pieces {
        splice_inputs(&mut union, piece)?;
    }

    // ---- 6. Everything else the blocks own moves in as it stands. ----
    for piece in pieces {
        union.placements.extend(piece.candidate.placements);
        union.junctions.extend(piece.candidate.junctions);
        for (id, observation) in piece.candidate.observations {
            // A block's own port observations name `PortId`s in the parent's
            // port space; they would silently overwrite the parent's.
            if matches!(
                id,
                ObservationId::PrimaryInput(_) | ObservationId::DeclaredOutput(_)
            ) {
                continue;
            }
            union.observations.insert(id, observation);
        }
        // `boundaries`, `pins`, `pin_contracts` and `pin_name_bindings` are a
        // block's own interface, and the union has none: the lever and lamp
        // cells they described are now ordinary route cells.
        union.routes.extend(piece.routes);
    }

    // ---- 7. One consistent numbering over the spliced trees. ----
    union.instances = flat_graph;
    normalise_routes_and_connections(&mut union)?;
    refresh_declared_output_owners(&mut union);

    union.validate_shape()?;
    Ok(union)
}

fn compiled_of<'a>(input: &UnionInput<'a>, index: u32) -> Result<&'a CompiledBlock, UnionError> {
    usize::try_from(index)
        .ok()
        .and_then(|index| input.blocks.get(index))
        .ok_or(UnionError::Incomplete("compiled block index"))
}

fn shift(at: Anchor, offset: Offset) -> Anchor {
    Anchor {
        x: at.x + offset.dx,
        y: at.y + offset.dy,
        z: at.z + offset.dz,
    }
}

/// Adds every cell and floor `source` owns to `into`, skipping any anchor
/// `into` already claims. The two trees meet exactly on the boundary cell,
/// which belongs to whichever of them laid it.
fn absorb_cells(into: &mut RealisedRouteTree, source: &RealisedRouteTree) {
    let mut claimed = into
        .cells
        .iter()
        .chain(into.floors.iter())
        .map(|block| block.at)
        .collect::<BTreeSet<_>>();
    for cell in &source.cells {
        if claimed.insert(cell.at) {
            into.cells.push(cell.clone());
        }
    }
    for floor in &source.floors {
        if claimed.insert(floor.at) {
            into.floors.push(floor.clone());
        }
    }
}

/// Ensures `tree` owns a floor at `at`, unless something in `tree` already
/// stands there.
fn ensure_floor(tree: &mut RealisedRouteTree, block: &PlacedBlock) {
    if tree
        .cells
        .iter()
        .chain(tree.floors.iter())
        .any(|owned| owned.at == block.at)
    {
        return;
    }
    tree.floors.push(block.clone());
}

/// Takes the tree holding a branch that satisfies `wanted` out of `routes`,
/// together with that branch's index.
fn take_tree_with<F>(
    routes: &mut BTreeMap<RouteId, RealisedRouteTree>,
    wanted: &F,
) -> Option<(RealisedRouteTree, usize)>
where
    F: Fn(&RealisedRouteBranch) -> bool,
{
    let (id, index) = routes.iter().find_map(|(id, tree)| {
        tree.branches
            .iter()
            .position(wanted)
            .map(|index| (*id, index))
    })?;
    Some((routes.remove(&id)?, index))
}

/// The same, over a block's not-yet-merged routes first and the union's
/// routes second. A block whose two declared outputs are driven by one gate
/// shares a single route tree, so the second output splice finds that tree
/// already merged into the union.
fn take_block_or_union_tree<F>(
    union: &mut ExpandedPhysicalCandidate,
    pending: &mut BTreeMap<RouteId, RealisedRouteTree>,
    wanted: F,
) -> Option<(RealisedRouteTree, usize)>
where
    F: Fn(&RealisedRouteBranch) -> bool,
{
    take_tree_with(pending, &wanted).or_else(|| take_tree_with(&mut union.routes, &wanted))
}

/// The block's route out of output `q` swallows the parent's route away from
/// the lamp: the lamp stops being a boundary lamp and becomes the parent
/// trunk's first dust cell, and every parent branch is re-rooted onto the
/// block's own output branch.
fn splice_outputs(
    union: &mut ExpandedPhysicalCandidate,
    piece: &mut Piece,
) -> Result<(), UnionError> {
    for (index, port) in piece.outputs.clone().iter().enumerate() {
        let node = TopologyNodeId(
            u16::try_from(index).map_err(|_| UnionError::Incomplete("block port width"))?,
        );
        let target = RouteTarget::DeclaredOutput(PortId(
            u32::try_from(index).map_err(|_| UnionError::Incomplete("block port width"))?,
        ));
        let Some((mut holder, ob_index)) =
            take_block_or_union_tree(union, &mut piece.routes, |branch| branch.target == target)
        else {
            return Err(UnionError::MissingRoute {
                block: piece.planning,
                port: port.clone(),
            });
        };
        let ob = holder.branches.remove(ob_index);

        let ghost_output = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance: piece.ghost,
            node,
        });
        let parent_id = union
            .routes
            .iter()
            .find(|(_, tree)| tree.source == ghost_output)
            .map(|(id, _)| *id);
        if let Some(parent_id) = parent_id {
            let parent = union
                .routes
                .remove(&parent_id)
                .ok_or(UnionError::Incomplete("parent route out of a block"))?;
            absorb_cells(&mut holder, &parent);
            for branch in parent.branches {
                let mut path = ob.path.clone();
                path.extend(branch.path.iter().copied());
                holder.branches.push(RealisedRouteBranch {
                    sink: branch.sink,
                    target: branch.target,
                    root: ob.root,
                    path,
                    terminal: branch.terminal,
                });
            }
        }
        // No parent route at all means nothing in the parent reads this
        // block output; dropping `ob` alone already retires its lamp.
        union.routes.insert(holder.id, holder);
    }
    Ok(())
}

/// The parent's route into input `k` swallows the block's route away from
/// the lever: the exact terminal repeater standing on the lever stops being
/// a delivery terminal and becomes an ordinary counted mid-route refresh.
fn splice_inputs(
    union: &mut ExpandedPhysicalCandidate,
    piece: &mut Piece,
) -> Result<(), UnionError> {
    for (index, (port, lever)) in piece.inputs.clone().iter().enumerate() {
        let input_index =
            u16::try_from(index).map_err(|_| UnionError::Incomplete("block port width"))?;
        let target = RouteTarget::Connection(ConnectionId::External {
            instance: piece.ghost,
            input_index,
        });
        let Some((mut parent, pb_index)) =
            take_tree_with(&mut union.routes, &|branch: &RealisedRouteBranch| {
                branch.target == target
            })
        else {
            return Err(UnionError::MissingRoute {
                block: piece.planning,
                port: port.clone(),
            });
        };
        let pb = parent.branches.remove(pb_index);
        if pb.terminal.at != *lever {
            return Err(UnionError::Incomplete(
                "parent route into a block misses its lever",
            ));
        }

        let source = PhysicalEndpointId::PrimaryInput(PortId(
            u32::try_from(index).map_err(|_| UnionError::Incomplete("block port width"))?,
        ));
        let inner = piece
            .routes
            .iter()
            .find(|(_, tree)| tree.source == source)
            .map(|(id, _)| *id);
        if let Some(inner) = inner {
            let inner = piece
                .routes
                .remove(&inner)
                .ok_or(UnionError::Incomplete("block route from an input"))?;
            absorb_cells(&mut parent, &inner);
            for branch in inner.branches {
                let mut path = pb.path.clone();
                path.extend(branch.path.iter().copied());
                parent.branches.push(RealisedRouteBranch {
                    sink: branch.sink,
                    target: branch.target,
                    root: pb.root,
                    path,
                    terminal: branch.terminal,
                });
            }
        }
        // The lever's own stone was the block's input boundary floor, and the
        // boundary is gone; the parent's route almost always floored the same
        // cell under its terminal repeater, so only fill the gap.
        ensure_floor(
            &mut parent,
            &PlacedBlock {
                at: Anchor {
                    y: lever.y - 1,
                    ..*lever
                },
                state: crate::compile::stone(),
            },
        );
        union.routes.insert(parent.id, parent);
    }
    Ok(())
}

/// Gives every branch in every tree a sink that agrees with the tree that
/// now owns it, and rebuilds `connections` from what the routes really
/// deliver. Splicing moves branches between trees wholesale, so rebuilding
/// is both simpler and stricter than patching: a connection the routes do
/// not reach simply is not in the map, and `validate_shape` says so.
fn normalise_routes_and_connections(
    union: &mut ExpandedPhysicalCandidate,
) -> Result<(), UnionError> {
    let mut connections = BTreeMap::<ConnectionId, ConnectionBinding>::new();
    for (&key, route) in union.routes.iter_mut() {
        if key != route.id {
            return Err(UnionError::Incomplete("route key disagrees with its tree"));
        }
        for (ordinal, branch) in route.branches.iter_mut().enumerate() {
            let sink = RoutedSinkId {
                route: route.id,
                ordinal: u16::try_from(ordinal)
                    .map_err(|_| UnionError::Incomplete("route fanout width"))?,
            };
            branch.sink = sink;
            branch.terminal.sink = sink;
            if matches!(branch.terminal.delayed_owner, Some(DelayedOwner::Route(_))) {
                branch.terminal.delayed_owner = Some(DelayedOwner::Route(route.id));
            }
            if let RouteTarget::Connection(id) = branch.target {
                connections.insert(
                    id,
                    ConnectionBinding {
                        id,
                        source: route.source,
                        landing: PhysicalEndpointId::Landing(id),
                        route: route.id,
                        sink,
                    },
                );
            }
        }
        // The boundary repeaters became ordinary mid-route refreshes, so
        // every branch's tick count has to be measured again over the joined
        // path.
        refresh_exact_route_delays(route);
    }
    union.connections = connections;
    Ok(())
}

/// A declared output driven by a block named the block's ghost instance as
/// its logical owner. The flat graph knows the real gate.
fn refresh_declared_output_owners(union: &mut ExpandedPhysicalCandidate) {
    let owners = union
        .instances
        .assignments
        .iter()
        .filter_map(|assignment| match (assignment.sink, &assignment.driver) {
            (PhysicalSink::DeclaredOutput(port), PhysicalDriver::Instance(driver)) => {
                let owner = match driver {
                    InstanceDriver::Primitive { logical_owner, .. }
                    | InstanceDriver::Junction { logical_owner, .. } => *logical_owner,
                };
                Some((port, owner))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    for (&id, observation) in union.observations.iter_mut() {
        if let ObservationId::DeclaredOutput(port) = id {
            observation.site.logical_owner = owners.get(&port).copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::seed::{ParentBlocks, SeedInput};

    /// gate -> block -> gate, certified as one flat circuit.
    #[test]
    fn a_gate_block_gate_chain_unions_into_one_certified_flat_candidate() {
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        // top(x, b, c) : n = NOT x ; u0 = full_adder(n, b, c) ; z = NOR(sum) ; cout = u0.cout
        let mut hb = crate::circuits::hierarchical_builder::HierarchicalNetlistBuilder::new();
        crate::circuits::hierarchical_builder::full_adder_module(&mut hb);
        hb.module("top", &["x", "b", "c"], &["z", "cout"], |m| {
            let n = m.gates.not("x");
            m.instance(
                "u0",
                "full_adder",
                &[
                    ("a", &n),
                    ("b", "b"),
                    ("cin", "c"),
                    ("sum", "sum"),
                    ("cout", "cout"),
                ],
            );
            m.gates.nor_named("z", "z", &["sum".to_string()]);
        });
        let design = hb.finish("top");
        let lowered = crate::compile::hierarchy::lower_hierarchy(&design).unwrap();
        let fa = lowered.block_netlist("full_adder");
        let block =
            crate::compile::fragment_synth::blocks::compile_block("full_adder", &fa, services)
                .unwrap();
        let ordered = vec![block.clone()];
        let (planning, owned) = planning_netlist(&lowered, "top", &ordered);
        let graph = InstanceGraph::with_blocks(
            &planning,
            &library,
            &owned.iter().map(BlockSpecOwned::as_spec).collect::<Vec<_>>(),
        )
        .unwrap();
        let planned = crate::compile::fragment_synth::seed::plan_parent_with_services(
            SeedInput {
                lowered: &planning,
                source_provenance: None,
                pins: None,
            },
            services,
            graph,
            ParentBlocks {
                compiled: std::slice::from_ref(&block),
            },
            &BTreeMap::new(),
        )
        .unwrap();
        let union = union_candidate(UnionInput {
            parent: &planned,
            blocks: std::slice::from_ref(&block),
            flat: &lowered.flat,
            paths: &lowered.paths,
            library: &library,
        })
        .expect("unions");
        union.validate_shape().expect("flat shape");
        assert!(union.instances.blocks.is_empty());
        assert_eq!(union.instances.instances.len(), lowered.flat.gates.len());
        // No block boundary objects survive.
        assert_eq!(
            union.boundaries.len(),
            lowered.flat.inputs.len() + lowered.flat.outputs.len()
        );
        // Boundary repeaters are counted in route delays.
        let repeaters: u64 = union
            .routes
            .values()
            .flat_map(|route| &route.branches)
            .map(|branch| branch.terminal.repeaters)
            .sum();
        assert!(
            repeaters >= 3 + 2,
            "three input joins and two output joins add repeaters"
        );
        let certified = crate::compile::fragment_synth::seed::certify_planned(
            union,
            &lowered.flat,
            services,
        )
        .expect("certifies");
        assert!(certified.metrics().quality.observed_settle > 0);
    }
}
