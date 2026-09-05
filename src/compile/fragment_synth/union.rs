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
///
/// # Preconditions
///
/// These three are the union's own, and no check downstream re-derives them.
/// The first is a genuine residual risk; the other two are structural, and
/// only one of them is reported as an error rather than mis-mapped.
///
/// * **A merge gate a block exports keeps the block's `isolation_mask`,
///   which the parent's fanout can invalidate.** The union carries every
///   instance's `ImplementationKey` over from the candidate that actually
///   placed it, because that key is what the cells on the ground realise.
///   For an `Or`/merge gate the default key is `Merge { isolation_mask }`,
///   and that mask is computed from the *enclosing* netlist's fanout -- so a
///   block signal that is bare inside the block can gain a consumer once the
///   block is flattened into the parent, and the mask the block chose is
///   then a bit short. Nothing here proves the mask right. The equivalence
///   proof does not: `reinstantiate_authority`
///   ([`crate::compile::fragment_synth::verify`]) re-instantiates from that
///   same stale key, so proof and candidate are self-consistent *with* a
///   wrong mask. Only the simulator can catch it, and exhaustively only when
///   the design has at most `exhaustive_input_threshold` inputs
///   ([`crate::compile::fragment_synth::config`], default 8) -- and wide
///   designs are exactly what blocks exist for. What actually holds this up
///   is geometry, not a check: every block port already carries a repeater,
///   and a repeater is a diode, which supplies the isolation a mask bit
///   would have asked for. A block that exports a merge input is the case
///   to re-derive the key for rather than carry it over.
///
/// * **The parent's instance graph must be one-to-one.** Step 1 below pairs
///   the k-th empty-path flat gate with planning instance k. An
///   `InstanceRole::Duplicate` instance breaks that count, and the union
///   then fails with [`UnionError::Incomplete`] rather than mapping a gate
///   onto the wrong instance.
///
/// * **A block must not itself contain blocks.** `block_locals` counts every
///   flat gate whose path *starts* with a block instance's name, so a nested
///   block's grandchildren land in that count, while the compiled block's
///   own graph (built from `LoweredHierarchy::block_netlist`, which is the
///   module's own gates only) does not have them. A nested block is
///   therefore mis-mapped, not rejected. A hierarchy deeper than one level
///   has to be unioned bottom-up, each level's union becoming the next
///   level's [`CompiledBlock`].
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
    #[error(
        "block {block:?} routes nothing out of input `{port}`: the parent's wire into that lever \
         would be left delivering to nothing, and no later check would notice"
    )]
    UnroutedBlockInput { block: InstanceId, port: String },
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

/// The half-open span of [`RouteId`]s one block's relocated routes occupy.
///
/// Block route ids and the parent's live in one id space, and the parent's
/// come first, so any scan of the union's routes that means "a tree that
/// came from THIS block" has to be fenced by this span -- an unfenced
/// ascending scan finds the parent's own route first and takes that instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RouteSpan {
    from: u32,
    to: u32,
}

impl RouteSpan {
    fn holds(&self, id: RouteId) -> bool {
        (self.from..self.to).contains(&id.0)
    }
}

