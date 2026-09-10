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
    endpoint_for_driver, CandidateError, ConnectionBinding, ExpandedPhysicalCandidate, PlacedBlock,
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
use crate::compile::fragment_synth::route_opt::{prune_route, relocate_refresh, ParentRouteChoice};
use crate::compile::fragment_synth::seed::{refresh_exact_route_delays, PlannedParent};
use crate::compile::geometry::Anchor;
use crate::compile::hierarchy::{GatePath, LoweredHierarchy, PortBinding};
use crate::compile::routing::{DelayedOwner, TerminalRecord};
use crate::compile::topology::{GateKind, Library};
use crate::compile::{Gate, Netlist};
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::BlockKind;

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
/// * **A block must not itself contain blocks, and its
///   [`CompiledBlock::lowered`] must be the netlist its candidate was
///   certified against.** Both halves are one requirement: `block_locals`
///   counts every flat gate whose path *starts* with a block instance's
///   name, and `input_is_read` asks that same netlist which of the block's
///   ports anything behind them consumes. A block still holding an
///   unresolved child would be mis-mapped rather than rejected; a block
///   carrying only its own gates while its candidate holds its whole
///   flattening reads as "no gate wants this lever" for every port only a
///   grandchild consumes, and the parent's delivery to a live route is
///   retired. A hierarchy deeper than one level is therefore unioned
///   bottom-up, each level's union becoming the next level's
///   [`CompiledBlock`] *with that level's flattening as its `lowered`* --
///   which is what `hierarchy_api::compile_blocks` does.
pub(crate) struct UnionInput<'a> {
    pub parent: &'a PlannedParent,
    pub blocks: &'a [CompiledBlock],
    /// Flattened lowered netlist of this module.
    pub flat: &'a Netlist,
    /// One per flat gate.
    pub paths: &'a [GatePath],
    pub library: &'a Library,
    /// Accepted Input Seam Absorption choices, applied to the named
    /// instance's clone only, before translation.
    pub seams: &'a [InputSeamChoice],
    /// Accepted Parent Route Repack choices, applied to the parent's own
    /// routes after renumbering and before any block is stamped.
    pub prunes: &'a [ParentRouteChoice],
    /// Accepted Refresh Relocation choices, applied to the parent's own
    /// routes after every `prunes` entry and still before any block is
    /// stamped, so both passes see the same parent-owned cells.
    pub refreshes: &'a [ParentRouteChoice],
}

/// One Input Seam Absorption choice in the sink block's own coordinates:
/// the route-owned repeater at `at` on the block's route out of `input`
/// becomes dust in the stamped clone of `sink_block`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(crate) struct InputSeamChoice {
    pub sink_block: InstanceId,
    pub input: u16,
    pub at: Anchor,
}

/// The block's own route out of primary input `input`.
pub(crate) fn input_route(
    candidate: &ExpandedPhysicalCandidate,
    input: u16,
) -> Option<&RealisedRouteTree> {
    let source = PhysicalEndpointId::PrimaryInput(PortId(u32::from(input)));
    candidate.routes.values().find(|tree| tree.source == source)
}

/// The first route-owned repeater along the tree's branches that is not a
/// branch terminal: the one cell Input Seam Absorption may select.
pub(crate) fn first_internal_repeater(tree: &RealisedRouteTree) -> Option<Anchor> {
    let terminals: BTreeSet<Anchor> = tree.branches.iter().map(|b| b.terminal.at).collect();
    let repeaters: BTreeSet<Anchor> = tree
        .cells
        .iter()
        .filter(|cell| cell.state.kind == BlockKind::Repeater)
        .map(|cell| cell.at)
        .collect();
    tree.branches
        .iter()
        .flat_map(|branch| branch.path.iter().copied())
        .find(|at| repeaters.contains(at) && !terminals.contains(at))
}

/// The parent's delivery terminal into block input `input`, in the parent's
/// own coordinates: the boundary cell Input Seam Absorption keeps.
pub(crate) fn parent_boundary(
    parent: &ExpandedPhysicalCandidate,
    block: InstanceId,
    input: u16,
) -> Option<&TerminalRecord> {
    let target = RouteTarget::Connection(ConnectionId::External {
        instance: block,
        input_index: input,
    });
    parent
        .routes
        .values()
        .flat_map(|tree| &tree.branches)
        .find(|branch| branch.target == target)
        .map(|branch| &branch.terminal)
}

