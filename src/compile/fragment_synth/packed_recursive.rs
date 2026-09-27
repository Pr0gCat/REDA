//! The packed recursion: one rule, applied until the pieces are small enough.
//!
//! A node splits its netlist in half, synthesises each half -- as a leaf if it
//! is at or below the terminal grain, otherwise as another packed node -- then
//! packs, routes and certifies the results with
//! [`synthesise_packed_node`] and hands itself on as one
//! [`FreeLeafArtifact`] through [`into_parent_connectable`].  Because that
//! conversion exists, every level speaks the same two types and the recursion
//! has no shape of its own.
//!
//! This is the shipping path for an **unpinned** root above the direct-leaf
//! grain: `recursive::compile` cuts over to [`synthesise_packed_recursive`]
//! and hands the result back through [`adapt_packed_root`].  A pinned root
//! still takes the allocating path, because packed ports are inherited child
//! cells and cannot satisfy a caller's pin row.  The packed path allocates
//! nothing, composes nothing through a corridor and negotiates no child
//! extent.  If a level cannot be built it refuses with the chunk identity and
//! the child index that refused; the one repair it makes is local to that
//! child (see [`synthesise_child`]).  The two constants it shares with the
//! allocating path -- the terminal grain and the halving split -- are imported
//! rather than restated, so the measured rationale for both lives in one place.

// Crate-private until the public synthesis API unfreezes at Gate 3, exactly as
// `leaf`, `packing`, `parent` and `packed_node` are.
#![cfg_attr(not(test), allow(dead_code))]

use thiserror::Error;

use std::collections::BTreeMap;

use crate::compile::fragment_synth::allocation::SignalContract;
use crate::compile::fragment_synth::attribution::{
    LeafDiagnostic, RecursiveDiagnostics, TrunkSummary,
};
use crate::compile::fragment_synth::certification::{
    certify_root_world, run_indexed, CandidateCertificationError, CertificationWorkers,
};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::leaf::{
    synthesise_free_leaf, FreeLeafArtifact, FreeLeafError, GateMetadata, LEAF_PITCHES,
};
use crate::compile::fragment_synth::packed_node::{
    into_parent_connectable, synthesise_packed_node, synthesise_packed_node_pinned,
    synthesise_packed_node_fabric, PackedNode, PackedNodeError,
};
use crate::compile::fragment_synth::partition::{
    node_chunk_id, partition, Chunk, ChunkId, PartitionError,
};
use crate::compile::fragment_synth::recursive::{
    assemble_product, split_of, RecursiveProduct, RootAssembly, TERMINAL_GATES,
};
use crate::compile::geometry::Anchor;
use crate::compile::planner::{PortPlacements, PortRole};
use crate::compile::routing::PhysicalRouter;
use crate::compile::topology::SignalPolarity;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// The largest netlist this driver hands straight to the leaf router.
///
/// Production is [`TERMINAL_GATES`], whose measurements live with it in
/// `recursive`. It is a newtype rather than a bare number so that the only
/// other value it can take is the one a test seam supplies: there is no
/// setter, no configuration field and no environment variable, because a grain
/// a caller can turn is a tuning knob and this is a measured constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PackedGrain(usize);

impl PackedGrain {
    /// The grain `recursive::compile` builds every unpinned root at.
    pub(crate) fn production() -> Self {
        Self(TERMINAL_GATES)
    }

    /// A smaller grain, for tests that need real recursion without paying for
    /// a netlist of hundreds of gates.
    #[cfg(test)]
    pub(crate) fn test_seam(gates: usize) -> Self {
        Self(gates.max(1))
    }
}

/// One completed packed recursion.
#[derive(Debug, Clone)]
pub(crate) struct PackedRecursiveProduct {
    /// Every leaf this synthesis built, at any depth, in child order.
    pub leaves: Vec<LeafDiagnostic>,
    /// The root node: its world, trunks, ports and certificate.
    pub node: PackedNode,
    /// The same root, as the one child an enclosing level would pack.
    pub artifact: FreeLeafArtifact,
    /// Contract levels this synthesis used: a leaf is one, and a node is one
    /// more than its deepest child. A root over leaves is therefore two.
    pub depth: usize,
    /// The most sibling workers any single level actually spawned -- the count
    /// used, not the one asked for, so a level with one child cannot make a
    /// serial run look parallel.
    pub peak_workers: usize,
}

impl PackedRecursiveProduct {
    /// Every gate of the root netlist, in root-world coordinates.
    pub fn gates(&self) -> &GateMetadata {
        &self.node.gates
    }
}