/// One block, translated and renumbered into the parent's space, with its
/// routes lifted out so the splice can consume them one at a time.
struct Piece {
    /// The planning-space id the parent knew this block by.
    planning: InstanceId,
    /// Where that id was parked while renumbering the parent, so it can
    /// never collide with a real flat instance.
    ghost: InstanceId,
    /// Which [`RouteId`]s this block's relocated routes were given.
    span: RouteSpan,
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
        pieces.push((
            Piece {
                planning: block.id,
                ghost: ghost_of[&block.id],
                span: RouteSpan {
                    from: route_offset,
                    to: next_route,
                },
                routes,
                inputs,
                outputs: compiled.lowered.outputs.clone(),
            },
            candidate,
        ));
    }

    // ---- 5. Every output splice, then every input splice. ----
    for (piece, _) in &mut pieces {
        splice_outputs(&mut union.routes, piece)?;
    }
    for (piece, _) in &mut pieces {
        splice_inputs(&mut union.routes, piece)?;
    }

    // ---- 6. Everything else the blocks own moves in as it stands. ----
    for (piece, body) in pieces {
        union.placements.extend(body.placements);
        union.junctions.extend(body.junctions);
        for (id, observation) in body.observations {
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

/// Takes this block's own route out of `source`, whether it is still pending
/// or has already been merged into the union by an earlier output splice.
///
/// The second case is what a pass-through -- a block input wired straight to
/// a block output, with no gate between -- looks like by the time the input
/// splice runs: the output splice consumed that very tree, so it is no
/// longer pending, and it sits in the union under the block's own route id
/// still carrying the *block's* `PrimaryInput(PortId(k))` as its source.
/// Finding it there and merging it is what keeps that block-specific source
/// from surviving into the finished candidate.
///
/// The union scan is fenced to the block's own id span. Without the fence it
/// would just as happily match the parent's own route out of the parent's
/// primary input `k`, which is a different signal wearing the same
/// `PhysicalEndpointId`.
fn take_block_route_from(
    union_routes: &mut BTreeMap<RouteId, RealisedRouteTree>,
    pending: &mut BTreeMap<RouteId, RealisedRouteTree>,
    span: RouteSpan,
    source: PhysicalEndpointId,
) -> Option<RealisedRouteTree> {
    if let Some(id) = pending
        .iter()
        .find(|(_, tree)| tree.source == source)
        .map(|(id, _)| *id)
    {
        return pending.remove(&id);
    }
    let merged = union_routes
        .iter()
        .find(|(id, tree)| span.holds(**id) && tree.source == source)
        .map(|(id, _)| *id)?;
    union_routes.remove(&merged)
}

/// The block's route out of output `q` swallows the parent's route away from
/// the lamp: the lamp stops being a boundary lamp and becomes the parent
/// trunk's first dust cell, and every parent branch is re-rooted onto the
/// block's own output branch.
///
/// The block's tree is looked up in the block's own not-yet-merged routes
/// and nowhere else. An earlier draft fell back to scanning the union's
/// routes for a branch targeting `DeclaredOutput(PortId(q))`, on the theory
/// that a block whose two declared outputs share one driver would share one
/// tree and so find it already merged. That case cannot arise: a route tree
/// is grouped by its source endpoint, and two distinct declared outputs of
/// one block are produced by two distinct gates (or two distinct primary
/// inputs), hence by two distinct endpoints. What the fallback could do was
/// hijack: `DeclaredOutput(PortId(q))` reads identically in the block's port
/// space and the parent's, so the scan could take the *parent's* route to
/// ITS output `q` -- and fencing the scan to the block's id span would not
/// even close that, since an earlier output splice pushes the parent's
/// downstream branches (one of which may deliver to the parent's declared
/// output `q`) into a tree inside that very span. Without the fallback the
/// unreachable case would report [`UnionError::MissingRoute`], which is the
/// right answer for something that cannot happen.
fn splice_outputs(
    union_routes: &mut BTreeMap<RouteId, RealisedRouteTree>,
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
            take_tree_with(&mut piece.routes, &|branch: &RealisedRouteBranch| {
                branch.target == target
            })
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
        let parent_id = union_routes
            .iter()
            .find(|(_, tree)| tree.source == ghost_output)
            .map(|(id, _)| *id);
        if let Some(parent_id) = parent_id {
            let parent = union_routes
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
        union_routes.insert(holder.id, holder);
    }
    Ok(())
}

/// The parent's route into input `k` swallows the block's route away from
/// the lever: the exact terminal repeater standing on the lever stops being
/// a delivery terminal and becomes an ordinary counted mid-route refresh.
///
/// A block that has no route out of input `k` at all is refused here, not
/// skipped. Skipping was worse than it looks: the parent's delivery branch
/// has already been taken out of its tree (its target names the block's
/// ghost id, so it cannot stay), which would leave the parent's wire running
/// into a dead end that nothing downstream is in a position to notice.
fn splice_inputs(
    union_routes: &mut BTreeMap<RouteId, RealisedRouteTree>,
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
            take_tree_with(union_routes, &|branch: &RealisedRouteBranch| {
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
        let Some(inner) =
            take_block_route_from(union_routes, &mut piece.routes, piece.span, source)
        else {
            return Err(UnionError::UnroutedBlockInput {
                block: piece.planning,
                port: port.clone(),
            });
        };
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
        union_routes.insert(parent.id, parent);
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
    use crate::compile::routing::{RouteTerminalKind, TerminalRecord};
    use crate::redstone::world::block::BlockKind;

    fn anchor(x: i32) -> Anchor {
        Anchor { x, y: 0, z: 0 }
    }

    /// A branch whose terminal stands on the last cell of `path`. Only the
    /// fields the splice reads are meaningful.
    fn a_branch(target: RouteTarget, root: Anchor, path: &[Anchor]) -> RealisedRouteBranch {
        let sink = RoutedSinkId {
            route: RouteId(0),
            ordinal: 0,
        };
        RealisedRouteBranch {
            sink,
            target,
            root,
            path: path.to_vec(),
            terminal: TerminalRecord {
                sink,
                at: *path.last().unwrap_or(&root),
                state: crate::compile::stone(),
                kind: RouteTerminalKind::OutputTerminalRepeater,
                repeaters: 0,
                delayed_owner: None,
            },
        }
    }

    fn a_tree(
        id: u32,
        source: PhysicalEndpointId,
        branches: Vec<RealisedRouteBranch>,
    ) -> RealisedRouteTree {
        RealisedRouteTree {
            id: RouteId(id),
            source,
            cells: Vec::new(),
            floors: Vec::new(),
            branches,
        }
    }

    const GHOST: InstanceId = InstanceId(99);

    fn a_piece(inputs: Vec<(String, Anchor)>, outputs: Vec<String>) -> Piece {
        Piece {
            planning: InstanceId(7),
            ghost: GHOST,
            span: RouteSpan { from: 10, to: 20 },
            routes: BTreeMap::new(),
            inputs,
            outputs,
        }
    }

    fn routes_of(trees: Vec<RealisedRouteTree>) -> BTreeMap<RouteId, RealisedRouteTree> {
        trees.into_iter().map(|tree| (tree.id, tree)).collect()
    }

    /// Finding 3: `DeclaredOutput(PortId(q))` reads identically in a block's
    /// port space and in the parent's, and the parent's routes come first in
    /// id order. The output splice must therefore look for the block's own
    /// output route in the block's own routes and NOWHERE else -- the
    /// earlier fallback scan of the union's routes would take the parent's
    /// route to the parent's output `q` and splice the block onto it.
    #[test]
    fn a_block_output_never_takes_the_parents_own_route_to_the_same_port_number() {
        let parent = a_tree(
            0,
            PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: InstanceId(0),
                node: TopologyNodeId(0),
            }),
            vec![a_branch(
                RouteTarget::DeclaredOutput(PortId(0)),
                anchor(0),
                &[anchor(1)],
            )],
        );
        let mut union_routes = routes_of(vec![parent.clone()]);
        let mut piece = a_piece(Vec::new(), vec!["y".to_string()]);

        let error = splice_outputs(&mut union_routes, &mut piece)
            .expect_err("the block has no route out of `y` of its own");
        assert!(
            matches!(&error, UnionError::MissingRoute { port, .. } if port == "y"),
            "expected a missing-route refusal, got {error}"
        );
        assert_eq!(
            union_routes.get(&RouteId(0)),
            Some(&parent),
            "the parent's own route to its declared output 0 must be untouched"
        );
    }

    /// Finding 2, the case that is refused: a block that routes nothing out
    /// of one of its levers. The parent's delivery branch has already been
    /// lifted out of its tree by then (its target names the block's ghost
    /// id, so it cannot stay), so skipping would leave the parent's wire
    /// running into a dead end that no later check looks at.
    #[test]
    fn a_block_input_with_no_route_out_of_its_lever_is_refused_not_skipped() {
        let lever = anchor(5);
        let mut union_routes = routes_of(vec![a_tree(
            0,
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            vec![a_branch(
                RouteTarget::Connection(ConnectionId::External {
                    instance: GHOST,
                    input_index: 0,
                }),
                anchor(0),
                &[lever],
            )],
        )]);
        let mut piece = a_piece(vec![("a".to_string(), lever)], Vec::new());

        let error = splice_inputs(&mut union_routes, &mut piece)
            .expect_err("nothing inside the block reads input `a`");
        assert!(
            matches!(&error, UnionError::UnroutedBlockInput { port, .. } if port == "a"),
            "expected an unrouted-input refusal, got {error}"
        );
    }

    /// Finding 2, the case that now works: `assign out = in`. By the time
    /// the input splice runs, the output splice has already consumed the
    /// block's tree out of `PrimaryInput(PortId(k))` -- it was the tree
    /// carrying the `DeclaredOutput` branch -- and left it in the union
    /// under the block's own route id, still wearing the BLOCK's source. The
    /// input splice has to find it there and merge it, or that block-space
    /// source survives into the finished candidate aliased onto the parent's
    /// port numbering.
    ///
    /// The parent's own route out of ITS primary input 0 wears the identical
    /// [`PhysicalEndpointId`] and comes first in id order, so the search has
    /// to be fenced to the block's route span.
    #[test]
    fn a_pass_through_block_route_is_found_after_the_output_splice_consumed_it() {
        let lever = anchor(5);
        let delivery = a_tree(
            0,
            PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: InstanceId(0),
                node: TopologyNodeId(0),
            }),
            vec![a_branch(
                RouteTarget::Connection(ConnectionId::External {
                    instance: GHOST,
                    input_index: 0,
                }),
                anchor(0),
                &[lever],
            )],
        );
        // The PARENT's own primary input 0: same endpoint identity, different
        // signal, lower route id.
        let decoy = a_tree(
            1,
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            vec![a_branch(
                RouteTarget::DeclaredOutput(PortId(9)),
                anchor(20),
                &[anchor(21)],
            )],
        );
        // What the output splice left behind for the pass-through.
        let merged = a_tree(
            11,
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            vec![a_branch(
                RouteTarget::DeclaredOutput(PortId(3)),
                anchor(6),
                &[anchor(7)],
            )],
        );
        let mut union_routes = routes_of(vec![delivery, decoy.clone(), merged]);
        let mut piece = a_piece(vec![("a".to_string(), lever)], Vec::new());

        splice_inputs(&mut union_routes, &mut piece).expect("the pass-through tree is spliced");

        assert!(
            !union_routes.contains_key(&RouteId(11)),
            "the block's own tree must have been consumed by the join"
        );
        assert_eq!(
            union_routes.get(&RouteId(1)),
            Some(&decoy),
            "the parent's own route out of its primary input 0 must be untouched"
        );
        let joined = &union_routes[&RouteId(0)];
        assert_eq!(joined.branches.len(), 1);
        assert_eq!(
            joined.branches[0].target,
            RouteTarget::DeclaredOutput(PortId(3))
        );
        assert_eq!(joined.branches[0].root, anchor(0));
        assert_eq!(
            joined.branches[0].path,
            vec![lever, anchor(7)],
            "the joined path is the parent's up to the lever, then the block's"
        );
    }

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
        // Every boundary join has to show up in the route delays. A floor on
        // the grand total says nothing here -- a compiled full adder's own
        // internal routes clear five repeaters on their own, whether or not
        // a single boundary was counted -- so measure each of the five joins
        // against the two halves it was made from. A joined branch's count
        // must be the parent half, plus the block half, plus exactly one for
        // the boundary repeater itself, which stopped being an excluded
        // delivery terminal and became an ordinary counted mid-route refresh.
        let block_id = planned.candidate.instances.blocks[0].id;
        let offset = planned.block_offsets[&block_id];
        let crossing = |at: Anchor| {
            let mut counts = union
                .routes
                .values()
                .flat_map(|route| &route.branches)
                .filter(|branch| branch.path.contains(&at))
                .map(|branch| branch.terminal.repeaters)
                .collect::<Vec<_>>();
            counts.sort_unstable();
            counts
        };
        for (index, port) in block.lowered.inputs.iter().enumerate() {
            let lever = shift(block.inputs[port].cell, offset);
            let pb = planned
                .candidate
                .routes
                .values()
                .flat_map(|route| &route.branches)
                .find(|branch| {
                    branch.target
                        == RouteTarget::Connection(ConnectionId::External {
                            instance: block_id,
                            input_index: u16::try_from(index).unwrap(),
                        })
                })
                .expect("the parent routes every block input");
            assert_eq!(pb.terminal.at, lever, "input `{port}` terminal");
            assert_eq!(
                pb.terminal.state.kind,
                BlockKind::Repeater,
                "input `{port}` boundary is a repeater"
            );
            assert_eq!(
                pb.terminal.kind,
                RouteTerminalKind::OutputTerminalRepeater,
                "input `{port}` boundary was an excluded delivery terminal"
            );
            let inner = block
                .candidate
                .routes
                .values()
                .find(|tree| {
                    tree.source
                        == PhysicalEndpointId::PrimaryInput(PortId(u32::try_from(index).unwrap()))
                })
                .expect("the block routes every input it reads");
            let mut expected = inner
                .branches
                .iter()
                .map(|branch| branch.terminal.repeaters + pb.terminal.repeaters + 1)
                .collect::<Vec<_>>();
            expected.sort_unstable();
            assert_eq!(crossing(lever), expected, "input `{port}` join");
        }
        for (index, port) in block.lowered.outputs.iter().enumerate() {
            let ob = block
                .candidate
                .routes
                .values()
                .flat_map(|route| &route.branches)
                .find(|branch| {
                    branch.target
                        == RouteTarget::DeclaredOutput(PortId(u32::try_from(index).unwrap()))
                })
                .expect("the block routes every declared output");
            assert_eq!(
                ob.terminal.state.kind,
                BlockKind::Repeater,
                "output `{port}` boundary is a repeater"
            );
            assert_eq!(
                ob.terminal.kind,
                RouteTerminalKind::OutputTerminalRepeater,
                "output `{port}` boundary was an excluded delivery terminal"
            );
            let downstream = planned
                .candidate
                .routes
                .values()
                .find(|tree| {
                    tree.source
                        == PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                            instance: block_id,
                            node: TopologyNodeId(u16::try_from(index).unwrap()),
                        })
                })
                .expect("the parent reads every block output");
            let mut expected = downstream
                .branches
                .iter()
                .map(|branch| branch.terminal.repeaters + ob.terminal.repeaters + 1)
                .collect::<Vec<_>>();
            expected.sort_unstable();
            assert_eq!(
                crossing(shift(ob.terminal.at, offset)),
                expected,
                "output `{port}` join"
            );
        }
        let certified = crate::compile::fragment_synth::seed::certify_planned(
            union,
            &lowered.flat,
            services,
        )
        .expect("certifies");
        assert!(certified.metrics().quality.observed_settle > 0);
    }
}