/// Replace the non-terminal route-owned repeater at `at` with dust, then
/// prove every branch through it still carries signal from the retained
/// parent `boundary` terminal, which must itself be a repeater (strength 15
/// into the block's root dust). A dust boundary
/// (`DirectedDustIntoSupport`) has no strength to lend and is refused.
///
/// On `Err` the tree is left modified; callers only ever pass a clone they
/// then discard. On `Ok` every affected branch's `terminal.repeaters` is
/// stale by one: the union's `normalise_routes_and_connections` is the one
/// refresh for every tree it splices or mutates, so nothing is refreshed
/// here.
pub(crate) fn absorb_input_seam(
    tree: &mut RealisedRouteTree,
    at: Anchor,
    boundary: &TerminalRecord,
) -> Result<(), UnionError> {
    if boundary.state.kind != BlockKind::Repeater {
        return Err(UnionError::Incomplete(
            "parent boundary into the block is not a retained repeater",
        ));
    }
    if tree.branches.iter().any(|branch| branch.terminal.at == at) {
        return Err(UnionError::Incomplete("seam anchor is a terminal repeater"));
    }
    let cell = tree
        .cells
        .iter_mut()
        .find(|cell| cell.at == at && cell.state.kind == BlockKind::Repeater)
        .ok_or(UnionError::Incomplete(
            "seam anchor is not a route-owned repeater",
        ))?;
    cell.state = crate::compile::dust();
    let cells: BTreeMap<Anchor, BlockKind> =
        tree.cells.iter().map(|cell| (cell.at, cell.state.kind)).collect();
    let mut affected = tree
        .branches
        .iter()
        .filter(|branch| branch.path.contains(&at))
        .peekable();
    if affected.peek().is_none() {
        return Err(UnionError::Incomplete("seam anchor is on no branch path"));
    }
    for branch in affected {
        if branch.path.last() != Some(&branch.terminal.at) {
            return Err(UnionError::Incomplete("seam branch terminal is off its path"));
        }
        // `root` is `path[0]` (the router records it that way), so the
        // path alone is the whole conductor sequence after the boundary
        // repeater validated above.
        let mut strength = MAX_SIGNAL_STRENGTH;
        for at in &branch.path {
            match cells.get(at) {
                Some(BlockKind::Repeater) => strength = MAX_SIGNAL_STRENGTH,
                Some(BlockKind::RedstoneWire) => {
                    strength -= 1;
                    if strength == 0 {
                        return Err(UnionError::Incomplete("seam absorption starves a branch"));
                    }
                }
                _ => return Err(UnionError::Incomplete("seam branch walks a non-conductor")),
            }
        }
    }
    Ok(())
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
        "block {block:?} has a gate reading input `{port}` but no route out of that lever: the \
         signal the parent delivers would never reach the gate that wants it"
    )]
    UnroutedBlockInput { block: InstanceId, port: String },
    #[error(
        "the parent reads block {block:?} output `{port}` but laid no route away from its lamp: \
         the consumer would be left with nothing driving it"
    )]
    UnroutedBlockOutput { block: InstanceId, port: String },
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
    /// Whether anything INSIDE the block actually consumes input `k` --
    /// read off the block's own lowered netlist, which is the only thing
    /// that can tell "no gate wanted this lever" from "the splice lost the
    /// route". A module may declare a port nothing reads, and the block
    /// compile places its lever anyway, so this is a legal, ordinary shape
    /// rather than a defect.
    input_is_read: Vec<bool>,
    /// Output port names in declared order.
    outputs: Vec<String>,
    /// Whether anything in the PARENT consumes output `q` -- read off the
    /// parent's own planning assignments, the mirror of `input_is_read`.
    output_is_read: Vec<bool>,
}

/// The flat candidate, plus where each of the parent's own routes ended up:
/// pre-union parent [`RouteId`] to the id of the final tree that carries its
/// branches. Most map to themselves; a route out of a block lamp is absorbed
/// by the block's output tree (and a pass-through may hand it on again), so
/// its timing arcs are filed under that final id.
pub(crate) fn union_candidate(
    input: UnionInput<'_>,
) -> Result<(ExpandedPhysicalCandidate, BTreeMap<RouteId, RouteId>), UnionError> {
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
    // Parent routes keep their ids (`route_offset: 0`) and no child cell
    // exists yet, so a prune here can only ever touch parent-owned cells. A
    // route that is gone or has nothing left to prune is a stale descriptor.
    for prune in input.prunes {
        let tree = union
            .routes
            .get_mut(&prune.route)
            .ok_or(UnionError::Incomplete("prune names no parent route"))?;
        if !prune_route(tree) {
            return Err(UnionError::Incomplete("prune leaves the parent route unchanged"));
        }
    }
    // Every prune first, then every refresh: a relocation is proven against
    // the cells the repack leaves behind, never the other way round, so the
    // two passes cannot each claim the same redundant repeater.
    for refresh in input.refreshes {
        let tree = union
            .routes
            .get_mut(&refresh.route)
            .ok_or(UnionError::Incomplete("refresh names no parent route"))?;
        if !relocate_refresh(tree) {
            return Err(UnionError::Incomplete(
                "refresh leaves the parent route unchanged",
            ));
        }
    }

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
        for seam in input.seams.iter().filter(|seam| seam.sink_block == block.id) {
            let tree = candidate
                .routes
                .values_mut()
                .find(|tree| {
                    tree.source == PhysicalEndpointId::PrimaryInput(PortId(u32::from(seam.input)))
                })
                .ok_or(UnionError::Incomplete("seam input has no block route"))?;
            let boundary = parent_boundary(parent_candidate, block.id, seam.input)
                .ok_or(UnionError::Incomplete("seam input has no parent delivery"))?;
            absorb_input_seam(tree, seam.at, boundary)?;
        }
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
        let input_is_read = compiled
            .lowered
            .inputs
            .iter()
            .map(|port| block_reads_input(&compiled.lowered, port))
            .collect();
        let output_is_read = (0..compiled.lowered.outputs.len())
            .map(|index| {
                let node = TopologyNodeId(
                    u16::try_from(index).map_err(|_| UnionError::Incomplete("block port width"))?,
                );
                Ok(parent_reads_block_output(
                    &parent_candidate.instances,
                    block.id,
                    node,
                ))
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
                input_is_read,
                outputs: compiled.lowered.outputs.clone(),
                output_is_read,
            },
            candidate,
        ));
    }

    // ---- 5. Every output splice, then every input splice. ----
    let mut absorbed = BTreeMap::<RouteId, RouteId>::new();
    for (piece, _) in &mut pieces {
        splice_outputs(&mut union.routes, piece, &mut absorbed)?;
    }
    for (piece, _) in &mut pieces {
        splice_inputs(&mut union.routes, piece, &mut absorbed)?;
    }
    let parent_routes = realised_parent_routes(parent_candidate.routes.keys(), &absorbed);

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
    Ok((union, parent_routes))
}