#[derive(Debug, Error)]
pub(crate) enum PackedRecursiveError {
    #[error(transparent)]
    Partition(#[from] PartitionError),
    #[error("node {chunk:?} has no gates to synthesise")]
    NoGates { chunk: ChunkId },
    #[error("node {chunk:?} split into a child of the same {gates} gates")]
    NoProgress { chunk: ChunkId, gates: usize },
    /// A one-gate leaf the router refused. Nothing smaller exists to split it
    /// into, so this is where a refusal stops descending; a larger leaf that
    /// refuses is repaired by splitting instead and never reaches here.
    #[error("terminal child {index} ({chunk:?}) of {parent:?} refused its contract: {error}")]
    ChildRefused {
        index: usize,
        chunk: ChunkId,
        parent: ChunkId,
        #[source]
        error: FreeLeafError,
    },
    /// A leaf the router refused, whose split repair then failed before it
    /// reached one gate. Both halves of the story are kept typed: the refusal
    /// that started the repair, and the error that ended it, which is the
    /// source. The index, chunk and parent are the refused child's own, so the
    /// lineage a caller reads is the one that was asked for, not the one the
    /// repair descended into.
    #[error("child {index} ({chunk:?}) of {parent:?} refused its contract ({refusal}) and its split repair failed: {error}")]
    RepairFailed {
        index: usize,
        chunk: ChunkId,
        parent: ChunkId,
        refusal: FreeLeafError,
        #[source]
        error: Box<PackedRecursiveError>,
    },
    #[error("node {chunk:?} could not be packed: {error}")]
    Packed {
        chunk: ChunkId,
        #[source]
        error: PackedNodeError,
    },
}

/// The one contract every leaf in a packed tree is built to.
///
/// It must be the same at every level: a trunk joins a source interface to a
/// sink interface only when their contracts agree, and a node's ports inherit
/// the contract of the cell behind them, so one leaf built to a different
/// budget would refuse at whatever level the two first met. The delay budget
/// is the search configuration's own per-transition tick cap rather than a
/// number chosen here.
fn packed_contract(search: &SearchConfig) -> SignalContract {
    SignalContract {
        polarity: SignalPolarity::Positive,
        strength: MAX_SIGNAL_STRENGTH,
        delay_budget_ticks: u32::try_from(search.max_game_ticks_per_transition).unwrap_or(u32::MAX),
    }
}

/// Synthesise `netlist` as one packed node under `parent`, recursing until
/// every piece is at or below the production grain.
///
/// This is what `recursive::compile` calls for an unpinned root above the
/// direct-leaf grain.
pub(crate) fn synthesise_packed_recursive<R: PhysicalRouter + Sync>(
    netlist: &Netlist,
    parent: &ChunkId,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
) -> Result<PackedRecursiveProduct, PackedRecursiveError> {
    synthesise_packed_recursive_on(
        netlist,
        parent,
        router,
        search,
        certification,
        workers,
        &LEAF_PITCHES,
    )
}

/// [`synthesise_packed_recursive`] with every leaf placed on the first of
/// `pitches` it certifies on ([`synthesise_free_leaf`]).
pub(crate) fn synthesise_packed_recursive_on<R: PhysicalRouter + Sync>(
    netlist: &Netlist,
    parent: &ChunkId,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
    pitches: &[i32],
) -> Result<PackedRecursiveProduct, PackedRecursiveError> {
    synthesise_packed_recursive_using(
        netlist,
        parent,
        router,
        search,
        certification,
        workers,
        PackedGrain::production(),
        &|chunk, contract, search| synthesise_free_leaf(chunk, contract, search, pitches),
    )
}

/// [`synthesise_packed_recursive`] at an explicit grain.
pub(crate) fn synthesise_packed_recursive_with<R: PhysicalRouter + Sync>(
    netlist: &Netlist,
    parent: &ChunkId,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
    grain: PackedGrain,
) -> Result<PackedRecursiveProduct, PackedRecursiveError> {
    synthesise_packed_recursive_using(
        netlist,
        parent,
        router,
        search,
        certification,
        workers,
        grain,
        &|chunk, contract, search| synthesise_free_leaf(chunk, contract, search, &LEAF_PITCHES),
    )
}

/// What builds a terminal leaf. Production has exactly one:
/// [`synthesise_free_leaf`]. The indirection exists so a test can stand a
/// refusing builder in its place and watch the repair below run; nothing
/// outside this module can name the type, so nothing outside can vary it.
type LeafBuilder<'a> = dyn Fn(&Chunk, SignalContract, &SearchConfig) -> Result<FreeLeafArtifact, FreeLeafError>
    + Sync
    + 'a;

/// [`synthesise_packed_recursive`] for a root whose caller pinned its
/// ports: its leaves are packed directly under the root, inside the room its
/// pins draw ([`synthesise_packed_node_pinned`]).
///
/// A room narrower than a leaf cannot take it however the leaves are laid, so
/// the leaves are rebuilt finer -- the grain halved from the production one --
/// until each fits, or a grain of one gate still does not. Only a shortfall of
/// room moves to the next grain; any other refusal is final.
pub(crate) fn synthesise_packed_recursive_pinned<R: PhysicalRouter + Sync>(
    netlist: &Netlist,
    parent: &ChunkId,
    pins: &PortPlacements,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
    pitches: &[i32],
) -> Result<PackedRecursiveProduct, PackedRecursiveError> {
    let chunk = node_chunk_id(netlist, parent)?;
    if netlist.gates.is_empty() {
        return Err(PackedRecursiveError::NoGates { chunk });
    }
    let contract = packed_contract(search);
    let mut grain = TERMINAL_GATES;
    loop {
        let trace = std::env::var_os("REDA_TRACE_PINNED").is_some();
        let siblings = CertificationWorkers::bounded(workers);
        let artifacts = flat_leaves(netlist, &chunk, grain, contract, search, siblings, pitches)?;
        if trace {
            eprintln!("reda: pinned root at grain {grain}: {} leaves", artifacts.len());
        }
        let packed = synthesise_packed_node_pinned(
            netlist,
            &artifacts,
            pins,
            router,
            search,
            certification,
            siblings,
        );
        let short = matches!(
            &packed,
            Err(PackedNodeError::PinnedSpaceShort { .. })
                | Err(PackedNodeError::PinnedRegionTooSmall { .. })
        ) || matches!(
            &packed,
            Err(PackedNodeError::LayoutsExhausted { rank_zero, .. })
                if matches!(**rank_zero, PackedNodeError::PinnedRegionTooSmall { .. })
        );
        if trace {
            if let Err(error) = &packed {
                eprintln!("reda: pinned root at grain {grain} refused: {error}");
            }
        }
        if short && grain > 1 {
            grain /= 2;
            continue;
        }
        let node = packed.map_err(|error| PackedRecursiveError::Packed {
            chunk: chunk.clone(),
            error,
        })?;
        let artifact = into_parent_connectable(&node, parent).map_err(|error| {
            PackedRecursiveError::Packed {
                chunk: chunk.clone(),
                error,
            }
        })?;
        return Ok(PackedRecursiveProduct {
            leaves: artifacts
                .iter()
                .map(|leaf| LeafDiagnostic {
                    chunk: leaf.chunk.clone(),
                    gates: leaf.netlist.gates.iter().map(|gate| gate.output.clone()).collect(),
                })
                .collect(),
            node,
            artifact,
            depth: 2,
            peak_workers: siblings.count().min(artifacts.len()),
        });
    }
}

/// Every leaf of `netlist` at `grain`, each under the chunk that split it so
/// it keeps the boundary its own split gave it, built in parallel in chunk
/// order.
fn flat_leaves(
    netlist: &Netlist,
    chunk: &ChunkId,
    grain: usize,
    contract: SignalContract,
    search: &SearchConfig,
    siblings: CertificationWorkers,
    pitches: &[i32],
) -> Result<Vec<FreeLeafArtifact>, PackedRecursiveError> {
    let mut leaves = Vec::new();
    let mut pending = vec![(chunk.clone(), netlist.clone())];
    while let Some((id, net)) = pending.pop() {
        for part in partition(&net, &id, split_of(&net))? {
            if part.netlist.gates.len() <= grain {
                leaves.push(part);
            } else {
                pending.push((part.id.clone(), part.netlist.clone()));
            }
        }
    }
    leaves.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(run_indexed(siblings, leaves.len(), |index| {
        build_leaf_finer(&leaves[index], contract, search, pitches)
    })?
    .into_iter()
    .flatten()
    .collect())
}

/// [`synthesise_packed_recursive`] on the lid fabric: the whole netlist cut
/// once into leaves at the production grain, all of them packed flat under one
/// node whose trunks are planned onto fixed layers ([`super::fabric`]). Its
/// height does not grow with trunk count, nesting depth or netlist size.
#[allow(clippy::too_many_arguments)]
pub(crate) fn synthesise_packed_recursive_fabric<R: PhysicalRouter + Sync>(
    netlist: &Netlist,
    parent: &ChunkId,
    pins: Option<&PortPlacements>,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
    pitches: &[i32],
) -> Result<PackedRecursiveProduct, PackedRecursiveError> {
    let chunk = node_chunk_id(netlist, parent)?;
    if netlist.gates.is_empty() {
        return Err(PackedRecursiveError::NoGates { chunk });
    }
    let siblings = CertificationWorkers::bounded(workers);
    // A pinned room narrower than a leaf takes finer leaves, exactly as the
    // pinned packed path does; nothing else changes the grain.
    let mut grain = TERMINAL_GATES;
    let (artifacts, node) = loop {
        let artifacts = flat_leaves(
            netlist,
            &chunk,
            grain,
            packed_contract(search),
            search,
            siblings,
            pitches,
        )?;
        let node = synthesise_packed_node_fabric(
            netlist,
            &artifacts,
            pins,
            router,
            search,
            certification,
            siblings,
        );
        let short = matches!(
            &node,
            Err(PackedNodeError::PinnedSpaceShort { .. })
                | Err(PackedNodeError::PinnedRegionTooSmall { .. })
        ) || matches!(
            &node,
            Err(PackedNodeError::LayoutsExhausted { rank_zero, .. })
                if matches!(**rank_zero, PackedNodeError::PinnedRegionTooSmall { .. })
        );
        if pins.is_some() && short && grain > 1 {
            grain /= 2;
            continue;
        }
        let node = node.map_err(|error| PackedRecursiveError::Packed {
            chunk: chunk.clone(),
            error,
        })?;
        break (artifacts, node);
    };
    let artifact = into_parent_connectable(&node, parent).map_err(|error| {
        PackedRecursiveError::Packed {
            chunk: chunk.clone(),
            error,
        }
    })?;
    Ok(PackedRecursiveProduct {
        leaves: artifacts
            .iter()
            .map(|leaf| LeafDiagnostic {
                chunk: leaf.chunk.clone(),
                gates: leaf.netlist.gates.iter().map(|gate| gate.output.clone()).collect(),
            })
            .collect(),
        node,
        artifact,
        depth: 2,
        peak_workers: siblings.count().min(artifacts.len()),
    })
}

/// One leaf at `chunk`, or -- when the leaf builder refuses it -- the leaves
/// of its halves, down to single gates.
fn build_leaf_finer(
    chunk: &Chunk,
    contract: SignalContract,
    search: &SearchConfig,
    pitches: &[i32],
) -> Result<Vec<FreeLeafArtifact>, PackedRecursiveError> {
    match synthesise_free_leaf(chunk, contract, search, pitches) {
        Ok(leaf) => Ok(vec![leaf]),
        Err(refusal) if chunk.netlist.gates.len() > 1 => {
            let built = partition(&chunk.netlist, &chunk.id, split_of(&chunk.netlist))?
                .iter()
                .map(|half| build_leaf_finer(half, contract, search, pitches))
                .collect::<Result<Vec<_>, _>>();
            built.map(|halves| halves.concat()).map_err(|error| {
                PackedRecursiveError::RepairFailed {
                    index: 0,
                    chunk: chunk.id.clone(),
                    parent: chunk.id.clone(),
                    refusal,
                    error: Box::new(error),
                }
            })
        }
        Err(error) => Err(PackedRecursiveError::ChildRefused {
            index: 0,
            chunk: chunk.id.clone(),
            parent: chunk.id.clone(),
            error,
        }),
    }
}

/// [`synthesise_packed_recursive_with`] with the leaf builder named.
#[allow(clippy::too_many_arguments)]
fn synthesise_packed_recursive_using<R: PhysicalRouter + Sync>(
    netlist: &Netlist,
    parent: &ChunkId,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
    grain: PackedGrain,
    leaf: &LeafBuilder<'_>,
) -> Result<PackedRecursiveProduct, PackedRecursiveError> {
    let chunk = node_chunk_id(netlist, parent)?;
    if netlist.gates.is_empty() {
        return Err(PackedRecursiveError::NoGates { chunk });
    }

    // Stable order before anything is scheduled: the child index a refusal
    // names is an index into this, not into whatever order the partitioner
    // happened to emit or the workers happened to finish in.
    let mut chunks = partition(netlist, &chunk, split_of(netlist))?;
    chunks.sort_by(|left, right| left.id.cmp(&right.id));
    if chunks.is_empty() {
        return Err(PackedRecursiveError::NoGates { chunk });
    }

    // The count that will be used, not the one that was asked for: bounded by
    // the machine, then by the siblings there are to run.
    let siblings = CertificationWorkers::bounded(workers);
    let spawned = siblings.count().min(chunks.len());
    // Each child recurses on a halved budget, as the allocating path's
    // `Session::nested` does, so a tree of levels cannot multiply the machine's
    // cores by its depth. The floor is one: a child always gets to run.
    let nested = (spawned / 2).max(1);
    let outcomes = run_indexed(siblings, chunks.len(), |index| {
        synthesise_child(
            &chunks[index],
            index,
            &chunk,
            netlist.gates.len(),
            router,
            search,
            certification,
            nested,
            grain,
            leaf,
        )
    })?;

    let depth = 1 + outcomes.iter().map(|child| child.depth).max().unwrap_or(0);
    let peak_workers = outcomes
        .iter()
        .map(|child| child.peak_workers)
        .max()
        .unwrap_or(1)
        .max(spawned);
    let mut leaves = Vec::new();
    let artifacts = outcomes
        .into_iter()
        .map(|child| {
            leaves.extend(child.leaves);
            child.artifact
        })
        .collect::<Vec<_>>();

    let refuse = |error| PackedRecursiveError::Packed {
        chunk: chunk.clone(),
        error,
    };
    let node = synthesise_packed_node(
        netlist,
        &artifacts,
        router,
        search,
        certification,
        CertificationWorkers::bounded(workers),
    )
    .map_err(refuse)?;
    let artifact = into_parent_connectable(&node, parent).map_err(refuse)?;

    Ok(PackedRecursiveProduct {
        node,
        artifact,
        depth,
        peak_workers,
        leaves,
    })
}

/// Refusals the adapter raises before it will hand back a product.
#[derive(Debug, Error)]
pub(crate) enum PackedAdapterError {
    #[error("port {signal} was pinned at {pinned:?} but the packed root built it at {built:?}")]
    PinNotHonoured {
        signal: String,
        pinned: Anchor,
        built: Anchor,
    },
    #[error("the packed root has no {role:?} port for {signal}")]
    MissingRootPort { signal: String, role: PortRole },
    #[error("the packed root declares {role:?} port {signal} more than once")]
    DuplicateRootPort { signal: String, role: PortRole },
    #[error("no gate metadata places {gate}")]
    MissingGate { gate: String },
    #[error("gate {gate} is placed more than once")]
    DuplicateGate { gate: String },
    #[error("gate {gate} has a position but no facing")]
    MissingGateFacing { gate: String },
    #[error("harness cell {at:?} for {signal} is outside the built world")]
    HarnessOutOfWorld { signal: String, at: Anchor },
    #[error("harness cell {at:?} for {signal} already holds {kind:?}")]
    HarnessCollision {
        signal: String,
        at: Anchor,
        kind: BlockKind,
    },
    #[error(transparent)]
    Certification(#[from] CandidateCertificationError),
}

/// Turn a certified packed root into the product contract the rest of the
/// compiler already speaks.
///
/// **What this adds is the harness, and nothing else.** A packed root's ports
/// are inherited child caller cells: bare, by design, because a node that is
/// packed into a parent must leave them for the parent to route to. A *root*
/// has no parent, so the cells stay empty forever unless something stands in
/// them -- and every consumer downstream, the benchmark driver included,
/// expects a real lever it can toggle at each declared input and a lamp it can
/// read at each declared output. Nested artifacts are untouched: the harness
/// exists only at the top, in a clone, and the world is re-certified with it
/// in place rather than certified before it and shipped after.
///
/// Pins are refused rather than approximated -- see [`PackedAdapterError`].
pub(crate) fn adapt_packed_root(
    lowered: &Netlist,
    product: &PackedRecursiveProduct,
    pins: Option<&PortPlacements>,
    search: &SearchConfig,
    workers: usize,
) -> Result<RecursiveProduct, PackedAdapterError> {
    // A port the caller pinned stands in the caller's own cell, which ships
    // empty; every other port gets the lever or lamp a root always shipped.
    let caller_pinned = |signal: &str| pins.is_some_and(|pins| pins.get(signal).is_some());

    let mut input_positions = BTreeMap::new();
    let mut output_positions = BTreeMap::new();
    let mut world = product.node.world.clone();
    for (role, signals, positions) in [
        (PortRole::Input, &lowered.inputs, &mut input_positions),
        (PortRole::Output, &lowered.outputs, &mut output_positions),
    ] {
        for signal in signals {
            let port = product
                .node
                .root_ports
                .iter()
                .find(|port| port.port.role == role && port.port.signal == *signal)
                .ok_or_else(|| PackedAdapterError::MissingRootPort {
                    signal: signal.clone(),
                    role,
                })?;
            let at = port.port.pin.at;
            if let Some(pin) = pins.and_then(|pins| pins.get(signal)) {
                if pin.at != at {
                    return Err(PackedAdapterError::PinNotHonoured {
                        signal: signal.clone(),
                        pinned: pin.at,
                        built: at,
                    });
                }
            }
            if positions
                .insert(signal.clone(), (at.x, at.y, at.z))
                .is_some()
            {
                return Err(PackedAdapterError::DuplicateRootPort {
                    signal: signal.clone(),
                    role,
                });
            }
            match role {
                _ if caller_pinned(signal) => {}
                PortRole::Input => install_lever(&mut world, signal, at)?,
                PortRole::Output => {
                    claim_harness_cell(&world, signal, at)?;
                    world.set(at.x, at.y, at.z, compile::lamp());
                }
            }
        }
    }

    // The gate maps, in the shapes the product declares: positions keyed by
    // output signal, facings in the netlist's own gate order.
    let mut gate_output_positions = BTreeMap::new();
    let mut gate_facings = Vec::with_capacity(lowered.gates.len());
    for gate in &lowered.gates {
        let at = product
            .node
            .gates
            .output_positions
            .get(&gate.output)
            .ok_or(PackedAdapterError::MissingGate {
                gate: gate.output.clone(),
            })?;
        if gate_output_positions
            .insert(gate.output.clone(), (at.x, at.y, at.z))
            .is_some()
        {
            return Err(PackedAdapterError::DuplicateGate {
                gate: gate.output.clone(),
            });
        }
        gate_facings.push(*product.node.gates.facings.get(&gate.output).ok_or(
            PackedAdapterError::MissingGateFacing {
                gate: gate.output.clone(),
            },
        )?);
    }

    // The harness is part of the shipped world, so the shipped world is what
    // answers for it. Certifying before it was installed would certify
    // something else.
    let certificate = certify_root_world(
        &world,
        lowered,
        &product
            .node
            .root_ports
            .iter()
            .map(|port| port.port.clone())
            .collect::<Vec<_>>(),
        &CertificationConfig::from_search(search),
        CertificationWorkers::bounded(workers),
    )?;

    let root_trunks = product
        .node
        .trunks
        .iter()
        .zip(&product.node.trunk_signals)
        .zip(&product.node.trunk_lanes)
        .map(|((tree, signal), lane)| TrunkSummary {
            signal: signal.clone(),
            cells: tree.cells.len(),
            floors: tree.floors.len(),
            repeaters: tree
                .cells
                .iter()
                .filter(|block| block.state.kind == BlockKind::Repeater)
                .count(),
            branch_terminal_repeaters: tree
                .branches
                .iter()
                .map(|branch| branch.terminal.repeaters)
                .collect(),
            lane: *lane,
        })
        .collect();
    Ok(assemble_product(
        RootAssembly {
            world,
            trunks: &product.node.trunks,
            input_positions,
            output_positions,
            gate_output_positions,
            gate_facings,
            depth: product.depth,
            peak_workers: product.peak_workers,
            diagnostics: Some(RecursiveDiagnostics {
                leaves: product.leaves.clone(),
                root_trunks,
            }),
        },
        certificate,
        search,
    ))
}

/// Stand a togglable lever in a root input's caller cell.
///
/// The cell itself is the caller's and ships empty, so it is free; the floor
/// beneath it is the minimum support a floor-faced lever needs, and is added
/// only when that cell is empty too. A lever with nothing under it reads
/// correctly in the simulator but pops off as a dropped item the moment the
/// schematic is pasted, which is the failure `compile::lever`'s own note
/// records.
fn install_lever(world: &mut World, signal: &str, at: Anchor) -> Result<(), PackedAdapterError> {
    claim_harness_cell(world, signal, at)?;
    let floor = Anchor { y: at.y - 1, ..at };
    if floor.y >= 0 && world.get(floor.x, floor.y, floor.z).kind == BlockKind::Air {
        world.set(floor.x, floor.y, floor.z, compile::stone());
    }
    world.set(at.x, at.y, at.z, compile::lever(false));
    Ok(())
}

/// A harness cell must exist and be empty; the caller owns it, so anything
/// standing there is a refusal rather than something to overwrite.
fn claim_harness_cell(world: &World, signal: &str, at: Anchor) -> Result<(), PackedAdapterError> {
    if world.index(at.x, at.y, at.z).is_none() {
        return Err(PackedAdapterError::HarnessOutOfWorld {
            signal: signal.to_owned(),
            at,
        });
    }
    let kind = world.get(at.x, at.y, at.z).kind;
    if kind != BlockKind::Air {
        return Err(PackedAdapterError::HarnessCollision {
            signal: signal.to_owned(),
            at,
            kind,
        });
    }
    Ok(())
}

/// What one child of a level contributed.
struct ChildOutcome {
    artifact: FreeLeafArtifact,
    depth: usize,
    peak_workers: usize,
    /// Every leaf under this child, at any depth: a leaf child is one, a
    /// node child is all of its own.
    leaves: Vec<LeafDiagnostic>,
}

/// Build one child of a level.
///
/// A child at or below the grain is handed to the leaf builder. If the builder
/// refuses it and the child has more than one gate, **that child alone** is
/// repaired: it is synthesised as a packed node of its own, which halves it
/// and hands each half back here. Each repair strictly reduces the gate count
/// -- the halving split cannot return a child as large as its parent, and
/// [`PackedRecursiveError::NoProgress`] says so by type before recursing -- so
/// the descent is bounded by the gate count and ends at one gate at the
/// latest. A one-gate leaf has nothing to split into, so its refusal is
/// returned typed, carrying the router's own error as its source; a repair
/// that fails before that returns both the refusal and the failure, typed. The
/// parent is never retried or moved; siblings never see the repair.
#[allow(clippy::too_many_arguments)]
fn synthesise_child<R: PhysicalRouter + Sync>(
    chunk: &Chunk,
    index: usize,
    parent: &ChunkId,
    parent_gates: usize,
    router: &R,
    search: &SearchConfig,
    certification: &CertificationConfig,
    workers: usize,
    grain: PackedGrain,
    leaf: &LeafBuilder<'_>,
) -> Result<ChildOutcome, PackedRecursiveError> {
    let gates = chunk.netlist.gates.len();
    let refusal = if gates <= grain.0 {
        match leaf(chunk, packed_contract(search), search) {
            Ok(artifact) => {
                let leaves = vec![LeafDiagnostic {
                    chunk: chunk.id.clone(),
                    gates: chunk
                        .netlist
                        .gates
                        .iter()
                        .map(|gate| gate.output.clone())
                        .collect(),
                }];
                return Ok(ChildOutcome {
                    artifact,
                    leaves,
                    // A leaf is one contract level: it answered the router
                    // directly.
                    depth: 1,
                    peak_workers: 1,
                });
            }
            Err(error) if gates <= 1 => {
                return Err(PackedRecursiveError::ChildRefused {
                    index,
                    chunk: chunk.id.clone(),
                    parent: parent.clone(),
                    error,
                })
            }
            // A refused leaf with room to split falls through to the same
            // recursion a nonterminal child takes: the repair is the split.
            // The refusal is kept so a repair that fails can say what it was
            // repairing.
            Err(error) => Some(error),
        }
    } else {
        None
    };
    // The halving split strictly shrinks every child of a node with two or
    // more gates, and a one-gate child is always terminal, so this cannot
    // stand still. Saying so by type is cheaper than trusting it.
    if gates >= parent_gates {
        return Err(PackedRecursiveError::NoProgress {
            chunk: chunk.id.clone(),
            gates,
        });
    }
    let product = match synthesise_packed_recursive_using(
        &chunk.netlist,
        parent,
        router,
        search,
        certification,
        workers,
        grain,
        leaf,
    ) {
        Ok(product) => product,
        Err(error) => {
            return Err(match refusal {
                Some(refusal) => PackedRecursiveError::RepairFailed {
                    index,
                    chunk: chunk.id.clone(),
                    parent: parent.clone(),
                    refusal,
                    error: Box::new(error),
                },
                None => error,
            })
        }
    };
    Ok(ChildOutcome {
        artifact: product.artifact,
        depth: product.depth,
        peak_workers: product.peak_workers,
        leaves: product.leaves,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
    use crate::compile::fragment_synth::certification::{
        certify_root_world, CandidateCertificationError,
    };
    use crate::compile::fragment_synth::leaf::{
        interface_route_direction, ParentConnectableInterface,
    };
    use crate::compile::fragment_synth::partition::root_chunk_id;
    use crate::compile::fragment_synth::terminal_geometry::runway_core;
    use crate::compile::planner::PortRole;
    use crate::compile::routing::DurablePhysicalRouter;
    use crate::compile::Gate;
    use crate::redstone::simulator::Simulator;
    use crate::redstone::world::block::BlockKind;

    /// Three inverters in a chain. At the test grain of one gate the halving
    /// split gives a two-gate chunk and a one-gate chunk: the first is
    /// nonterminal and recurses into two leaves, the second goes straight to
    /// the leaf router. The root is therefore a node over a node and a leaf --
    /// three contract levels, with two siblings at each level that has any.
    ///
    /// The gate names are part of the fixture. A [`ChunkId`] is a fingerprint,
    /// packing visits leaves in that order, and the order decides which leaf
    /// is placed first -- so the names decide the placement, and the placement
    /// decides whether the trunk between two leaves can be routed at all. See
    /// `a_four_gate_chain_is_refused_by_the_router`, which records that no
    /// four-gate chain routes today.
    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["f".into()],
            gates: vec![
                Gate::nor("d", &["x"]),
                Gate::nor("e", &["d"]),
                Gate::nor("f", &["e"]),
            ],
        }
    }

    /// `n` inputs, each inverted twice: `2n` gates, so above the grain the
    /// first and second inversions land in different leaves and up to `n`
    /// trunks cross between them.
    fn wide(n: usize) -> Netlist {
        let x = |i: usize| format!("x{i:02}");
        let a = |i: usize| format!("a{i:02}");
        let b = |i: usize| format!("b{i:02}");
        Netlist {
            inputs: (0..n).map(x).collect(),
            outputs: (0..n).map(b).collect(),
            gates: (0..n)
                .flat_map(|i| {
                    [
                        Gate::nor(&a(i), &[x(i).as_str()]),
                        Gate::nor(&b(i), &[a(i).as_str()]),
                    ]
                })
                .collect(),
        }
    }

    /// **The lid fabric's height does not grow with its trunks.** Wider and
    /// wider netlists put more and more trunks between their leaves; every one
    /// builds, certifies, and stands no higher than nine over its tallest
    /// leaf -- the lanes stacked one pitch higher per trunk.
    #[test]
    #[ignore = "measurement: builds 20- and 40-input netlists, about eight minutes"]
    fn the_lid_fabric_height_does_not_grow_with_trunk_count() {
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let mut heights = Vec::new();
        for n in [20, 40] {
            let net = wide(n);
            let root = root_chunk_id(&net).unwrap();
            let product = synthesise_packed_recursive_fabric(
                &net,
                &root,
                None,
                &DurablePhysicalRouter,
                &search,
                &certification,
                4,
                &LEAF_PITCHES,
            )
            .unwrap_or_else(|error| panic!("{n} inputs: {error}"));
            let lid = product.node.packed.halo.iter().map(|at| at.y).max().unwrap();
            let (width, height, depth) = product.node.world.size();
            let top = (0..height)
                .rev()
                .find(|&y| {
                    (0..width).any(|x| {
                        (0..depth).any(|z| product.node.world.get(x, y, z).kind != BlockKind::Air)
                    })
                })
                .unwrap();
            assert!(top <= lid + 8, "{n} inputs: top {top} over lid {lid}");
            assert!(product.node.trunks.len() >= n / 2, "{n} inputs: too few trunks to measure");
            heights.push(top - lid);
        }
        assert_eq!(heights[0], heights[1], "the fabric's height moved with its trunks");
    }

    /// The fabric is the same world at one worker and at eight.
    #[test]
    #[ignore = "measurement: builds a 20-input netlist twice, about eight minutes"]
    fn the_lid_fabric_is_worker_invariant() {
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let net = wide(20);
        let root = root_chunk_id(&net).unwrap();
        let build = |workers| {
            synthesise_packed_recursive_fabric(
                &net,
                &root,
                None,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                &LEAF_PITCHES,
            )
            .unwrap()
        };
        let (one, many) = (build(1), build(8));
        assert!(many.peak_workers > 1);
        assert_eq!(
            canonical_world_fingerprint(&one.node.world),
            canonical_world_fingerprint(&many.node.world)
        );
    }

    /// Every child that drives a boundary signal stands west of every child
    /// that reads it -- the placement evidence that dataflow order held.
    fn assert_drivers_precede_readers(node: &PackedNode) {
        for (chunk, leaf) in &node.packed.placements {
            for driven in leaf
                .interfaces
                .values()
                .filter(|interface| interface.role == PortRole::Output)
            {
                for (other, reader) in &node.packed.placements {
                    if other == chunk {
                        continue;
                    }
                    let reads = reader.interfaces.values().any(|interface| {
                        interface.role == PortRole::Input && interface.signal == driven.signal
                    });
                    assert!(
                        !reads || leaf.translation.x < reader.translation.x,
                        "{:?} drives {} but was placed east of its reader {:?}",
                        chunk,
                        driven.signal,
                        other
                    );
                }
            }
        }
    }

    /// **The four-gate chain, which used to be unbuildable.**
    ///
    /// Before children were placed in dataflow order this refused: the two
    /// leaves of an inner node were laid out in fingerprint order, so about
    /// half the time the reader stood west of its driver and the two forced
    /// runways pointed away from each other. All twenty-one four-gate chains
    /// the `a..z` names give now build, every one of them at rank zero.
    #[test]
    fn a_four_gate_chain_builds_and_certifies_with_drivers_placed_first() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["d".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["b"]),
                Gate::nor("d", &["c"]),
            ],
        };
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let build = |workers: usize| {
            synthesise_packed_recursive_with(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(1),
            )
            .expect("a four-gate chain builds once its children are ordered by dataflow")
        };

