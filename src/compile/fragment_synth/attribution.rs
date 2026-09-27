//! **Where a measured critical path spends its time: inside children, or on
//! the parent trunks between them.**
//!
//! A recursive-contract product is one world; the timing harness reads a
//! critical path off it as a chain of signal names, and the settle time as
//! one number. This module splits that number by the production partition:
//! every gate on the path belongs to exactly one leaf chunk of the tree
//! [`partition`] builds by halving down to [`TERMINAL_GATES`], and every
//! consecutive hop on the path is either inside one chunk or a trunk from
//! one chunk to another. The measured tick deltas between consecutive
//! arrivals are attributed to those hops, so the totals reconcile exactly to
//! the arrival of the critical output -- with the lead before the path's
//! first signal and the tail after its last accounted for separately and
//! explicitly, never folded into either bucket.
//!
//! The partition is the static one: a leaf that the router refused and the
//! repair split further is finer in the product than here, and a hop such a
//! split created would be counted as inside its static chunk. That is a
//! stated limit of the static view, not a silent one.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::compile::fragment_synth::certification::QualityKey;
use crate::compile::fragment_synth::partition::{
    partition, root_chunk_id, ChunkId, PartitionError,
};
use crate::compile::fragment_synth::recursive::{split_of, TERMINAL_GATES};
use crate::compile::Netlist;
use crate::timing::TransitionResult;

/// One leaf the recursive producer actually built: its stable identity in
/// the producer's own tree, and the gates it certified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeafDiagnostic {
    pub chunk: ChunkId,
    pub gates: Vec<String>,
}

/// One parent trunk the root node actually laid, read off the realised
/// route tree it shipped: the boundary signal it carries, its conductor and
/// floor cells, the repeaters among those conductors, the repeaters each
/// branch's terminal added, and the lane it was guided to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrunkSummary {
    pub signal: String,
    pub cells: usize,
    pub floors: usize,
    pub repeaters: usize,
    pub branch_terminal_repeaters: Vec<u64>,
    pub lane: Option<i32>,
}

/// What the recursive producer built, as the read-only facts a measurement
/// needs to be sure it is attributing to the right things: every leaf, at
/// any depth, and the root node's trunks. Reported, never consulted by the
/// producer itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveDiagnostics {
    pub leaves: Vec<LeafDiagnostic>,
    pub root_trunks: Vec<TrunkSummary>,
    /// Every candidate the packed root built, in list order, and what each
    /// certified to. Empty for a product that was not chosen from a list.
    pub candidates: Vec<CandidateOutcome>,
    /// The index into `candidates` that shipped.
    pub chosen: Option<usize>,
}

/// One packed-root candidate and its outcome: the certified quality, or the
/// refusal as text. Reported, never consulted by the producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateOutcome {
    pub label: String,
    pub quality: Result<QualityKey, String>,
}

/// Which side of the partition one critical-path hop lies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HopKind {
    /// Both ends belong to the same leaf chunk.
    Intra(ChunkId),
    /// The ends belong to different leaf chunks: a parent trunk.
    Trunk { from: ChunkId, to: ChunkId },
    /// The hop starts at a primary input, which no chunk owns; it enters the
    /// chunk that owns its reader.
    FromInput(ChunkId),
}

/// One consecutive hop of the measured critical path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HopAttribution {
    pub from: String,
    pub to: String,
    pub kind: HopKind,
    /// Arrival of `to` minus arrival of `from`, in game ticks, on the worst
    /// transition. This is measured, not modelled.
    pub ticks: u64,
}

/// The whole path, with totals that reconcile to the measurement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAttribution {
    pub hops: Vec<HopAttribution>,
    /// Ticks from the transition's start to the arrival of the path's first
    /// signal: the input's own change, before any hop.
    pub lead_ticks: u64,
    pub intra_ticks: u64,
    pub trunk_ticks: u64,
    /// Ticks from the arrival of the critical output to the settle the
    /// simulator reported: lamp and quiescence, after the last hop.
    pub tail_ticks: u64,
    pub settle_game_ticks: u64,
    /// Chunks the path touches, in path order, deduplicated.
    pub chunks: Vec<ChunkId>,
}

impl PathAttribution {
    /// `lead + intra + trunk + tail == settle`, which is what makes this an
    /// attribution rather than an estimate.
    pub fn reconciles(&self) -> bool {
        self.lead_ticks + self.intra_ticks + self.trunk_ticks + self.tail_ticks
            == self.settle_game_ticks
    }