/// Follows each pre-union parent route through `absorbed` (absorbed tree id
/// to the id of the tree that took its branches) to the tree it finally
/// lives in. An absorbed tree leaves the union the moment it is recorded,
/// so every chain ends.
fn realised_parent_routes<'a>(
    parents: impl IntoIterator<Item = &'a RouteId>,
    absorbed: &BTreeMap<RouteId, RouteId>,
) -> BTreeMap<RouteId, RouteId> {
    parents
        .into_iter()
        .map(|&parent| {
            let mut id = parent;
            while let Some(&into) = absorbed.get(&id) {
                id = into;
            }
            (parent, id)
        })
        .collect()
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

/// Does anything inside the block consume its own input port `port`?
///
/// This is the one question that separates a benign unread port from a lost
/// route, and the block's own lowered netlist is the only place that
/// answers it: a gate naming the port as an input reads it, and so does a
/// declared output carrying the port's name (`assign y = a`, whose route out
/// of the lever is the one the output splice consumes). A port no gate names
/// is ordinary hardware description -- an unused pin on an instantiated
/// module -- and the block compile still places its lever, because
/// `automatic_input_ports` is every declared input, unfiltered by consumers.
fn block_reads_input(lowered: &Netlist, port: &str) -> bool {
    lowered
        .gates
        .iter()
        .any(|gate| gate.inputs.iter().any(|input| input == port))
        || lowered.outputs.iter().any(|output| output == port)
}

/// Does anything in the parent consume block output `node`?
///
/// The mirror of [`block_reads_input`], asked of the parent's planning
/// instance graph: every sink the parent has -- a gate input or a declared
/// output -- carries the driver it is fed from, and a block output's driver
/// resolves to `PrimitiveOutput(PrimitiveId { block, node })`. If no sink
/// names it, nothing reads the lamp and the parent lays no route away from
/// it; if some sink does, a missing route is a defect.
fn parent_reads_block_output(
    graph: &InstanceGraph,
    block: InstanceId,
    node: TopologyNodeId,
) -> bool {
    let endpoint = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
        instance: block,
        node,
    });
    graph
        .assignments
        .iter()
        .any(|assignment| endpoint_for_driver(&assignment.driver) == Some(endpoint))
}

/// Retires a delivery branch nothing needs any more, together with the cells
/// and floors that only that branch put on the ground.
///
/// `dropped` must already be out of `tree.branches`. Every anchor still
/// walked by a surviving branch stays; the rest of the dropped branch's path
/// leaves, and a floor whose cell has just left goes with it. Keeping them
/// would be *legal* -- neither `validate_shape` nor
/// `validate_physical_ownership` requires a route cell to lie on any branch,
/// and `RealisedRouteTree::validate` only forbids two cells on one anchor --
/// but it would emit a live spur of dust ending in a repeater that drives
/// nothing, which is exactly the dead end this fix exists to avoid.
fn retire_branch(tree: &mut RealisedRouteTree, dropped: &RealisedRouteBranch) {
    let kept = tree
        .branches
        .iter()
        .flat_map(|branch| branch.path.iter().copied())
        .collect::<BTreeSet<_>>();
    let dead = dropped
        .path
        .iter()
        .copied()
        .filter(|at| !kept.contains(at))
        .collect::<BTreeSet<_>>();
    tree.cells.retain(|cell| !dead.contains(&cell.at));
    tree.floors.retain(|floor| {
        let supported = Anchor {
            y: floor.at.y + 1,
            ..floor.at
        };
        !dead.contains(&supported)
    });
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
///
/// An output the parent never reads is a deliberate case, not a fall-through.
/// A module may declare an output whose instantiation leaves it unconnected,
/// and the parent then lays no route away from that lamp -- so the block's
/// own delivery branch has nowhere to go and is retired, along with the dust
/// and terminal repeater that only it walked. What tells that apart from a
/// route the splice lost is `Piece::output_is_read`, read off the parent's
/// own assignments before any renumbering; if the parent DOES read the lamp
/// and there is still no route, that is [`UnionError::UnroutedBlockOutput`].
fn splice_outputs(
    union_routes: &mut BTreeMap<RouteId, RealisedRouteTree>,
    piece: &mut Piece,
    absorbed: &mut BTreeMap<RouteId, RouteId>,
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
        let Some(parent_id) = parent_id else {
            // Nothing in the parent reads this lamp. Whether that is benign
            // or a lost route is not visible here, so ask the parent's own
            // assignments -- the mirror of the question the input splice
            // asks the block's netlist.
            if piece.output_is_read[index] {
                return Err(UnionError::UnroutedBlockOutput {
                    block: piece.planning,
                    port: port.clone(),
                });
            }
            // A declared output of an instantiated module that the parent
            // leaves unconnected is ordinary hardware description. The lamp
            // was a block boundary and is dropped with the rest of them, and
            // the dust and terminal repeater that fed it go too: keeping
            // them would leave a powered spur driving nothing.
            retire_branch(&mut holder, &ob);
            if !holder.branches.is_empty() {
                union_routes.insert(holder.id, holder);
            }
            continue;
        };
        let parent = union_routes
            .remove(&parent_id)
            .ok_or(UnionError::Incomplete("parent route out of a block"))?;
        absorbed.insert(parent_id, holder.id);
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
        union_routes.insert(holder.id, holder);
    }
    Ok(())
}