        let serial = build(1);
        assert_eq!(serial.depth, 3, "two halves, each split again");
        assert!(
            serial.node.layout_rank > 0,
            "portal ranking preserves the selected candidate's original rank"
        );
        let selected_score = serial
            .node
            .candidate_scores
            .iter()
            .find(|row| row.original_rank == serial.node.layout_rank)
            .expect("the selected original rank is in the candidate score table")
            .score
            .clone();
        assert_drivers_precede_readers(&serial.node);
        assert!(!serial.node.certificate.measurements.is_empty());
        assert_eq!(
            serial.node.certificate.world_fingerprint,
            canonical_world_fingerprint(&serial.node.world)
        );

        // Non-vacuous: cut the root's inter-node trunk and the same authority
        // refuses the same netlist.
        assert_eq!(serial.node.trunks.len(), 1, "one signal crosses the root");
        let mut cut = serial.node.world.clone();
        for block in serial.node.trunks[0]
            .cells
            .iter()
            .chain(&serial.node.trunks[0].floors)
        {
            cut.set(
                block.at.x,
                block.at.y,
                block.at.z,
                crate::redstone::world::block::BlockState::air(),
            );
        }
        let refusal = certify_root_world(
            &cut,
            &net,
            &serial
                .node
                .root_ports
                .iter()
                .map(|port| port.port.clone())
                .collect::<Vec<_>>(),
            &certification,
            CertificationWorkers::serial(),
        )
        .expect_err("a cut trunk does not compute the chain");
        assert!(
            matches!(
                refusal,
                CandidateCertificationError::FunctionalMismatch { .. }
            ),
            "unexpected refusal: {refusal}"
        );