    /// The trunk share of the ticks between the path's first and last
    /// arrivals, which is the part any packing or seam change can move.
    pub fn trunk_fraction_of_path(&self) -> f64 {
        let path = self.intra_ticks + self.trunk_ticks;
        if path == 0 {
            0.0
        } else {
            self.trunk_ticks as f64 / path as f64
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AttributionError {
    #[error(transparent)]
    Partition(#[from] PartitionError),
    #[error("critical path gate {signal} belongs to no leaf chunk of the production partition")]
    UnpartitionedGate { signal: String },
    #[error("critical path signal {signal} is neither a gate output nor a primary input")]
    UnknownSignal { signal: String },
    #[error("critical path signal {signal} has no arrival on the worst transition")]
    NoArrival { signal: String },
    #[error("arrival of {to} ({to_tick}) precedes arrival of {from} ({from_tick})")]
    ArrivalOrder {
        from: String,
        from_tick: u64,
        to: String,
        to_tick: u64,
    },
    #[error("settle ({settle}) precedes the critical output's arrival ({arrival})")]
    SettleBeforeArrival { settle: u64, arrival: u64 },
    #[error("a critical path needs at least two signals")]
    PathTooShort,
    #[error("the product carries no recursive diagnostics; only the packed recursive root does")]
    NoDiagnostics,
    #[error(
        "the product's {actual} leaves do not match the {expected} leaves of the static production partition: first difference at leaf {first_difference:?}"
    )]
    PartitionMismatch {
        expected: usize,
        actual: usize,
        first_difference: Vec<String>,
    },
    #[error("no root trunk carries critical hop signal {signal}")]
    TrunkNotFound { signal: String },
}

/// Refuse unless the leaves the producer actually built are, gate for gate,
/// the leaves the static partition attributes to.
///
/// Identities differ by construction -- the producer names its root under a
/// parent, the static view under none -- so leaves are compared as sets of
/// gate outputs, which is what the attribution keys on. Any difference is a
/// typed refusal naming the first leaf that does not match.
pub fn verify_partition(
    netlist: &Netlist,
    leaves: &[LeafDiagnostic],
) -> Result<(), AttributionError> {
    let owner = production_leaf_chunks(netlist)?;
    let mut expected: BTreeMap<&ChunkId, Vec<String>> = BTreeMap::new();
    for (gate, chunk) in &owner {
        expected.entry(chunk).or_default().push(gate.clone());
    }
    let mut expected = expected
        .into_values()
        .map(|mut gates| {
            gates.sort();
            gates
        })
        .collect::<Vec<_>>();
    expected.sort();
    let mut actual = leaves
        .iter()
        .map(|leaf| {
            let mut gates = leaf.gates.clone();
            gates.sort();
            gates
        })
        .collect::<Vec<_>>();
    actual.sort();
    if expected != actual {
        let first_difference = expected
            .iter()
            .zip(actual.iter())
            .find(|(e, a)| e != a)
            .map(|(e, _)| e.clone())
            .or_else(|| expected.get(actual.len()).cloned())
            .or_else(|| actual.get(expected.len()).cloned())
            .unwrap_or_default();
        return Err(AttributionError::PartitionMismatch {
            expected: expected.len(),
            actual: actual.len(),
            first_difference,
        });
    }
    Ok(())
}

/// The root trunk behind each trunk hop of an attributed path, by the
/// boundary signal the hop leaves on: a trunk hop `a -> b` rides the trunk
/// that carries `a`. A hop with no such trunk is a typed refusal.
pub fn critical_trunks<'a>(
    attribution: &'a PathAttribution,
    trunks: &'a [TrunkSummary],
) -> Result<Vec<(&'a HopAttribution, &'a TrunkSummary)>, AttributionError> {
    attribution
        .hops
        .iter()
        .filter(|hop| matches!(hop.kind, HopKind::Trunk { .. }))
        .map(|hop| {
            trunks
                .iter()
                .find(|trunk| trunk.signal == hop.from)
                .map(|trunk| (hop, trunk))
                .ok_or_else(|| AttributionError::TrunkNotFound {
                    signal: hop.from.clone(),
                })
        })
        .collect()
}

/// Every gate output's leaf chunk under the production partition: the same
/// halving [`split_of`] chooses at every level, stopped at the same grain
/// [`TERMINAL_GATES`] names, from the same [`root_chunk_id`].
pub fn production_leaf_chunks(
    netlist: &Netlist,
) -> Result<BTreeMap<String, ChunkId>, AttributionError> {
    let root = root_chunk_id(netlist)?;
    let mut owner = BTreeMap::new();
    let mut pending = vec![(root, netlist.clone())];
    while let Some((id, net)) = pending.pop() {
        if net.gates.len() <= TERMINAL_GATES {
            for gate in &net.gates {
                owner.insert(gate.output.clone(), id.clone());
            }
            continue;
        }
        for chunk in partition(&net, &id, split_of(&net))? {
            pending.push((chunk.id, chunk.netlist));
        }
    }
    Ok(owner)
}

/// Attribute one measured critical path on one transition.
///
/// `arrivals` are the worst transition's per-net arrival ticks, relative to
/// its start; `settle` its settle time. Every path signal must have an
/// arrival, arrivals must not run backwards along the path, and every gate on
/// the path must be owned by exactly one leaf chunk -- anything else is a
/// typed refusal, never a bucket.
pub fn attribute_path(
    netlist: &Netlist,
    path: &[String],
    arrivals: &BTreeMap<String, u64>,
    settle: u64,
) -> Result<PathAttribution, AttributionError> {
    attribute_path_with(netlist, &production_leaf_chunks(netlist)?, path, arrivals, settle)
}

/// Every gate output's leaf chunk in what the producer actually built: the
/// leaves its diagnostics record, or -- for a product with none, the direct
/// root leaf -- the whole netlist as one leaf under its root identity. A gate
/// no leaf, or two leaves, claims is a typed refusal.
pub fn leaf_owner(
    netlist: &Netlist,
    diagnostics: Option<&RecursiveDiagnostics>,
) -> Result<BTreeMap<String, ChunkId>, AttributionError> {
    let mut owner = BTreeMap::new();
    match diagnostics.filter(|diagnostics| !diagnostics.leaves.is_empty()) {
        Some(diagnostics) => {
            for leaf in &diagnostics.leaves {
                for gate in &leaf.gates {
                    if owner.insert(gate.clone(), leaf.chunk.clone()).is_some() {
                        return Err(AttributionError::UnpartitionedGate {
                            signal: gate.clone(),
                        });
                    }
                }
            }
        }
        None => {
            let root = root_chunk_id(netlist)?;
            for gate in &netlist.gates {
                owner.insert(gate.output.clone(), root.clone());
            }
        }
    }
    if let Some(gate) = netlist.gates.iter().find(|gate| !owner.contains_key(&gate.output)) {
        return Err(AttributionError::UnpartitionedGate {
            signal: gate.output.clone(),
        });
    }
    Ok(owner)
}

/// The critical path of one measured transition, read backwards off the
/// arrivals: from the declared output that settled last, each step goes to
/// the input of the current gate that changed last, until a primary input or
/// a gate none of whose inputs changed.
///
/// A gate's last change is caused by its last-changing input, so this is the
/// path the measurement itself took, with no timing model in between. Ties
/// go to the earlier declared output and the earlier gate input, so the path
/// is the same on every run.
pub fn last_change_path(
    netlist: &Netlist,
    arrivals: &BTreeMap<String, u64>,
) -> Result<Vec<String>, AttributionError> {
    let (mut current, mut at) = netlist
        .outputs
        .iter()
        .filter_map(|output| arrivals.get(output).map(|&tick| (output.clone(), tick)))
        .fold(None, |best: Option<(String, u64)>, (output, tick)| match best {
            Some((_, best_tick)) if best_tick >= tick => best,
            _ => Some((output, tick)),
        })
        .ok_or(AttributionError::PathTooShort)?;
    let mut path = vec![current.clone()];
    while let Some(gate) = netlist.gates.iter().find(|gate| gate.output == current) {
        let Some((input, tick)) = gate
            .inputs
            .iter()
            .filter_map(|input| arrivals.get(input).map(|&tick| (input, tick)))
            .filter(|&(_, tick)| tick <= at)
            .fold(None, |best: Option<(&String, u64)>, (input, tick)| match best {
                Some((_, best_tick)) if best_tick >= tick => best,
                _ => Some((input, tick)),
            })
        else {
            break;
        };
        if path.len() > netlist.gates.len() {
            return Err(AttributionError::UnknownSignal {
                signal: input.clone(),
            });
        }
        path.push(input.clone());
        current = input.clone();
        at = tick;
    }
    path.reverse();
    Ok(path)
}

/// [`attribute_path`] against an explicit leaf ownership, such as
/// [`leaf_owner`] reads off a shipped product.
pub fn attribute_path_with(
    netlist: &Netlist,
    owner: &BTreeMap<String, ChunkId>,
    path: &[String],
    arrivals: &BTreeMap<String, u64>,
    settle: u64,
) -> Result<PathAttribution, AttributionError> {
    if path.len() < 2 {
        return Err(AttributionError::PathTooShort);
    }
    let is_input = |signal: &str| netlist.inputs.iter().any(|input| input == signal);
    let is_gate = |signal: &str| netlist.gates.iter().any(|gate| gate.output == signal);
    let chunk_of = |signal: &str| -> Result<ChunkId, AttributionError> {
        if !is_gate(signal) {
            return Err(AttributionError::UnknownSignal {
                signal: signal.to_owned(),
            });
        }
        owner
            .get(signal)
            .cloned()
            .ok_or_else(|| AttributionError::UnpartitionedGate {
                signal: signal.to_owned(),
            })
    };
    let arrival = |signal: &str| -> Result<u64, AttributionError> {
        arrivals
            .get(signal)
            .copied()
            .ok_or_else(|| AttributionError::NoArrival {
                signal: signal.to_owned(),
            })
    };
    let mut hops = Vec::with_capacity(path.len() - 1);
    let mut chunks: Vec<ChunkId> = Vec::new();
    let mut intra = 0_u64;
    let mut trunk = 0_u64;
    // A primary input that did not change in this transition has no observer
    // event; its value was already present at transition start (tick zero).
    let lead_ticks = match arrival(&path[0]) {
        Ok(tick) => tick,
        Err(AttributionError::NoArrival { .. }) if is_input(&path[0]) => 0,
        Err(error) => return Err(error),
    };
    for (index, pair) in path.windows(2).enumerate() {
        let (from, to) = (&pair[0], &pair[1]);
        let to_chunk = chunk_of(to)?;
        let kind = if is_input(from) && !is_gate(from) {
            HopKind::FromInput(to_chunk.clone())
        } else {
            let from_chunk = chunk_of(from)?;
            if from_chunk == to_chunk {
                HopKind::Intra(to_chunk.clone())
            } else {
                HopKind::Trunk {
                    from: from_chunk,
                    to: to_chunk.clone(),
                }
            }
        };
        let from_tick = if index == 0 {
            lead_ticks
        } else {
            arrival(from)?
        };
        let to_tick = arrival(to)?;
        let ticks =
            to_tick
                .checked_sub(from_tick)
                .ok_or_else(|| AttributionError::ArrivalOrder {
                    from: from.clone(),
                    from_tick,
                    to: to.clone(),
                    to_tick,
                })?;
        match &kind {
            HopKind::Trunk { .. } => trunk += ticks,
            HopKind::Intra(_) | HopKind::FromInput(_) => intra += ticks,
        }
        if !chunks.contains(&to_chunk) {
            chunks.push(to_chunk);
        }
        hops.push(HopAttribution {
            from: from.clone(),
            to: to.clone(),
            kind,
            ticks,
        });
    }
    let last = arrival(path.last().expect("path has two signals"))?;
    let tail_ticks = settle
        .checked_sub(last)
        .ok_or(AttributionError::SettleBeforeArrival {
            settle,
            arrival: last,
        })?;
    Ok(PathAttribution {
        hops,
        lead_ticks,
        intra_ticks: intra,
        trunk_ticks: trunk,
        tail_ticks,
        settle_game_ticks: settle,
        chunks,
    })
}

/// [`attribute_path`] on a measured transition.
pub fn attribute_transition(
    netlist: &Netlist,
    path: &[String],
    transition: &TransitionResult,
) -> Result<PathAttribution, AttributionError> {
    attribute_path(netlist, path, &arrivals_of(transition), transition.settle_game_ticks)
}

/// Every net's arrival on one measured transition, relative to its start.
pub fn arrivals_of(transition: &TransitionResult) -> BTreeMap<String, u64> {
    transition
        .nets
        .iter()
        .filter_map(|(name, timing)| timing.arrival_tick().map(|tick| (name.clone(), tick)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::compile::fragment_synth::benchmark::legacy_benchmark_evaluator;
    use crate::compile::fragment_synth::candidate::endpoint_for_driver;
    use crate::compile::fragment_synth::certification::CompleteCandidateCertifier;
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::identity::{ConnectionId, GateIndex, PortId};
    use crate::compile::fragment_synth::instance_graph::LogicalSignalId;
    use crate::compile::fragment_synth::partition::Chunk;
    use crate::compile::fragment_synth::seed::{
        compile_parent_connectable_seed_with_services, SeedInput, SeedServices,
    };
    use crate::compile::fragment_synth::services::{
        DurableSeedEmitter, DurableSeedVerifier, TopologyAwareSeedPlacer,
    };
    use crate::compile::routing::{DurablePhysicalRouter, RouteTarget};
    use crate::compile::topology::Library;
    use crate::compile::Gate;
    use crate::redstone::world::block::BlockKind;

    fn chain(gates: usize) -> Netlist {
        let names = (0..=gates).map(|i| format!("s{i}")).collect::<Vec<_>>();
        Netlist {
            inputs: vec![names[0].clone()],
            outputs: vec![names[gates].clone()],
            gates: (1..=gates)
                .map(|i| Gate::nor(&names[i], &[&names[i - 1]]))
                .collect(),
        }
    }

    fn recursive_leaf_chunks(netlist: &Netlist) -> Vec<Chunk> {
        let root = root_chunk_id(netlist).unwrap();
        let root_chunk = Chunk {
            id: root,
            netlist: netlist.clone(),
            boundary_inputs: netlist.inputs.clone(),
            boundary_outputs: netlist.outputs.clone(),
        };
        let mut pending = vec![root_chunk];
        let mut leaves = Vec::new();
        while let Some(chunk) = pending.pop() {
            if chunk.netlist.gates.len() <= TERMINAL_GATES {
                leaves.push(chunk);
            } else {
                pending.extend(
                    partition(&chunk.netlist, &chunk.id, split_of(&chunk.netlist)).unwrap(),
                );
            }
        }
        leaves.sort_by(|left, right| left.id.cmp(&right.id));
        leaves
    }

    fn logical_signal_name<'a>(signal: LogicalSignalId, netlist: &'a Netlist) -> &'a str {
        match signal {
            LogicalSignalId::PrimaryInput(PortId(index)) => &netlist.inputs[index as usize],
            LogicalSignalId::GateOutput(GateIndex(index)) => &netlist.gates[index as usize].output,
        }
    }

    /// One repeatable paired-child measurement for the real segment_a leaf
    /// partition. Route totals count owned cells once; branch counts follow the
    /// exact certified source-to-sink paths and may share those owned cells.
    #[test]
    #[ignore = "manual certified-leaf metric gate; run with --release -- --ignored --nocapture"]
    fn segment_a_recursive_leaf_route_metrics() {
        let evaluator = legacy_benchmark_evaluator().unwrap();
        let netlist = evaluator.fixture("segment_a").unwrap().lowered_netlist();
        let leaves = recursive_leaf_chunks(netlist);
        assert_eq!(leaves.len(), 2, "the production recursion has two leaves");

        let wanted = BTreeSet::from([
            ("g4".to_owned(), "g26".to_owned()),
            ("g28".to_owned(), "g40".to_owned()),
            ("g41".to_owned(), "g44".to_owned()),
            ("g27".to_owned(), "g28".to_owned()),
        ]);
        let mut found = BTreeSet::new();
        for chunk in leaves {
            let library = Library::default_library();
            let search = SearchConfig::checked_defaults();
            let certified = compile_parent_connectable_seed_with_services(
                SeedInput {
                    lowered: &chunk.netlist,
                    source_provenance: None,
                    pins: None,
                },
                SeedServices {
                    library: &library,
                    placer: &TopologyAwareSeedPlacer,
                    router: &DurablePhysicalRouter,
                    emitter: &DurableSeedEmitter,
                    verifier: &DurableSeedVerifier,
                    certifier: &CompleteCandidateCertifier,
                    search_config: &search,
                },
            )
            .unwrap_or_else(|error| panic!("leaf {:?} did not certify: {error}", chunk.id));
            let candidate = certified.candidate();
            let gates = chunk
                .netlist
                .gates
                .iter()
                .map(|gate| gate.output.clone())
                .collect::<Vec<_>>();
            let route_cells = candidate
                .routes
                .values()
                .map(|route| route.cells.len())
                .sum::<usize>();
            let repeaters = candidate
                .routes
                .values()
                .flat_map(|route| &route.cells)
                .filter(|cell| cell.state.kind == BlockKind::Repeater)
                .count();
            eprintln!(
                "segment_a leaf chunk={:?} candidate={:?} gates={gates:?} route_cells={route_cells} repeaters={repeaters}",
                chunk.id,
                candidate.fingerprint()
            );

            let graph = &candidate.instances;
            for route in candidate.routes.values() {
                let source = graph
                    .assignments
                    .iter()
                    .find(|assignment| {
                        endpoint_for_driver(&assignment.driver) == Some(route.source)
                    })
                    .map(|assignment| assignment.signal);
                let Some(source) = source else { continue };
                let source_name = logical_signal_name(source, &chunk.netlist);
                for branch in &route.branches {
                    let RouteTarget::Connection(ConnectionId::External {
                        instance,
                        input_index,
                    }) = branch.target
                    else {
                        continue;
                    };
                    let sink_gate = graph
                        .instances
                        .iter()
                        .find(|candidate| candidate.id == instance)
                        .expect("route target instance exists")
                        .logical_gate;
                    let sink_name =
                        logical_signal_name(LogicalSignalId::GateOutput(sink_gate), &chunk.netlist);
                    if !wanted.contains(&(source_name.to_owned(), sink_name.to_owned())) {
                        continue;
                    }
                    assert_eq!(
                        chunk.netlist.gates[sink_gate.0 as usize].inputs[input_index as usize],
                        source_name,
                        "logical IDs identify the exact routed edge"
                    );
                    let path = branch.path.iter().copied().collect::<BTreeSet<_>>();
                    let path_cells = route
                        .cells
                        .iter()
                        .filter(|cell| path.contains(&cell.at))
                        .collect::<Vec<_>>();
                    let path_repeaters = path_cells
                        .iter()
                        .filter(|cell| cell.state.kind == BlockKind::Repeater)
                        .count();
                    let vertical_edges = branch
                        .path
                        .windows(2)
                        .filter(|edge| edge[0].y != edge[1].y)
                        .count();
                    found.insert((source_name.to_owned(), sink_name.to_owned()));
                    eprintln!(
                        "segment_a branch {source:?} ({source_name}) -> {:?} ({sink_name}) cells={} repeaters={path_repeaters} vertical_edges={vertical_edges}",
                        LogicalSignalId::GateOutput(sink_gate),
                        path_cells.len()
                    );
                }
            }
        }
        assert_eq!(
            found, wanted,
            "all four named critical branches are measured"
        );
    }

    /// Every gate of a netlist above the grain lands in exactly one leaf
    /// chunk, and the leaves are the halving `partition` makes at the grain.
    #[test]
    fn every_gate_belongs_to_exactly_one_production_leaf_chunk() {
        let net = chain(2 * TERMINAL_GATES + 1);
        let owner = production_leaf_chunks(&net).unwrap();
        assert_eq!(owner.len(), net.gates.len());
        let distinct = owner.values().collect::<std::collections::BTreeSet<_>>();
        assert!(
            distinct.len() >= 2,
            "a netlist above the grain has more than one leaf"
        );
        for gate in &net.gates {
            assert!(
                owner.contains_key(&gate.output),
                "{} is unowned",
                gate.output
            );
        }
        // Below the grain: one chunk, the root.
        let small = chain(3);
        let owner = production_leaf_chunks(&small).unwrap();
        assert_eq!(
            owner
                .values()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1
        );
    }

    /// A path from one chunk into another has exactly one trunk hop, the
    /// buckets are the measured deltas, and lead + intra + trunk + tail is
    /// the settle time to the tick.
    #[test]
    fn a_two_chunk_path_has_one_trunk_hop_and_reconciles_to_the_settle() {
        let net = chain(2 * TERMINAL_GATES);
        let owner = production_leaf_chunks(&net).unwrap();
        // Walk the chain until the chunk changes: that hop is the trunk.
        let names = (0..=2 * TERMINAL_GATES)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>();
        let seam = (2..names.len())
            .find(|&i| owner[&names[i]] != owner[&names[i - 1]])
            .expect("a two-leaf chain crosses once");
        let path = names[seam - 2..=seam + 1].to_vec();
        // Arrivals: 1 tick lead, 3 ticks per intra hop, 11 for the trunk.
        let mut arrivals = BTreeMap::new();
        let mut tick = 1;
        arrivals.insert(path[0].clone(), tick);
        for pair in path.windows(2) {
            tick += if owner.get(&pair[1]) != owner.get(&pair[0]) {
                11
            } else {
                3
            };
            arrivals.insert(pair[1].clone(), tick);
        }
        let settle = tick + 4;
        let attributed = attribute_path(&net, &path, &arrivals, settle).unwrap();
        let trunks = attributed
            .hops
            .iter()
            .filter(|hop| matches!(hop.kind, HopKind::Trunk { .. }))
            .count();
        assert_eq!(trunks, 1);
        assert_eq!(attributed.trunk_ticks, 11);
        assert_eq!(attributed.intra_ticks, 6);
        assert_eq!(attributed.lead_ticks, 1);
        assert_eq!(attributed.tail_ticks, 4);
        assert!(attributed.reconciles());
        assert_eq!(attributed.chunks.len(), 2);
    }

    /// The product's leaves must be the static partition's leaves, gate for
    /// gate, whatever their identities; and a trunk hop is tied to the root
    /// trunk carrying its signal, or refused.
    #[test]
    fn partition_verification_and_trunk_lookup_refuse_by_type() {
        let net = chain(2 * TERMINAL_GATES);
        let owner = production_leaf_chunks(&net).unwrap();
        let mut by_chunk: BTreeMap<ChunkId, Vec<String>> = BTreeMap::new();
        for (gate, chunk) in &owner {
            by_chunk
                .entry(chunk.clone())
                .or_default()
                .push(gate.clone());
        }
        let leaves = by_chunk
            .into_iter()
            .map(|(chunk, gates)| LeafDiagnostic { chunk, gates })
            .collect::<Vec<_>>();
        assert_eq!(verify_partition(&net, &leaves), Ok(()));
        // One gate moved to the other leaf: refused, naming the leaf.
        let mut moved = leaves.clone();
        let gate = moved[0].gates.pop().unwrap();
        moved[1].gates.push(gate);
        assert!(matches!(
            verify_partition(&net, &moved),
            Err(AttributionError::PartitionMismatch {
                expected: 2,
                actual: 2,
                ..
            })
        ));
        // A finer split than the static one: refused too.
        let mut finer = leaves.clone();
        let split = finer[0].gates.split_off(1);
        finer.push(LeafDiagnostic {
            chunk: finer[0].chunk.clone(),
            gates: split,
        });
        assert!(matches!(
            verify_partition(&net, &finer),
            Err(AttributionError::PartitionMismatch {
                expected: 2,
                actual: 3,
                ..
            })
        ));

        let trunks = vec![TrunkSummary {
            signal: "s3".into(),
            cells: 5,
            floors: 5,
            repeaters: 1,
            branch_terminal_repeaters: vec![1],
            lane: None,
        }];
        let hop = |from: &str, kind: HopKind| HopAttribution {
            from: from.into(),
            to: "x".into(),
            kind,
            ticks: 1,
        };
        let trunk_kind = HopKind::Trunk {
            from: leaves[0].chunk.clone(),
            to: leaves[1].chunk.clone(),
        };
        let attribution = PathAttribution {
            hops: vec![
                hop("s1", HopKind::Intra(leaves[0].chunk.clone())),
                hop("s3", trunk_kind.clone()),
            ],
            lead_ticks: 0,
            intra_ticks: 1,
            trunk_ticks: 1,
            tail_ticks: 0,
            settle_game_ticks: 2,
            chunks: vec![],
        };
        let tied = critical_trunks(&attribution, &trunks).unwrap();
        assert_eq!(tied.len(), 1);
        assert_eq!(tied[0].1.signal, "s3");
        let untied = PathAttribution {
            hops: vec![hop("s9", trunk_kind)],
            ..attribution
        };
        assert!(matches!(
            critical_trunks(&untied, &trunks),
            Err(AttributionError::TrunkNotFound { .. })
        ));
    }

    /// Unknown, unowned and backwards-arriving signals are typed refusals.
    #[test]
    fn attribution_refuses_by_type_rather_than_bucketing() {
        let net = chain(3);
        let mut arrivals = BTreeMap::new();
        for (i, name) in ["s0", "s1", "s2", "s3"].iter().enumerate() {
            arrivals.insert((*name).to_owned(), i as u64 * 2);
        }
        let path = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert!(matches!(
            attribute_path(&net, &path(&["s0", "nope"]), &arrivals, 10),
            Err(AttributionError::UnknownSignal { .. })
        ));
        assert!(matches!(
            attribute_path(&net, &path(&["s0", "s1", "s2", "s3"]), &arrivals, 5),
            Err(AttributionError::SettleBeforeArrival { .. })
        ));
        let mut backwards = arrivals.clone();
        backwards.insert("s2".to_owned(), 0);
        assert!(matches!(
            attribute_path(&net, &path(&["s1", "s2"]), &backwards, 10),
            Err(AttributionError::ArrivalOrder { .. })
        ));
        let mut missing = arrivals.clone();
        missing.remove("s3");
        assert!(matches!(
            attribute_path(&net, &path(&["s2", "s3"]), &missing, 10),
            Err(AttributionError::NoArrival { .. })
        ));
        assert!(matches!(
            attribute_path(&net, &path(&["s0"]), &arrivals, 10),
            Err(AttributionError::PathTooShort)
        ));
        // The input hop is its own kind, counted with the reader's chunk.
        let ok = attribute_path(&net, &path(&["s0", "s1"]), &arrivals, 10).unwrap();
        assert!(matches!(ok.hops[0].kind, HopKind::FromInput(_)));
        assert_eq!(ok.intra_ticks, 2);
        assert!(ok.reconciles());

        let mut unchanged_input = arrivals;
        unchanged_input.remove("s0");
        let ok = attribute_path(&net, &path(&["s0", "s1"]), &unchanged_input, 10).unwrap();
        assert_eq!(ok.lead_ticks, 0);
        assert_eq!(ok.intra_ticks, 2);
        assert_eq!(ok.tail_ticks, 8);
        assert!(ok.reconciles());
    }

    /// The path the measurement took: from the output that settled last,
    /// each step to the input that changed last, ties to the earlier one.
    #[test]
    fn last_change_path_follows_the_latest_input_back_to_a_primary_input() {
        let net = Netlist {
            inputs: vec!["a".to_owned(), "b".to_owned()],
            outputs: vec!["y".to_owned(), "z".to_owned()],
            gates: vec![
                Gate::nor("p", &["a"]),
                Gate::nor("q", &["b"]),
                Gate::nor("y", &["p", "q"]),
                Gate::nor("z", &["p"]),
            ],
        };
        let arrivals: BTreeMap<String, u64> =
            [("a", 0), ("b", 1), ("p", 2), ("q", 5), ("y", 7), ("z", 4)]
                .into_iter()
                .map(|(name, tick)| (name.to_owned(), tick))
                .collect();
        assert_eq!(last_change_path(&net, &arrivals).unwrap(), ["b", "q", "y"]);

        // Equal arrivals: the earlier declared output, then the earlier input.
        let tied: BTreeMap<String, u64> =
            [("a", 0), ("b", 0), ("p", 2), ("q", 2), ("y", 4), ("z", 4)]
                .into_iter()
                .map(|(name, tick)| (name.to_owned(), tick))
                .collect();
        assert_eq!(last_change_path(&net, &tied).unwrap(), ["a", "p", "y"]);

        // No output changed: nothing to read.
        assert!(matches!(
            last_change_path(&net, &BTreeMap::new()),
            Err(AttributionError::PathTooShort)
        ));
    }

    /// A product's own leaves decide ownership; with none, the whole netlist
    /// is one leaf under its root identity.
    #[test]
    fn leaf_owner_reads_the_shipped_leaves_or_the_whole_root() {
        let net = chain(3);
        let root = root_chunk_id(&net).unwrap();
        let direct = leaf_owner(&net, None).unwrap();
        assert!(direct.values().all(|chunk| *chunk == root));
        assert_eq!(direct.len(), 3);

        let halves = partition(&net, &root, 2).unwrap();
        let diagnostics = RecursiveDiagnostics {
            leaves: halves
                .iter()
                .map(|chunk| LeafDiagnostic {
                    chunk: chunk.id.clone(),
                    gates: chunk.netlist.gates.iter().map(|gate| gate.output.clone()).collect(),
                })
                .collect(),
            root_trunks: Vec::new(),
            candidates: Vec::new(),
            chosen: None,
        };
        let owned = leaf_owner(&net, Some(&diagnostics)).unwrap();
        assert_eq!(owned["s1"], halves[0].id);
        assert_eq!(owned["s3"], halves[1].id);

        let mut missing = diagnostics.clone();
        missing.leaves.pop();
        assert!(matches!(
            leaf_owner(&net, Some(&missing)),
            Err(AttributionError::UnpartitionedGate { .. })
        ));
    }
}