/// The parent's route into input `k` swallows the block's route away from
/// the lever: the exact terminal repeater standing on the lever stops being
/// a delivery terminal and becomes an ordinary counted mid-route refresh.
///
/// A block with no route out of input `k` has two quite different causes,
/// and they look identical at this call site, so the decision is made from
/// the block's own netlist (`Piece::input_is_read`) instead:
///
/// * **Nothing inside the block reads that lever.** An unused pin on an
///   instantiated module is ordinary hardware description, and it compiles
///   as a block today -- `automatic_input_ports` places a lever for every
///   declared input whether or not a gate wants it, while the block's
///   routing is grouped from actual consumers only, so there is no route to
///   find. The parent still delivers to that lever, because `route_all`
///   walks `0..block.inputs.len()` unconditionally. The delivery branch is
///   retired here, together with the cells and floors that only it walked;
///   what remains is the same shape a flat compile produces for a primary
///   input no gate reads.
/// * **A gate does read it and the route is missing anyway.** That is a real
///   defect, and it is refused with [`UnionError::UnroutedBlockInput`].
///   Skipping it would be worse than it looks: the parent's delivery branch
///   has already been taken out of its tree (its target names the block's
///   ghost id, so it cannot stay), leaving the parent's wire running into a
///   dead end that nothing downstream is in a position to notice.
fn splice_inputs(
    union_routes: &mut BTreeMap<RouteId, RealisedRouteTree>,
    piece: &mut Piece,
    absorbed: &mut BTreeMap<RouteId, RouteId>,
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

        if !piece.input_is_read[index] {
            // An unused pin on an instantiated module. The block compile
            // placed the lever anyway and the parent routes to every
            // declared input, so the delivery exists with nothing on the
            // far side of it. Retire the branch and the dust that only it
            // walked; what is left is the same shape a flat compile
            // produces for an input no gate reads.
            retire_branch(&mut parent, &pb);
            if !parent.branches.is_empty() {
                union_routes.insert(parent.id, parent);
            }
            continue;
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
        // A pass-through's tree may already carry a parent route absorbed by
        // the output splice; recording this hop keeps that chain resolvable.
        absorbed.insert(inner.id, parent.id);
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
        // Parent boundary repeaters became ordinary mid-route refreshes, and
        // `absorb_input_seam` / `prune_route` turned selected refreshes into dust, so
        // every joined path needs a fresh tick count. This is the union's only
        // refresh; the mutations above rely on it.
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
pub(crate) mod tests {
    use super::*;
    use crate::compile::fragment_synth::seed::{ParentBlocks, SeedInput};
    use crate::compile::routing::{RouteTerminalKind, TerminalRecord};
    use crate::redstone::world::block::Facing;

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

    /// Every port read by the side that faces it, which is the case where a
    /// missing route is a defect rather than a benign unused pin.
    fn a_piece(inputs: Vec<(String, Anchor)>, outputs: Vec<String>) -> Piece {
        Piece {
            planning: InstanceId(7),
            ghost: GHOST,
            span: RouteSpan { from: 10, to: 20 },
            routes: BTreeMap::new(),
            input_is_read: vec![true; inputs.len()],
            inputs,
            output_is_read: vec![true; outputs.len()],
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

        let error = splice_outputs(&mut union_routes, &mut piece, &mut BTreeMap::new())
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

        let error = splice_inputs(&mut union_routes, &mut piece, &mut BTreeMap::new())
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

        splice_inputs(&mut union_routes, &mut piece, &mut BTreeMap::new())
            .expect("the pass-through tree is spliced");

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

    /// The prune stage reads slack off the certified timing graph, whose arcs
    /// name FINAL route ids. The parent's route away from a block lamp is
    /// absorbed by the block's output tree, and a pass-through block then
    /// hands that tree on to the parent's delivery tree, so the parent-route
    /// mapping has to follow the whole chain rather than one hop.
    #[test]
    fn parent_routes_are_mapped_through_output_and_pass_through_splices() {
        let lever = anchor(5);
        let lamp = anchor(8);
        // Parent route 0 leaves the block's lamp (ghost output 0).
        let downstream = a_tree(
            0,
            PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: GHOST,
                node: TopologyNodeId(0),
            }),
            vec![a_branch(
                RouteTarget::DeclaredOutput(PortId(4)),
                lamp,
                &[lamp, anchor(9)],
            )],
        );
        // Parent route 1 delivers primary input 0 to the block's lever.
        let delivery = a_tree(
            1,
            PhysicalEndpointId::PrimaryInput(PortId(0)),
            vec![a_branch(
                RouteTarget::Connection(ConnectionId::External {
                    instance: GHOST,
                    input_index: 0,
                }),
                anchor(0),
                &[lever],
            )],
        );
        // Parent route 2 touches no block and keeps its id.
        let aside = a_tree(
            2,
            PhysicalEndpointId::PrimaryInput(PortId(1)),
            vec![a_branch(
                RouteTarget::DeclaredOutput(PortId(3)),
                anchor(20),
                &[anchor(21)],
            )],
        );
        let mut union_routes = routes_of(vec![downstream, delivery, aside]);
        // The block is a pass-through: its own route 10 runs from the cell
        // east of its lever to its lamp.
        let mut piece = a_piece(vec![("a".to_string(), lever)], vec!["y".to_string()]);
        piece.routes.insert(
            RouteId(10),
            a_tree(
                10,
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                vec![a_branch(
                    RouteTarget::DeclaredOutput(PortId(0)),
                    anchor(6),
                    &[anchor(6), anchor(7)],
                )],
            ),
        );

        let mut absorbed = BTreeMap::new();
        splice_outputs(&mut union_routes, &mut piece, &mut absorbed).expect("output splice");
        assert_eq!(
            absorbed,
            BTreeMap::from([(RouteId(0), RouteId(10))]),
            "the block's output tree absorbed the parent's route away from the lamp"
        );
        splice_inputs(&mut union_routes, &mut piece, &mut absorbed).expect("input splice");
        assert_eq!(
            absorbed,
            BTreeMap::from([(RouteId(0), RouteId(10)), (RouteId(10), RouteId(1))]),
            "the delivery tree then absorbed the pass-through tree"
        );
        assert_eq!(
            union_routes.keys().copied().collect::<Vec<_>>(),
            vec![RouteId(1), RouteId(2)]
        );

        let parents = [RouteId(0), RouteId(1), RouteId(2)];
        assert_eq!(
            realised_parent_routes(&parents, &absorbed),
            BTreeMap::from([
                (RouteId(0), RouteId(1)),
                (RouteId(1), RouteId(1)),
                (RouteId(2), RouteId(2)),
            ]),
            "route 0 resolves through 10 to the surviving tree 1; the rest map to themselves"
        );
    }

    /// One block instance, all the way through: lower the design, compile
    /// the block, plan the parent around it, and union the two.
    fn union_one_block<'a>(
        design: &crate::compile::HierarchicalNetlist,
        block_module: &str,
        library: &'a Library,
        services: crate::compile::fragment_synth::seed::SeedServices<'a>,
    ) -> (
        crate::compile::hierarchy::LoweredHierarchy,
        CompiledBlock,
        PlannedParent,
        Result<ExpandedPhysicalCandidate, UnionError>,
    ) {
        let lowered = crate::compile::hierarchy::lower_hierarchy(design).expect("lowers");
        let netlist = lowered.block_netlist(block_module);
        let block = crate::compile::fragment_synth::blocks::compile_block(
            block_module,
            &netlist,
            services,
        )
        .expect("the block compiles");
        let ordered = vec![block.clone()];
        let top = lowered.top.clone();
        let (planning, owned) = planning_netlist(&lowered, &top, &ordered);
        let graph = InstanceGraph::with_blocks(
            &planning,
            library,
            &owned.iter().map(BlockSpecOwned::as_spec).collect::<Vec<_>>(),
        )
        .expect("the parent graph builds");
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
            &BTreeMap::new(),
        )
        .expect("the parent plans");
        let union = union_candidate(UnionInput {
            parent: &planned,
            blocks: std::slice::from_ref(&block),
            flat: &lowered.flat,
            paths: &lowered.paths,
            library,
            seams: &[],
            prunes: &[],
            refreshes: &[],
        })
        .map(|(union, _)| union);
        (lowered, block, planned, union)
    }

    /// Every anchor the union owns, whoever owns it.
    fn occupied(union: &ExpandedPhysicalCandidate) -> BTreeSet<Anchor> {
        union
            .placements
            .values()
            .flat_map(|placement| placement.blocks.iter())
            .chain(
                union
                    .boundaries
                    .values()
                    .flat_map(|boundary| boundary.blocks.iter()),
            )
            .chain(
                union
                    .routes
                    .values()
                    .flat_map(|route| route.cells.iter().chain(route.floors.iter())),
            )
            .chain(
                union
                    .junctions
                    .values()
                    .flat_map(|junction| junction.cells.iter()),
            )
            .map(|block| block.at)
            .collect()
    }

    /// An input port no gate inside the module reads is ordinary hardware
    /// description, and it compiles as a block today: the block's own
    /// placement puts a lever there anyway, while its routing is grouped
    /// from actual consumers and so lays no route out of it. The parent
    /// nonetheless delivers to every declared input. The union must retire
    /// that delivery -- branch, dust and terminal repeater together -- and
    /// still certify, rather than refuse the whole design.
    #[test]
    fn a_block_input_no_gate_reads_unions_and_certifies_with_no_dead_dust() {
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let mut hb = crate::circuits::hierarchical_builder::HierarchicalNetlistBuilder::new();
        // `ignores_a` declares `a` and never reads it.
        hb.module("ignores_a", &["a", "b"], &["y"], |m| {
            m.gates.nor_named("y", "y", &["b".to_string()]);
        });
        // `n` feeds the unread port AND a parent gate, so the parent's tree
        // out of it survives the retirement with one branch left: the dead
        // cells have to be picked out of a tree that stays.
        hb.module("top", &["x", "p"], &["z", "t"], |m| {
            let n = m.gates.not("x");
            m.instance("u0", "ignores_a", &[("a", &n), ("b", "p"), ("y", "w")]);
            m.gates.nor_named("z", "z", &["w".to_string()]);
            m.gates.nor_named("t", "t", &[n]);
        });
        let design = hb.finish("top");
        let (lowered, block, planned, union) =
            union_one_block(&design, "ignores_a", &library, services);
        let union = union.expect("an unread block input is not a reason to refuse the design");

        // The case really is the one under test: the block routes nothing
        // out of `a`, and the parent delivers to its lever regardless.
        let block_id = planned.candidate.instances.blocks[0].id;
        let offset = planned.block_offsets[&block_id];
        assert!(
            !block.candidate.routes.values().any(|tree| tree.source
                == PhysicalEndpointId::PrimaryInput(PortId(0))),
            "the block must have no route out of the input nothing reads"
        );
        let delivery = planned
            .candidate
            .routes
            .values()
            .flat_map(|route| &route.branches)
            .find(|branch| {
                branch.target
                    == RouteTarget::Connection(ConnectionId::External {
                        instance: block_id,
                        input_index: 0,
                    })
            })
            .expect("the parent routes to every declared block input")
            .clone();
        assert_eq!(delivery.terminal.at, shift(block.inputs["a"].cell, offset));

        // Nothing survives on the retired path: no branch walks it, and no
        // owner is left standing on the cells that only it used.
        let live = union
            .routes
            .values()
            .flat_map(|route| &route.branches)
            .flat_map(|branch| branch.path.iter().copied())
            .collect::<BTreeSet<_>>();
        let standing = occupied(&union);
        for at in &delivery.path {
            if live.contains(at) {
                continue;
            }
            assert!(
                !standing.contains(at),
                "the retired delivery left a block at {at:?} that no branch walks"
            );
        }
        assert!(
            !live.contains(&delivery.terminal.at),
            "the lever's boundary repeater must not survive as a live route cell"
        );

        assert!(union.instances.blocks.is_empty());
        assert_eq!(union.instances.instances.len(), lowered.flat.gates.len());
        assert_eq!(
            union.boundaries.len(),
            lowered.flat.inputs.len() + lowered.flat.outputs.len()
        );
        let certified = crate::compile::fragment_synth::seed::certify_planned(
            union,
            &lowered.flat,
            services,
        )
        .expect("certifies");
        assert!(certified.metrics().quality.observed_settle > 0);
    }

    /// The mirror: a declared block output the parent leaves unconnected.
    /// The block always routes to its own lamp, so the delivery exists with
    /// nothing on the far side; the union retires it, drops the lamp with
    /// the rest of the block's boundaries, and certifies.
    #[test]
    fn a_block_output_the_parent_never_reads_unions_and_certifies() {
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let mut hb = crate::circuits::hierarchical_builder::HierarchicalNetlistBuilder::new();
        crate::circuits::hierarchical_builder::full_adder_module(&mut hb);
        // top(x, b, c) : n = NOT x ; u0 = full_adder(n, b, c) ; z = NOR(sum).
        // `cout` is bound and then never read by anything in `top`.
        hb.module("top", &["x", "b", "c"], &["z"], |m| {
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
        let (lowered, block, planned, union) =
            union_one_block(&design, "full_adder", &library, services);
        let union = union.expect("an unread block output is not a reason to refuse the design");

        // The case really is the one under test: `cout` is block output 1,
        // the block delivers to its lamp, and the parent lays no route away
        // from it.
        assert_eq!(block.lowered.outputs[1], "cout");
        let block_id = planned.candidate.instances.blocks[0].id;
        let offset = planned.block_offsets[&block_id];
        assert!(
            !planned.candidate.routes.values().any(|tree| tree.source
                == PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                    instance: block_id,
                    node: TopologyNodeId(1),
                })),
            "nothing in the parent reads `cout`, so the parent lays no route from its lamp"
        );
        let delivery = block
            .candidate
            .routes
            .values()
            .flat_map(|route| &route.branches)
            .find(|branch| branch.target == RouteTarget::DeclaredOutput(PortId(1)))
            .expect("the block routes every declared output of its own")
            .clone();

        let live = union
            .routes
            .values()
            .flat_map(|route| &route.branches)
            .flat_map(|branch| branch.path.iter().copied())
            .collect::<BTreeSet<_>>();
        let standing = occupied(&union);
        for at in delivery.path.iter().map(|at| shift(*at, offset)) {
            if live.contains(&at) {
                continue;
            }
            assert!(
                !standing.contains(&at),
                "the retired output delivery left a block at {at:?} that no branch walks"
            );
        }
        assert!(
            !standing.contains(&shift(block.outputs["cout"].cell, offset)),
            "the lamp of an output nothing reads must not survive"
        );

        assert!(union.instances.blocks.is_empty());
        assert_eq!(union.instances.instances.len(), lowered.flat.gates.len());
        assert_eq!(
            union.boundaries.len(),
            lowered.flat.inputs.len() + lowered.flat.outputs.len()
        );
        let certified = crate::compile::fragment_synth::seed::certify_planned(
            union,
            &lowered.flat,
            services,
        )
        .expect("certifies");
        assert!(certified.metrics().quality.observed_settle > 0);
    }

    /// The other half of the pair: a block output the parent DOES read, with
    /// no route away from its lamp, is a lost route and is refused.
    #[test]
    fn a_block_output_the_parent_reads_with_no_route_is_refused() {
        let mut union_routes = BTreeMap::new();
        let mut piece = a_piece(Vec::new(), vec!["y".to_string()]);
        piece.routes.insert(
            RouteId(10),
            a_tree(
                10,
                PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                    instance: InstanceId(3),
                    node: TopologyNodeId(0),
                }),
                vec![a_branch(
                    RouteTarget::DeclaredOutput(PortId(0)),
                    anchor(0),
                    &[anchor(1)],
                )],
            ),
        );

        let error = splice_outputs(&mut union_routes, &mut piece, &mut BTreeMap::new())
            .expect_err("the parent reads `y` but has no route from its lamp");
        assert!(
            matches!(&error, UnionError::UnroutedBlockOutput { port, .. } if port == "y"),
            "expected an unrouted-output refusal, got {error}"
        );
    }

    /// `top(x, b, c) : n = NOT x ; u0 = full_adder(n, b, c) ; z = NOR(sum) ;
    /// cout = u0.cout` -- one stamped block with parent glue on both sides.
    pub(crate) fn gate_block_gate_chain() -> crate::compile::HierarchicalNetlist {
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
        hb.finish("top")
    }

    /// gate -> block -> gate, certified as one flat circuit.
    #[test]
    fn a_gate_block_gate_chain_unions_into_one_certified_flat_candidate() {
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let design = gate_block_gate_chain();
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
            &BTreeMap::new(),
        )
        .unwrap();
        let (union, parent_routes) = union_candidate(UnionInput {
            parent: &planned,
            blocks: std::slice::from_ref(&block),
            flat: &lowered.flat,
            paths: &lowered.paths,
            library: &library,
            seams: &[],
            prunes: &[],
            refreshes: &[],
        })
        .expect("unions");
        union.validate_shape().expect("flat shape");
        assert_eq!(
            parent_routes.keys().copied().collect::<Vec<_>>(),
            planned.candidate.routes.keys().copied().collect::<Vec<_>>(),
            "every parent route is mapped"
        );
        for (parent, realised) in &parent_routes {
            assert!(
                union.routes.contains_key(realised),
                "parent route {parent:?} maps to a tree {realised:?} the union no longer has"
            );
        }
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

    /// A block input route: `trunk` dust cells, one repeater, then two
    /// branches of `tail` dust each ending in a terminal repeater.
    pub(crate) fn seam_tree(trunk: usize, tail: usize) -> RealisedRouteTree {
        let cell = |x: i32, z: i32, state: crate::redstone::world::block::BlockState| PlacedBlock {
            at: Anchor { x, y: 0, z },
            state,
        };
        let refresh = trunk as i32;
        let mut cells = (0..refresh).map(|x| cell(x, 0, crate::compile::dust())).collect::<Vec<_>>();
        cells.push(cell(refresh, 0, crate::compile::repeater(Facing::East)));
        let mut branches = Vec::new();
        for z in [0, 1] {
            let mut path = (0..=refresh).map(|x| Anchor { x, y: 0, z: 0 }).collect::<Vec<_>>();
            for x in refresh + 1..=refresh + tail as i32 {
                cells.push(cell(x, z, crate::compile::dust()));
                path.push(Anchor { x, y: 0, z });
            }
            let end = Anchor { x: refresh + tail as i32 + 1, y: 0, z };
            cells.push(cell(end.x, end.z, crate::compile::repeater(Facing::East)));
            path.push(end);
            let mut branch = a_branch(RouteTarget::DeclaredOutput(PortId(z as u32)), path[0], &path);
            branch.terminal.state = crate::compile::repeater(Facing::East);
            branches.push(branch);
        }
        let mut tree = a_tree(0, PhysicalEndpointId::PrimaryInput(PortId(0)), branches);
        tree.cells = cells;
        tree
    }

    /// The parent's delivery terminal on the block's lever, one cell west
    /// of the tree's root dust.
    fn boundary(kind: RouteTerminalKind) -> TerminalRecord {
        let state = match kind {
            RouteTerminalKind::DirectedDustIntoSupport => crate::compile::dust(),
            _ => crate::compile::repeater(Facing::East),
        };
        TerminalRecord {
            sink: RoutedSinkId {
                route: RouteId(9),
                ordinal: 0,
            },
            at: anchor(-1),
            state,
            kind,
            repeaters: 0,
            delayed_owner: None,
        }
    }

    #[test]
    fn seam_absorption_requires_every_branch_to_keep_signal() {
        let repeater = boundary(RouteTerminalKind::RepeaterIntoSupport);
        let mut short = seam_tree(3, 4);
        let at = first_internal_repeater(&short).expect("the mid-route repeater is selectable");
        assert_eq!(at, Anchor { x: 3, y: 0, z: 0 }, "terminal repeaters are never selected");
        absorb_input_seam(&mut short, at, &repeater)
            .expect("3 + 4 dust from a strength-15 boundary keeps signal");
        let replaced = short.cells.iter().find(|cell| cell.at == at).unwrap();
        assert_eq!(replaced.state.kind, BlockKind::RedstoneWire);

        let mut long = seam_tree(3, 12);
        let error = absorb_input_seam(&mut long, at, &repeater).expect_err("3 + 12 dust reaches zero");
        assert!(matches!(error, UnionError::Incomplete(_)), "{error}");

        let mut terminal = seam_tree(3, 4);
        let end = terminal.branches[0].terminal.at;
        absorb_input_seam(&mut terminal, end, &repeater).expect_err("a terminal repeater is refused");
        absorb_input_seam(&mut terminal, Anchor { x: 1, y: 0, z: 0 }, &repeater)
            .expect_err("a dust cell is not a repeater and is refused");

        let mut stale = seam_tree(3, 4);
        let orphan = Anchor { x: 40, y: 0, z: 0 };
        stale.cells.push(PlacedBlock {
            at: orphan,
            state: crate::compile::repeater(Facing::East),
        });
        absorb_input_seam(&mut stale, orphan, &repeater)
            .expect_err("a repeater on no branch path is a stale descriptor");
    }

    /// The strength walk starts from the parent's real boundary state: a
    /// dust delivery has no strength to lend, so nothing is absorbed behind
    /// it even when the walk from a repeater would pass.
    #[test]
    fn seam_absorption_refuses_a_dust_parent_boundary() {
        let mut tree = seam_tree(3, 4);
        let at = first_internal_repeater(&tree).unwrap();
        let error = absorb_input_seam(
            &mut tree,
            at,
            &boundary(RouteTerminalKind::DirectedDustIntoSupport),
        )
        .expect_err("DirectedDustIntoSupport is not a retained repeater");
        assert!(matches!(error, UnionError::Incomplete(_)), "{error}");
        let untouched = tree.cells.iter().find(|cell| cell.at == at).unwrap();
        assert_eq!(
            untouched.state.kind,
            BlockKind::Repeater,
            "the boundary is checked before any cell is rewritten"
        );
        absorb_input_seam(
            &mut seam_tree(3, 4),
            at,
            &boundary(RouteTerminalKind::OutputTerminalRepeater),
        )
        .expect("any repeater terminal is a valid boundary");
    }

    /// The exact `Incomplete` reason a replay refused with, so a test can
    /// hold the union to one deterministic message rather than "some error".
    fn refusal(
        result: Result<(ExpandedPhysicalCandidate, BTreeMap<RouteId, RouteId>), UnionError>,
    ) -> &'static str {
        match result {
            Err(UnionError::Incomplete(reason)) => reason,
            Err(other) => panic!("expected an Incomplete refusal, got {other}"),
            Ok(_) => panic!("expected a refusal; the union accepted the descriptor"),
        }
    }

    /// Every route-owned repeater of `tree` that stands on one of `own`'s
    /// cells: the parent's own window, which is all Refresh Relocation may
    /// touch once the block's trees have been spliced onto the same route.
    fn refreshes_within(tree: &RealisedRouteTree, own: &BTreeSet<Anchor>) -> BTreeSet<Anchor> {
        tree.cells
            .iter()
            .filter(|cell| cell.state.kind == BlockKind::Repeater && own.contains(&cell.at))
            .map(|cell| cell.at)
            .collect()
    }

    /// The union replays every Parent Route Repack choice before any Refresh
    /// Relocation choice, on the renumbered parent clone and before a single
    /// child cell is stamped.
    ///
    /// One parent route of the fixture tells the two orders apart: pruning it
    /// first leaves its relocated refresh on different cells than relocating
    /// it first would. The union's own answer has to be the prune-first one.
    #[test]
    fn refresh_relocation_replays_every_prune_before_any_refresh() {
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let design = gate_block_gate_chain();
        let (lowered, block, planned, unioned) =
            union_one_block(&design, "full_adder", &library, services);
        unioned.expect("the fixture unions with no choices at all");
        let union = |prunes: &[ParentRouteChoice], refreshes: &[ParentRouteChoice]| {
            union_candidate(UnionInput {
                parent: &planned,
                blocks: std::slice::from_ref(&block),
                flat: &lowered.flat,
                paths: &lowered.paths,
                library: &library,
                seams: &[],
                prunes,
                refreshes,
            })
        };

        let (&route, tree) = planned
            .candidate
            .routes
            .iter()
            .find(|(_, tree)| {
                let mut prune_first = (*tree).clone();
                let mut refresh_first = (*tree).clone();
                prune_route(&mut prune_first)
                    && relocate_refresh(&mut prune_first)
                    && relocate_refresh(&mut refresh_first)
                    && prune_route(&mut refresh_first)
                    && prune_first != refresh_first
            })
            .expect("the fixture has a parent route that tells the two orders apart");
        let own: BTreeSet<Anchor> = tree.cells.iter().map(|cell| cell.at).collect();
        let mut prune_first = tree.clone();
        prune_route(&mut prune_first);
        relocate_refresh(&mut prune_first);
        let mut refresh_first = tree.clone();
        relocate_refresh(&mut refresh_first);
        prune_route(&mut refresh_first);
        assert_ne!(
            refreshes_within(&prune_first, &own),
            refreshes_within(&refresh_first, &own),
            "the fixture route must place its refreshes differently under each order"
        );
        assert!(
            refreshes_within(&prune_first, &own).len() < refreshes_within(tree, &own).len(),
            "the two replays together must retire at least one refresh"
        );

        // The control: with no descriptors the parent's own window is
        // exactly what it was planned as, spliced child trees and all.
        let (untouched, mapped) = union(&[], &[]).expect("no descriptors, no mutation");
        assert_eq!(
            refreshes_within(&untouched.routes[&mapped[&route]], &own),
            refreshes_within(tree, &own),
            "an empty replay leaves every planned refresh standing"
        );

        let choice = ParentRouteChoice { route };
        let (both, mapped) = union(&[choice], &[choice]).expect("both replays apply to one route");
        assert_eq!(
            refreshes_within(&both.routes[&mapped[&route]], &own),
            refreshes_within(&prune_first, &own),
            "the union replays every prune before it replays any refresh"
        );

        // The block's own trees exist only after stamping and are numbered
        // past every parent route, so naming one is a stale descriptor --
        // which is what pins the replay ahead of the stamping.
        let stamped = both
            .routes
            .keys()
            .copied()
            .find(|id| !planned.candidate.routes.contains_key(id))
            .expect("the stamped block brings trees of its own");
        assert_eq!(
            refusal(union(&[], &[ParentRouteChoice { route: stamped }])),
            "refresh names no parent route",
            "a refresh is replayed before any child tree exists to name"
        );
    }

    /// Every refusal the refresh replay owes its caller: one deterministic
    /// `Incomplete` reason each, mirroring the prune replay it follows.
    #[test]
    fn refresh_relocation_refuses_a_stale_or_no_op_descriptor() {
        let (library, config) =
            crate::compile::fragment_synth::seed::tests::default_services_parts();
        let services = crate::compile::fragment_synth::seed::tests::services(&library, &config);
        let design = gate_block_gate_chain();
        let (lowered, block, planned, unioned) =
            union_one_block(&design, "full_adder", &library, services);
        unioned.expect("the fixture unions with no choices at all");
        let union = |prunes: &[ParentRouteChoice], refreshes: &[ParentRouteChoice]| {
            union_candidate(UnionInput {
                parent: &planned,
                blocks: std::slice::from_ref(&block),
                flat: &lowered.flat,
                paths: &lowered.paths,
                library: &library,
                seams: &[],
                prunes,
                refreshes,
            })
        };

        let missing = ParentRouteChoice {
            route: RouteId(u32::MAX),
        };
        assert_eq!(
            refusal(union(&[], &[missing])),
            "refresh names no parent route"
        );
        assert_eq!(
            refusal(union(&[missing], &[])),
            "prune names no parent route"
        );
        assert_eq!(
            refusal(union(&[missing], &[missing])),
            "prune names no parent route",
            "with both stages stale the prune refuses first, because it runs first"
        );

        // A route with no relocatable pair is a no-op, and a no-op is a
        // stale descriptor rather than a silently accepted proposal.
        let (&steady, _) = planned
            .candidate
            .routes
            .iter()
            .find(|(_, tree)| !relocate_refresh(&mut (*tree).clone()))
            .expect("the fixture has a parent route no relocation can change");
        assert_eq!(
            refusal(union(&[], &[ParentRouteChoice { route: steady }])),
            "refresh leaves the parent route unchanged"
        );
    }
}