        // Siblings in flight change the schedule, not the answer.
        let parallel = build(4);
        assert_eq!(parallel.peak_workers, peak_for(4, 2));
        assert_eq!(parallel.node.fingerprint, serial.node.fingerprint);
        assert_eq!(parallel.node.certificate, serial.node.certificate);
        assert_eq!(parallel.node.root_ports, serial.node.root_ports);
        assert_eq!(parallel.node.trunks, serial.node.trunks);
        assert_eq!(parallel.node.gates, serial.node.gates);
        assert_eq!(parallel.node.layout_rank, serial.node.layout_rank);
        assert_eq!(parallel.node.candidate_scores, serial.node.candidate_scores);
        assert_eq!(
            parallel
                .node
                .candidate_scores
                .iter()
                .find(|row| row.original_rank == parallel.node.layout_rank)
                .expect("the parallel selected rank is in the score table")
                .score,
            selected_score
        );
    }

    /// **The adapter: a packed root becomes a product with a real harness.**
    ///
    /// A packed root ships its ports bare, because a node that will be packed
    /// again must. A root will not be, so the adapter stands a lever in each
    /// declared input and a lamp in each declared output, and re-certifies the
    /// world with them in it. The lever is then toggled the way the benchmark
    /// driver toggles one -- `state.lit = bit`, nothing else -- to prove it
    /// really drives the inherited child handover.
    #[test]
    fn a_packed_root_adapts_to_a_levered_certified_product() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["d".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["b"]),
                Gate::nor("d", &["c"]),
            ],
        };
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let build = |workers: usize| {
            let packed = synthesise_packed_recursive_with(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(1),
            )
            .expect("the four-gate chain packs");
            adapt_packed_root(&net, &packed, None, &search, workers)
                .expect("an unpinned packed root adapts")
        };

        let product = build(1);

        // The harness is there, and it is hardware rather than empty space.
        assert_eq!(product.input_positions.len(), 1);
        assert_eq!(product.output_positions.len(), 1);
        for at in product.input_positions.values() {
            let block = product.world.get(at.0, at.1, at.2);
            assert_eq!(block.kind, BlockKind::Lever, "an input must be togglable");
            assert!(!block.lit, "a shipped lever starts off");
        }
        for at in product.output_positions.values() {
            assert_eq!(
                product.world.get(at.0, at.1, at.2).kind,
                BlockKind::Lamp,
                "an output must be readable"
            );
        }

        // `d` is four inversions of `x`, so it follows it. Driven exactly as
        // the benchmark driver drives an unpinned input.
        let input = product.input_positions["x"];
        let output = product.output_positions["d"];
        for bit in [false, true] {
            let mut world = product.world.clone();
            let mut lever = world.get(input.0, input.1, input.2).clone();
            assert_eq!(lever.kind, BlockKind::Lever);
            lever.lit = bit;
            world.set(input.0, input.1, input.2, lever);
            let mut simulator = Simulator::new(world);
            simulator.run_until_stable(400).expect("the chain settles");
            assert_eq!(
                simulator.world().get(output.0, output.1, output.2).lit,
                bit,
                "the lever must drive the chain through to the lamp"
            );
        }

        // The certificate is this world's, harness included.
        assert_eq!(
            product.metrics.emitted_world_fingerprint,
            canonical_world_fingerprint(&product.world)
        );
        assert_eq!(
            product.metrics.candidate_fingerprint,
            product.candidate_fingerprint
        );
        assert!(product.metrics.transition_count > 0);

        // Every declared field is complete.
        assert_eq!(product.gate_facings.len(), net.gates.len());
        assert_eq!(
            product.gate_output_positions.keys().collect::<Vec<_>>(),
            net.gates
                .iter()
                .map(|gate| &gate.output)
                .collect::<Vec<_>>()
        );
        assert_eq!(product.depth, 3);
        assert_eq!(product.peak_workers, 1);

        // Worker count is not an input to any of it.
        let parallel = build(4);
        assert_eq!(
            parallel.candidate_fingerprint,
            product.candidate_fingerprint
        );
        assert_eq!(parallel.metrics, product.metrics);
        assert_eq!(parallel.input_positions, product.input_positions);
        assert_eq!(parallel.output_positions, product.output_positions);
        assert_eq!(
            parallel.gate_output_positions,
            product.gate_output_positions
        );
        assert_eq!(parallel.gate_facings, product.gate_facings);
        assert_eq!(parallel.depth, product.depth);
        assert_eq!(parallel.peak_workers, peak_for(4, 2));
    }

    /// Build `net` at the **production** grain and adapt it, then read the
    /// truth at the shipped lever and lamp cells.
    fn adapt_at_production_grain(net: &Netlist, workers: usize) -> RecursiveProduct {
        let context = root_chunk_id(net).unwrap();
        let search = SearchConfig::checked_defaults();
        let packed = synthesise_packed_recursive(
            net,
            &context,
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            workers,
        )
        .expect("the netlist packs at the production grain");
        adapt_packed_root(net, &packed, None, &search, workers).expect("the packed root adapts")
    }

    /// Drive every declared input from its lever and read every declared
    /// output from its lamp, in the shipped world.
    fn observe(product: &RecursiveProduct, inputs: &[(&str, bool)]) -> BTreeMap<String, bool> {
        let mut world = product.world.clone();
        for (signal, bit) in inputs {
            let at = product.input_positions[*signal];
            let mut lever = world.get(at.0, at.1, at.2).clone();
            assert_eq!(lever.kind, BlockKind::Lever, "{signal} must be a lever");
            lever.lit = *bit;
            world.set(at.0, at.1, at.2, lever);
        }
        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(600)
            .expect("the circuit settles");
        product
            .output_positions
            .iter()
            .map(|(signal, at)| (signal.clone(), simulator.world().get(at.0, at.1, at.2).lit))
            .collect()
    }

    /// Fanout at the production grain: one gate drives two readers, and the
    /// split puts the driver and one reader in different leaves.
    #[test]
    fn a_fanout_node_builds_and_simulates_at_the_production_grain() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["p".into(), "q".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("p", &["a"]),
                Gate::nor("q", &["a"]),
            ],
        };
        let product = adapt_at_production_grain(&net, 1);
        assert_eq!(product.gate_facings.len(), 3);
        for bit in [false, true] {
            let read = observe(&product, &[("x", bit)]);
            // Two inversions each, so both outputs follow `x`.
            assert_eq!(read["p"], bit);
            assert_eq!(read["q"], bit);
        }
    }

    /// A chain at the production grain: two two-gate leaves and one trunk.
    /// The layout repair earns its keep here -- rank zero is refused and a
    /// later layout is the one that routes.
    #[test]
    fn a_chain_builds_and_simulates_at_the_production_grain() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["d".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["b"]),
                Gate::nor("d", &["c"]),
            ],
        };
        let product = adapt_at_production_grain(&net, 1);
        assert_eq!(product.gate_facings.len(), 4);
        assert_eq!(product.depth, 2, "two leaves under one root");
        for bit in [false, true] {
            // Four inversions: `d` follows `x`.
            assert_eq!(observe(&product, &[("x", bit)])["d"], bit);
        }
    }

    /// Reconvergence at the production grain: two branches split from one
    /// gate and rejoin at another, so two boundary signals cross the same
    /// leaf seam. Before packed trunks had lanes this had no second route at
    /// all.
    #[test]
    fn a_reconvergent_node_builds_and_simulates_at_the_production_grain() {
        let net = reconvergent(&[
            ("a", vec!["x"]),
            ("b", vec!["a"]),
            ("c", vec!["a"]),
            ("z", vec!["b", "c"]),
        ]);
        let product = adapt_at_production_grain(&net, 1);
        assert_eq!(product.gate_facings.len(), 4);
        for bit in [false, true] {
            // a = !x, b = c = x, z = !(b | c) = !x.
            assert_eq!(observe(&product, &[("x", bit)])["z"], !bit);
        }
    }

    /// **Split operands at the production grain.**
    ///
    /// A two-input gate whose operands are split across the seam needs one
    /// trunk per operand. This refused for as long as the only candidate
    /// placements were the legacy corner abutments, which are chosen without
    /// reference to the signals that have to cross; it builds now because the
    /// ranked search also offers the placement that puts the two terminals of
    /// a boundary signal mouth to mouth.
    #[test]
    fn split_operands_build_and_simulate_at_the_production_grain() {
        let net = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["z".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["y"]),
                Gate::nor("z", &["a", "b"]),
            ],
        };
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let packed = synthesise_packed_recursive(
            &net,
            &context,
            &DurablePhysicalRouter,
            &search,
            &CertificationConfig::from_search(&search),
            1,
        )
        .expect("both operand trunks route once a bridge placement is offered");
        assert!(
            packed.node.layout_rank > 0,
            "a legacy layout would have done, so the bridge proved nothing"
        );
        assert_eq!(packed.node.trunks.len(), 2, "one trunk per operand");

        // The winning layout really is a bridge: for each boundary signal the
        // two terminals face each other with their runway mouths adjacent.
        for (signal, _) in [("a", ()), ("b", ())] {
            let ends = packed
                .node
                .packed
                .placements
                .values()
                .flat_map(|leaf| leaf.interfaces.values())
                .filter(|interface| interface.signal == signal)
                .collect::<Vec<_>>();
            assert_eq!(ends.len(), 2, "{signal} joins exactly two terminals");
            let mouth = |interface: &ParentConnectableInterface| {
                *runway_core(interface.pin.at, interface_route_direction(interface))
                    .last()
                    .expect("a runway has a mouth")
            };
            let (first, second) = (mouth(ends[0]), mouth(ends[1]));
            let apart = first.x.abs_diff(second.x)
                + first.y.abs_diff(second.y)
                + first.z.abs_diff(second.z);
            assert_eq!(apart, 1, "{signal}'s runway mouths are not adjacent");
        }

        // The whole truth table, read at the shipped lever and lamp.
        let product =
            adapt_packed_root(&net, &packed, None, &search, 1).expect("the packed root adapts");
        for x in [false, true] {
            for y in [false, true] {
                // a = !x, b = !y, z = !(!x | !y) = x AND y.
                assert_eq!(
                    observe(&product, &[("x", x), ("y", y)])["z"],
                    x && y,
                    "x={x} y={y}"
                );
            }
        }
        assert_eq!(
            product.metrics.emitted_world_fingerprint,
            canonical_world_fingerprint(&product.world)
        );

        // The two trunks stay out of each other's coupling reach.
        let mut closest = u32::MAX;
        for first in &packed.node.trunks[0].cells {
            for second in &packed.node.trunks[1].cells {
                closest = closest.min(
                    first.at.x.abs_diff(second.at.x)
                        + first.at.y.abs_diff(second.at.y)
                        + first.at.z.abs_diff(second.at.z),
                );
            }
        }
        assert!(closest > 2, "the trunks come within {closest}");

        // Both operands are required, so cutting either trunk is visible.
        for cut_index in 0..packed.node.trunks.len() {
            let mut cut = packed.node.world.clone();
            for block in packed.node.trunks[cut_index]
                .cells
                .iter()
                .chain(&packed.node.trunks[cut_index].floors)
            {
                cut.set(
                    block.at.x,
                    block.at.y,
                    block.at.z,
                    crate::redstone::world::block::BlockState::air(),
                );
            }
            let refusal = certify_root_world(
                &cut,
                &net,
                &packed
                    .node
                    .root_ports
                    .iter()
                    .map(|port| port.port.clone())
                    .collect::<Vec<_>>(),
                &CertificationConfig::from_search(&search),
                CertificationWorkers::serial(),
            )
            .expect_err("a cut operand trunk does not compute the netlist");
            assert!(
                matches!(
                    refusal,
                    CandidateCertificationError::FunctionalMismatch { .. }
                ),
                "cutting trunk {cut_index} gave {refusal}"
            );
        }
    }

    /// The same circuit, declared in another order, is the same circuit:
    /// identity comes from the netlist, not from the vector.
    #[test]
    fn declaration_order_does_not_change_a_production_grain_root() {
        let forward = fanout(&[("a", vec!["x"]), ("p", vec!["a"]), ("q", vec!["a"])]);
        let shuffled = fanout(&[("q", vec!["a"]), ("a", vec!["x"]), ("p", vec!["a"])]);
        let first = adapt_at_production_grain(&forward, 1);
        let second = adapt_at_production_grain(&shuffled, 1);
        assert_eq!(first.candidate_fingerprint, second.candidate_fingerprint);
        assert_eq!(first.input_positions, second.input_positions);
        assert_eq!(first.output_positions, second.output_positions);
        assert_eq!(first.gate_output_positions, second.gate_output_positions);
        assert_eq!(first.gate_facings, second.gate_facings);
    }

    fn fanout(gates: &[(&str, Vec<&str>)]) -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["p".into(), "q".into()],
            gates: gates
                .iter()
                .map(|(output, inputs)| Gate::nor(*output, inputs))
                .collect(),
        }
    }

    fn reconvergent(gates: &[(&str, Vec<&str>)]) -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["z".into()],
            gates: gates
                .iter()
                .map(|(output, inputs)| Gate::nor(*output, inputs))
                .collect(),
        }
    }

    /// Pins are refused by type; this adapter does not place them.
    #[test]
    fn a_pin_the_packed_root_did_not_honour_is_refused_by_the_adapter() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["f".into()],
            gates: vec![Gate::nor("e", &["x"]), Gate::nor("f", &["e"])],
        };
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let packed = synthesise_packed_recursive_with(
            &net,
            &context,
            &DurablePhysicalRouter,
            &search,
            &certification,
            1,
            PackedGrain::test_seam(1),
        )
        .unwrap();

        let mut pins = PortPlacements::default();
        pins.pin(
            "x",
            Anchor { x: 1, y: 1, z: 0 },
            crate::redstone::world::block::Facing::South,
        );
        let Err(refusal) = adapt_packed_root(&net, &packed, Some(&pins), &search, 1) else {
            panic!("a root built without the pin cannot be handed back as honouring it");
        };
        assert!(
            matches!(refusal, PackedAdapterError::PinNotHonoured { ref signal, .. } if signal == "x"),
            "unexpected refusal: {refusal}"
        );
    }

    /// The most sibling workers a level of `siblings` children can spawn on
    /// this machine when `requested` are asked for. The expectation has to be
    /// machine-aware or a single-core runner would fail a determinism test for
    /// running serially.
    fn peak_for(requested: usize, siblings: usize) -> usize {
        CertificationWorkers::bounded(requested)
            .count()
            .min(siblings)
    }

    /// The level-one children of `net` under `context`, in the order the driver
    /// schedules them: what a refusal's `index`, `chunk` and `parent` name.
    fn level_one(net: &Netlist, context: &ChunkId) -> (ChunkId, Vec<Chunk>) {
        let root = node_chunk_id(net, context).unwrap();
        let mut chunks = partition(net, &root, split_of(net)).unwrap();
        chunks.sort_by(|left, right| left.id.cmp(&right.id));
        (root, chunks)
    }

    /// A leaf builder that records every chunk it is asked for, refuses when
    /// `refuse` says so, and otherwise defers to the production builder. The
    /// record is the evidence: which chunks were tried, at what size, and
    /// therefore how far a repair descended and whom it touched.
    fn recording_leaf(
        chunk: &Chunk,
        contract: SignalContract,
        search: &SearchConfig,
        refuse: bool,
        asked: &std::sync::Mutex<Vec<(ChunkId, usize)>>,
    ) -> Result<FreeLeafArtifact, FreeLeafError> {
        asked
            .lock()
            .unwrap()
            .push((chunk.id.clone(), chunk.netlist.gates.len()));
        if refuse {
            return Err(FreeLeafError::EmptyWorld);
        }
        synthesise_free_leaf(chunk, contract, search, &LEAF_PITCHES)
    }

    /// **A refused multi-gate leaf is repaired by splitting it, and only it.**
    ///
    /// At a grain of two the four-gate chain is two two-gate leaves. The
    /// builder refuses exactly one of them, so that one is repaired: split into
    /// two one-gate leaves the real builder accepts. The record shows exactly
    /// that -- the refused sibling asked once at two gates and then its halves
    /// at one gate each, the other sibling asked once and accepted -- and the
    /// packed root places both siblings as its direct children. Siblings in
    /// flight repair to the same answer as the serial baseline.
    #[test]
    fn a_refused_multi_gate_leaf_is_split_and_repaired_in_place() {
        let net = four_chain();
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let (root, chunks) = level_one(&net, &context);
        assert_eq!(chunks.len(), 2);
        let (refused, kept) = (&chunks[0], &chunks[1]);
        assert_eq!(refused.netlist.gates.len(), 2);
        assert_eq!(kept.netlist.gates.len(), 2);

        let asked = std::sync::Mutex::new(Vec::new());
        let leaf = |chunk: &Chunk, contract: SignalContract, search: &SearchConfig| {
            recording_leaf(chunk, contract, search, chunk.id == refused.id, &asked)
        };
        let build = |workers: usize| {
            synthesise_packed_recursive_using(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(2),
                &leaf,
            )
            .expect("a refused two-gate leaf is repaired by splitting it")
        };

        let serial = build(1);
        let record = asked.lock().unwrap().clone();
        let halves = level_one(&refused.netlist, &root).1;
        assert_eq!(halves.len(), 2, "the repair halves the refused child");
        // Exactly one ask per chunk, and only the refused child's halves are
        // ever asked for: the kept sibling is asked once and never split.
        let mut expected = vec![
            (refused.id.clone(), 2),
            (kept.id.clone(), 2),
            (halves[0].id.clone(), 1),
            (halves[1].id.clone(), 1),
        ];
        let mut sorted = record.clone();
        sorted.sort();
        expected.sort();
        assert_eq!(
            sorted, expected,
            "the repair touched only the refused child"
        );
        // With one worker the order is the schedule: the refused child, its
        // repair, then the kept sibling untouched.
        assert_eq!(
            record.iter().map(|(_, gates)| *gates).collect::<Vec<_>>(),
            [2, 1, 1, 2]
        );
        assert_eq!(serial.depth, 3, "the repaired child is one level deeper");
        assert_eq!(serial.peak_workers, 1);
        assert_eq!(
            serial.node.packed.placements.keys().collect::<Vec<_>>(),
            [&refused.id, &kept.id],
            "the root still packs its two level-one children"
        );

        // Siblings in flight change the schedule, not the answer.
        asked.lock().unwrap().clear();
        let parallel = build(4);
        let mut sorted = asked.lock().unwrap().clone();
        sorted.sort();
        assert_eq!(sorted, expected);
        assert_eq!(parallel.peak_workers, peak_for(4, 2));
        assert_eq!(parallel.node.fingerprint, serial.node.fingerprint);
        assert_eq!(parallel.node.certificate, serial.node.certificate);
        assert_eq!(parallel.node.root_ports, serial.node.root_ports);
        assert_eq!(parallel.node.trunks, serial.node.trunks);
        assert_eq!(parallel.node.gates, serial.node.gates);
        assert_eq!(parallel.depth, serial.depth);
    }

    /// **A refused one-gate leaf is a typed terminal refusal with a lineage.**
    ///
    /// At a grain of one the four-gate chain is two nonterminal children over
    /// four one-gate leaves. The builder refuses every leaf; nothing smaller
    /// exists, so the first refusal a serial run reaches is the answer: child
    /// zero of the first nonterminal child, carrying the builder's own error
    /// as its source rather than a rendering of it. Workers in flight reach
    /// the same refusal, because `run_indexed` files the lowest index.
    #[test]
    fn a_refused_one_gate_leaf_is_a_typed_terminal_refusal() {
        let net = four_chain();
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let (root, chunks) = level_one(&net, &context);
        let (inner, leaves) = level_one(&chunks[0].netlist, &root);
        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].netlist.gates.len(), 1);

        let asked = std::sync::Mutex::new(Vec::new());
        let leaf = |chunk: &Chunk, contract: SignalContract, search: &SearchConfig| {
            recording_leaf(chunk, contract, search, true, &asked)
        };
        let refuse = |workers: usize| {
            synthesise_packed_recursive_using(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(1),
                &leaf,
            )
            .expect_err("a one-gate leaf that refuses has nothing left to split")
        };

        let serial = refuse(1);
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            [(leaves[0].id.clone(), 1)],
            "a serial run stops at the first one-gate refusal"
        );
        let PackedRecursiveError::ChildRefused {
            index,
            chunk,
            parent,
            error,
        } = &serial
        else {
            panic!("expected a typed terminal refusal, got {serial}");
        };
        assert_eq!(*index, 0);
        assert_eq!(*chunk, leaves[0].id);
        assert_eq!(
            *parent, inner,
            "the parent named is the nonterminal child, not the root"
        );
        assert!(matches!(error, FreeLeafError::EmptyWorld));
        let source = std::error::Error::source(&serial).expect("the leaf error is the source");
        assert!(source.downcast_ref::<FreeLeafError>().is_some());

        // The same refusal, at any worker count.
        let parallel = refuse(4);
        let PackedRecursiveError::ChildRefused {
            index: parallel_index,
            chunk: parallel_chunk,
            parent: parallel_parent,
            ..
        } = &parallel
        else {
            panic!("expected a typed terminal refusal, got {parallel}");
        };
        assert_eq!(
            (parallel_index, parallel_chunk, parallel_parent),
            (index, chunk, parent)
        );
    }

    /// Splitting a refused shared-input leaf now repairs it through a real
    /// parent-owned fanout, with the same result for serial/parallel workers.
    #[test]
    fn a_refused_shared_input_leaf_repairs_with_parent_owned_fanout() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["p".into(), "q".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("p", &["b"]),
                Gate::nor("q", &["b"]),
            ],
        };
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let (_, chunks) = level_one(&net, &context);
        let fanout = chunks
            .iter()
            .find(|chunk| chunk.netlist.gates.iter().any(|gate| gate.output == "p"))
            .unwrap();
        assert_eq!(fanout.netlist.gates.len(), 2);
        assert_eq!(fanout.boundary_inputs, ["b"]);
        let asked = std::sync::Mutex::new(Vec::new());
        let leaf = |chunk: &Chunk, contract: SignalContract, search: &SearchConfig| {
            recording_leaf(chunk, contract, search, chunk.id == fanout.id, &asked)
        };
        let build = |workers| {
            synthesise_packed_recursive_using(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(2),
                &leaf,
            )
            .expect("a parent-owned input trunk joins both repaired halves")
        };
        let serial = build(1);
        let record = asked.lock().unwrap().clone();
        assert!(record.contains(&(fanout.id.clone(), 2)));
        assert_eq!(record.iter().filter(|(_, gates)| *gates == 1).count(), 2);
        let parallel = build(4);
        assert_eq!(serial.node.fingerprint, parallel.node.fingerprint);
        let product = adapt_packed_root(&net, &serial, None, &search, 1).unwrap();
        for x in [false, true] {
            let observed = observe(&product, &[("x", x)]);
            assert_eq!(observed["p"], !x);
            assert_eq!(observed["q"], !x);
        }
    }

    /// A trunk router that refuses every route, so packing fails after every
    /// leaf beneath it succeeded.
    struct RefusingRouter;

    impl crate::compile::routing::PhysicalRouter for RefusingRouter {
        fn route(
            &self,
            request: crate::compile::routing::RouteRequest<'_>,
        ) -> Result<crate::compile::routing::RealisedRouteTree, crate::compile::routing::RouterFailure>
        {
            Err(crate::compile::routing::RouterFailure::NoLocalRoute {
                route: request.id,
                source: request.source.id,
                sink: request.sinks.as_slice()[0].id,
            })
        }
    }

    /// A refused leaf whose output is also read inside it repairs through a
    /// node-built output handover: the exported signal and the chain's end
    /// both hold, the same with siblings in flight.
    #[test]
    fn a_refused_leaf_exporting_an_internal_signal_repairs_through_its_own_handover() {
        let mut net = four_chain();
        net.outputs.push("a".into());
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let (_, chunks) = level_one(&net, &context);
        let refused = chunks
            .iter()
            .find(|chunk| chunk.netlist.gates.iter().any(|gate| gate.output == "a"))
            .unwrap();
        assert_eq!(refused.boundary_outputs, ["a", "b"]);
        let asked = std::sync::Mutex::new(Vec::new());
        let leaf = |chunk: &Chunk, contract: SignalContract, search: &SearchConfig| {
            recording_leaf(chunk, contract, search, chunk.id == refused.id, &asked)
        };
        let build = |workers| {
            synthesise_packed_recursive_using(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(2),
                &leaf,
            )
            .expect("the repaired half exports `a` through a node-built handover")
        };
        let serial = build(1);
        let parallel = build(4);
        assert_eq!(serial.node.fingerprint, parallel.node.fingerprint);
        let product = adapt_packed_root(&net, &serial, None, &search, 1).unwrap();
        for x in [false, true] {
            let observed = observe(&product, &[("x", x)]);
            assert_eq!(observed["a"], !x);
            assert_eq!(observed["d"], x);
        }
    }

    /// Successful repair leaves can still fail at packing. Keep that cause,
    /// the original refusal and both node identities across worker counts.
    #[test]
    fn an_early_packing_repair_failure_keeps_its_typed_cause_and_lineage() {
        let mut net = four_chain();
        net.outputs.push("a".into());
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let (root, chunks) = level_one(&net, &context);
        let (refused_index, refused) = chunks
            .iter()
            .enumerate()
            .find(|(_, chunk)| chunk.netlist.gates.iter().any(|gate| gate.output == "a"))
            .unwrap();
        let (inner, halves) = level_one(&refused.netlist, &root);
        assert_eq!(refused.netlist.gates.len(), 2);
        assert_eq!(refused.boundary_outputs, ["a", "b"]);
        let asked = std::sync::Mutex::new(Vec::new());
        let leaf = |chunk: &Chunk, contract: SignalContract, search: &SearchConfig| {
            recording_leaf(chunk, contract, search, chunk.id == refused.id, &asked)
        };
        let mut serial_message = None;
        for workers in [1, 4] {
            asked.lock().unwrap().clear();
            let failure = synthesise_packed_recursive_using(
                &net,
                &context,
                &RefusingRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(2),
                &leaf,
            )
            .expect_err("no trunk joins the repaired halves");
            let PackedRecursiveError::RepairFailed {
                index,
                chunk,
                parent,
                refusal,
                error,
            } = &failure
            else {
                panic!("expected a typed repair failure, got {failure}");
            };
            assert_eq!((*index, chunk, parent), (refused_index, &refused.id, &root));
            assert!(matches!(refusal, FreeLeafError::EmptyWorld));
            let repair_error = error;
            let PackedRecursiveError::Packed { chunk, error } = &**repair_error else {
                panic!("the successful leaves must fail at packing, got {repair_error}");
            };
            assert_eq!(*chunk, inner);
            assert!(matches!(error, PackedNodeError::LayoutsExhausted { .. }));
            // thiserror exposes this boxed source as Box<PackedRecursiveError>,
            // not its dereferenced inner value.
            let source = std::error::Error::source(&failure).unwrap();
            assert_eq!(source.to_string(), repair_error.to_string());
            let cause = source
                .downcast_ref::<Box<PackedRecursiveError>>()
                .expect("the boxed repair cause remains typed");
            assert!(std::ptr::eq(cause, repair_error));
            assert!(matches!(
                std::error::Error::source(cause.as_ref())
                    .unwrap()
                    .downcast_ref::<PackedNodeError>(),
                Some(PackedNodeError::LayoutsExhausted { .. })
            ));
            let record = asked.lock().unwrap();
            assert_eq!(record.iter().filter(|(id, _)| id == &refused.id).count(), 1);
            assert_eq!(halves.len(), 2);
            for half in &halves {
                assert_eq!(
                    record
                        .iter()
                        .filter(|entry| **entry == (half.id.clone(), 1))
                        .count(),
                    1,
                    "both one-gate leaves succeeded before packing refused"
                );
            }
            let message = failure.to_string();
            if let Some(serial) = &serial_message {
                assert_eq!(&message, serial);
            } else {
                serial_message = Some(message);
            }
        }
    }

    /// **A repair that descends to a one-gate refusal reports both ends.**
    ///
    /// At a grain of two the builder refuses everything. The first two-gate
    /// child is repaired, its first one-gate half refuses, and nothing is left
    /// to split: the outer error is the two-gate child's `RepairFailed`, still
    /// carrying its own refusal, and its typed cause is the one-gate
    /// `ChildRefused` with the half's own index, chunk and inner parent.
    #[test]
    fn a_repair_that_ends_in_a_one_gate_refusal_keeps_both_lineages() {
        let net = four_chain();
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let (root, chunks) = level_one(&net, &context);
        let (inner, halves) = level_one(&chunks[0].netlist, &root);
        assert_eq!(chunks[0].netlist.gates.len(), 2);
        assert_eq!(halves[0].netlist.gates.len(), 1);

        let asked = std::sync::Mutex::new(Vec::new());
        let leaf = |chunk: &Chunk, contract: SignalContract, search: &SearchConfig| {
            recording_leaf(chunk, contract, search, true, &asked)
        };
        let failure = synthesise_packed_recursive_using(
            &net,
            &context,
            &DurablePhysicalRouter,
            &search,
            &certification,
            1,
            PackedGrain::test_seam(2),
            &leaf,
        )
        .expect_err("a one-gate half that refuses ends the repair");
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            [(chunks[0].id.clone(), 2), (halves[0].id.clone(), 1)],
            "the two-gate refusal was repaired once, then the one-gate refusal ended it"
        );

        let PackedRecursiveError::RepairFailed {
            index,
            chunk,
            parent,
            refusal,
            error,
        } = &failure
        else {
            panic!("expected a typed repair failure, got {failure}");
        };
        assert_eq!(*index, 0);
        assert_eq!(*chunk, chunks[0].id);
        assert_eq!(*parent, root);
        assert!(matches!(refusal, FreeLeafError::EmptyWorld));
        let PackedRecursiveError::ChildRefused {
            index: half_index,
            chunk: half_chunk,
            parent: half_parent,
            error: half_error,
        } = &**error
        else {
            panic!("expected the one-gate refusal as the cause, got {error}");
        };
        assert_eq!(*half_index, 0);
        assert_eq!(*half_chunk, halves[0].id);
        assert_eq!(
            *half_parent, inner,
            "the half's parent is the repaired child"
        );
        assert!(matches!(half_error, FreeLeafError::EmptyWorld));
    }

    /// The four-gate chain the production-grain tests use, as a fixture.
    fn four_chain() -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["d".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["b"]),
                Gate::nor("d", &["c"]),
            ],
        }
    }

    /// The driver recurses, certifies every level, and gives the same answer
    /// serially as it does with siblings in flight.
    #[test]
    fn a_recursive_packed_root_is_worker_invariant_and_three_levels_deep() {
        let net = chain();
        let context = root_chunk_id(&net).unwrap();
        let search = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&search);
        let build = |workers: usize| {
            synthesise_packed_recursive_with(
                &net,
                &context,
                &DurablePhysicalRouter,
                &search,
                &certification,
                workers,
                PackedGrain::test_seam(1),
            )
            .expect("a three-gate chain synthesises at a one-gate grain")
        };

        let serial = build(1);
        assert_eq!(serial.depth, 3, "node over a node and a leaf, over leaves");
        assert!(serial.depth > 1, "more than one contract level");
        assert_eq!(serial.peak_workers, 1, "a serial run spawns one worker");

        // Every gate is placed once, in the root's own frame, on a cell the
        // root world actually built.
        let gates = serial.gates();
        assert_eq!(gates.output_positions.len(), net.gates.len());
        assert_eq!(gates.facings.len(), net.gates.len());
        gates.covers(&net).expect("the root places its own gates");
        let (size_x, size_y, size_z) = serial.node.world.size();
        let mut seen = std::collections::BTreeSet::new();
        for (gate, at) in &gates.output_positions {
            assert!(
                at.x >= 0 && at.y >= 0 && at.z >= 0,
                "{gate} translated to {at:?}"
            );
            assert!(
                at.x < size_x && at.y < size_y && at.z < size_z,
                "{gate} at {at:?} is outside the root world"
            );
            assert_ne!(
                serial.node.world.get(at.x, at.y, at.z).kind,
                BlockKind::Air,
                "{gate} at {at:?} names an empty cell"
            );
            assert!(seen.insert(*at), "{gate} shares a cell with another gate");
        }

        // The root hands itself on carrying exactly what it accumulated.
        assert_eq!(&serial.artifact.gates, gates);
        assert_eq!(
            serial.artifact.certificate.as_ref(),
            Some(&serial.node.certificate)
        );

        // Siblings in flight change the schedule, not the answer.
        let parallel = build(4);
        assert_eq!(parallel.depth, serial.depth);
        assert_eq!(
            parallel.peak_workers,
            peak_for(4, 2),
            "two siblings per level is two workers, on a machine that has them"
        );
        assert_eq!(parallel.node.fingerprint, serial.node.fingerprint);
        assert_eq!(
            parallel.node.certificate.world_fingerprint,
            serial.node.certificate.world_fingerprint
        );
        assert_eq!(parallel.node.certificate, serial.node.certificate);
        assert_eq!(parallel.node.root_ports, serial.node.root_ports);
        assert_eq!(parallel.node.trunks, serial.node.trunks);
        assert_eq!(parallel.node.gates, serial.node.gates);
        assert_eq!(parallel.artifact.chunk, serial.artifact.chunk);
    }
}
