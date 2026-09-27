//! One deterministic recursive-contract synthesis round.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

use crate::compile::fragment_synth::allocation::{
    allocate_with, normalise_root_pins, root_placement, AllocationError, AllocationLimits,
    AllocationPlan, ChildAllocation, ChildExtent, RootPort,
};
use crate::compile::fragment_synth::attribution::{CandidateOutcome, RecursiveDiagnostics};
use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use crate::compile::fragment_synth::certification::{
    certify_root_world, CandidateCertificationError, CandidateMetrics, CertificationWorkers,
    QualityKey, RootCertificate,
};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::leaf::{synthesise_leaf, LeafArtifact, LeafError};
use crate::compile::fragment_synth::leaf::LEAF_PITCHES;
use crate::compile::fragment_synth::placement::STANDARD_PITCH;
use crate::compile::fragment_synth::packed_recursive::{
    adapt_packed_root, synthesise_packed_recursive_fabric, synthesise_packed_recursive_on,
    synthesise_packed_recursive_pinned, wide_cut_differs, LeafCut,
    PackedAdapterError, PackedRecursiveError, PackedRecursiveProduct,
};
use crate::compile::fragment_synth::packed_node::{pinned_floors_short, PinnedRoom};
use crate::compile::fragment_synth::parent::{compose, ComposeError};
use crate::compile::fragment_synth::partition::{
    partition, root_chunk_id, Chunk, ChunkId, PartitionError,
};
use crate::compile::fragment_synth::schedule::ScheduleError;
use crate::compile::geometry::{Anchor, CellFacing};
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::planner::{self, PortPin, PortPlacements, PortRole};
use crate::compile::routing::{DurablePhysicalRouter, RealisedRouteTree};
use crate::compile::{self, Netlist};
use crate::redstone::simulator::SimulationError;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

/// The largest node the leaf router is asked to build in one piece.
///
/// Every larger node is a contract parent. Parents keep their children at this
/// grain until that would exceed the bounded fanout, then raise the grain just
/// enough to keep the level to eight children. One gate per leaf makes the
/// deepest possible tree, and pays for it: every gate boundary becomes a
/// portal, a trunk and a corridor lane, so a circuit is mostly interconnect
/// between single gates.
///
/// Thirty-two is the preferred direct-leaf grain. It was chosen from the same
/// measurements that showed each smaller circuit losing badly as a parent and
/// winning as one leaf:
///
/// | circuit | gates | as a parent | as one leaf | baseline |
/// |---|---|---|---|---|
/// | `verilog:and4` | 9 | 1142 blocks, 54 gt | 290, 14 | 480, 22 |
/// | `full_adder` | 22 | 4362 blocks, 146 gt | 1065, 46 | 1784, 46 |
///
/// In both cases nearly all of the difference is the corridor joining halves
/// that did not need joining -- `full_adder` spends 190 extra repeaters and
/// some 1500 extra dust cells on it. Thirty-two keeps those small roots on the
/// direct leaf path, while a sixty-five-gate netlist still crosses the boundary
/// and recurses.
///
/// It is not raised further because the flat planner's own ring invariant
/// refuses `segment_a` at 46 gates and `seven_segment` at 84 -- measured, with
/// addresses -- so a larger grain would only move those refusals earlier.
///
/// This is the *preferred* grain, not a floor: a leaf the router refuses is
/// split again, all the way down to one gate if that is what it takes. Roots at
/// or below this grain first try the flat leaf; only its typed leaf refusal
/// continues into the existing recursive solver. See the repair arm in
/// `split_refused_child` here, and `synthesise_child` in `packed_recursive`,
/// both of which deliberately do not read this constant.
pub(crate) const TERMINAL_GATES: usize = 32;

/// The largest seed-built leaf the wide fabric candidate asks for.
///
/// [`TERMINAL_GATES`] stays the production grain; this only names one extra,
/// fixed candidate the unpinned root also builds and keeps when it is no
/// worse on ticks and blocks ([`pick`]). The number is measured, not tuned:
/// on the lid fabric a free leaf of 42 gates (`seven_segment` cut once) and
/// one of 46 (`segment_a` whole) both certify inside the unchanged A* cap,
/// shipping 142 ticks / 15,261 blocks and 80 / 4,183 against 198 / 21,847 and
/// 124 / 9,305 at the production grain; one of 84 gates is refused and falls
/// back through the ordinary repair split. A refused wide leaf splits into
/// exactly the chunks the production grain would have built.
pub(crate) const WIDE_LEAF_GATES: usize = 48;
const MAX_RECURSIVE_WORKERS: usize = 8;

pub(crate) struct RecursiveProduct {
    pub world: World,
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_facings: Vec<CellFacing>,
    pub metrics: CandidateMetrics,
    pub candidate_fingerprint: Fingerprint,
    /// Contract levels this synthesis used: the root counts as one, a chunk
    /// that went straight to the leaf router as one more.  Nothing outside
    /// reads it until the public API unfreezes at Gate 3; it is what the
    /// depth tests assert on, because "it compiled" cannot tell a real
    /// recursion from one level that re-wrapped the whole netlist.
    #[cfg_attr(not(test), allow(dead_code))]
    pub depth: usize,
    /// The most sibling workers any single level actually spawned.
    ///
    /// The count that was used, not the one that was asked for. A level with
    /// one sibling collapses to a single worker however large the cap, so a
    /// determinism test that only sets `workers` can compare a serial run
    /// against another serial run and never notice; this is what tells it
    /// apart. It counts sibling workers spawned, not wall-clock overlap --
    /// that is deliberate, because overlap is a timing measurement and would
    /// make the assertion flaky.
    #[cfg_attr(not(test), allow(dead_code))]
    pub peak_workers: usize,
    /// Read-only facts about what was built, when the shape records them:
    /// the packed recursive root does, the direct leaf and the allocating
    /// root do not. Reported through the public result; consulted by nothing.
    pub diagnostics: Option<RecursiveDiagnostics>,
}

#[derive(Debug, Error)]
pub(crate) enum RecursiveError {
    #[error(transparent)]
    Partition(#[from] PartitionError),
    #[error(transparent)]
    Allocation(#[from] AllocationError),
    #[error(transparent)]
    Schedule(#[from] ScheduleError),
    #[error(transparent)]
    Compose(#[from] ComposeError),
    #[error(transparent)]
    PackedRecursive(#[from] PackedRecursiveError),
    #[error(transparent)]
    PackedAdapter(#[from] PackedAdapterError),
    #[error(transparent)]
    Certification(#[from] CandidateCertificationError),
    #[error("gate metadata is missing for `{0}`")]
    MissingGate(String),
    #[error("child {index} ({chunk:?}) refused its contract: {error}")]
    ChildRefused {
        index: usize,
        chunk: ChunkId,
        error: String,
    },
    #[error("child {index} ({chunk:?}) panicked during recursive synthesis")]
    ChildPanicked { index: usize, chunk: ChunkId },
    #[error("chunk {chunk:?} now needs {needed:?} but was allocated {available:?}")]
    ExtentRefused {
        chunk: ChunkId,
        needed: Anchor,
        available: Anchor,
    },
    #[error("chunk {chunk:?} still did not fit after {attempts} reallocations")]
    ExtentNegotiationExhausted { chunk: ChunkId, attempts: usize },
    #[error("root input {signal}'s lever cell {at:?} is outside the built world")]
    LeverOutOfWorld { signal: String, at: Anchor },
    #[error("root input {signal}'s lever cell {at:?} already holds {kind:?}")]
    LeverCollision {
        signal: String,
        at: Anchor,
        kind: BlockKind,
    },
}

/// Stand the lever a root ships in an input cell it chose itself, on the
/// floor a floor-faced lever needs.
///
/// The one installer both root shapes use for every input the caller did not
/// pin. The cell is expected empty -- a caller-row cell the allocator planned,
/// or a cell this root pinned for the caller and the planner therefore left
/// bare -- and a lever already standing there is the planner's own, placed
/// and floored by it, so it is kept exactly as placed. Anything else in the
/// cell is a refusal, not something to overwrite: an input cell holding
/// hardware is a plan this root does not understand. The floor cell gets
/// stone only when it is empty; whatever already stands there is left alone,
/// since a lever reads the same on any full block. `World::set` ignores a
/// cell outside the world, so the bounds are checked here rather than trusted.
fn install_root_lever(
    world: &mut World,
    signal: &str,
    at: (i32, i32, i32),
) -> Result<(), RecursiveError> {
    let (size_x, size_y, size_z) = world.size();
    let inside = |(x, y, z): (i32, i32, i32)| {
        x >= 0 && y >= 0 && z >= 0 && x < size_x && y < size_y && z < size_z
    };
    let anchor = |(x, y, z): (i32, i32, i32)| Anchor { x, y, z };
    if !inside(at) {
        return Err(RecursiveError::LeverOutOfWorld {
            signal: signal.to_owned(),
            at: anchor(at),
        });
    }
    match world.get(at.0, at.1, at.2).kind {
        BlockKind::Air => {}
        BlockKind::Lever => return Ok(()),
        kind => {
            return Err(RecursiveError::LeverCollision {
                signal: signal.to_owned(),
                at: anchor(at),
                kind,
            })
        }
    }
    let floor = (at.0, at.1 - 1, at.2);
    if !inside(floor) {
        return Err(RecursiveError::LeverOutOfWorld {
            signal: signal.to_owned(),
            at: anchor(floor),
        });
    }
    if world.get(floor.0, floor.1, floor.2).kind == BlockKind::Air {
        world.set(floor.0, floor.1, floor.2, compile::stone());
    }
    world.set(at.0, at.1, at.2, compile::lever(false));
    Ok(())
}

/// What one node hands its immediate parent: the artifact the parent composes,
/// plus the subtree facts the parent cannot recover from that artifact alone.
struct NodeOutcome {
    leaf: LeafArtifact,
    /// Every gate in this subtree, keyed by its output signal.  A nonterminal
    /// node's own chunk netlist names only its boundary, so its parent has no
    /// other way to reach the facings its grandchildren chose.
    facings: BTreeMap<String, CellFacing>,
    /// Contract levels below and including this node; a terminal leaf is 1.
    depth: usize,
}

/// The widest sibling fan-out any level of one synthesis actually spawned.
///
/// Counting live threads instead would count depth, not breadth: a parent
/// worker is still alive while the nested scope it is blocked on runs, so a
/// strictly serial run of a four-level tree reports four. What a determinism
/// test needs to know is whether any level handed work to more than one
/// worker at the same time, which is the effective width of that level.
#[derive(Default)]
struct Gauge {
    widest: AtomicUsize,
}

impl Gauge {
    fn record(&self, workers: usize) {
        self.widest.fetch_max(workers, Ordering::AcqRel);
    }

    fn peak(&self) -> usize {
        self.widest.load(Ordering::Acquire)
    }
}

/// Everything every node in one synthesis shares.
#[derive(Clone, Copy)]
struct Session<'a> {
    search: &'a SearchConfig,
    gauge: &'a Gauge,
    /// Workers this node may use.  Halved on the way down so the tree does
    /// not oversubscribe; never below one.
    workers: usize,
}

impl<'a> Session<'a> {
    fn nested(self) -> Self {
        Self {
            workers: (self.workers / 2).max(1),
            ..self
        }
    }
}

/// Whether the recursive path can honour this case's pins exactly.
///
/// **Test-only.** The public API no longer routes on this: there is no other
/// path to route to, so a case whose pins this would refuse is compiled here
/// and fails here, as an `UnsupportedRootPin` from `root_placement`. It stays
/// so the refusal rule itself can be tested by type, cell by cell.
///
/// Pins that fold onto one caller row are honoured the way they always were;
/// pins that do not are honoured literally, with the body landed behind them.
/// Only geometry no contract can build -- off the portal plane, in the parent's
/// own column, facing out of the plane, two ports on one cell -- is refused.
#[cfg(test)]
pub(crate) fn honours_pins(lowered: &Netlist, pins: Option<&PortPlacements>) -> bool {
    RootPins::normalise(lowered, pins)
        .is_ok_and(|pins| root_placement(lowered, pins.compiled()).is_ok())
}

/// The root's pins, as the caller supplied them and as the root compiles them.
///
/// The two differ only for a partial set: the caller's pins are kept exactly
/// and every declared port left unpinned is placed by
/// [`normalise_root_pins`], once, before either root shape runs. Both shapes
/// build on `compiled`, so a partial set takes the same branch, and is refused
/// the same way, as the complete set it becomes. `supplied` stays for the one
/// decision that must not see the completion: whose cell a port stands in. A
/// caller-pinned cell ships empty; every other port cell is this root's, and
/// gets the lever or lamp an unpinned root always shipped there.
struct RootPins<'a> {
    supplied: Option<&'a PortPlacements>,
    compiled: Option<PortPlacements>,
}

impl<'a> RootPins<'a> {
    fn normalise(
        lowered: &Netlist,
        supplied: Option<&'a PortPlacements>,
    ) -> Result<Self, RecursiveError> {
        let compiled = normalise_root_pins(lowered, supplied)?;
        Ok(Self { supplied, compiled })
    }

    fn compiled(&self) -> Option<&PortPlacements> {
        self.compiled.as_ref()
    }

    /// Did the caller pin `signal`? Its cell is the caller's then, and ships
    /// empty; any other port cell is this root's to furnish.
    fn caller_pinned(&self, signal: &str) -> bool {
        self.supplied.is_some_and(|pins| pins.get(signal).is_some())
    }
}

/// Where a diverged simulation's stuck cells sit in the routes this plan laid.
///
/// **Failure path only.** Nothing calls it unless certification already
/// refused, and it reads nothing that is not already in hand -- the plan and
/// the realised trunks `compose` returned. No state is kept for it and no
/// fingerprint sees it.
///
/// A pending position on a trunk's own conductor, far from its terminal, reads
/// very differently from two positions on the same trunk or from a position
/// nothing routed: the first is a wavefront still in flight, the second a
/// contention, the third hardware the router never owned.
fn describe_pending_routes(
    plan: &AllocationPlan,
    trunks: &[RealisedRouteTree],
    error: &CandidateCertificationError,
) -> Option<String> {
    let CandidateCertificationError::TransitionDidNotSettle {
        simulation_error:
            SimulationError::Diverged {
                pending_detail,
                game_ticks,
                last_progress_tick,
                ..
            },
        manifest_index,
        phase,
    } = error
    else {
        return None;
    };
    if pending_detail.is_empty() {
        return None;
    }

    let mut lines = vec![format!(
        "transition {manifest_index} {phase:?} diverged after {game_ticks} ticks \
         (last change at tick {last_progress_tick}); where its {} stuck cells sit:",
        pending_detail.len()
    )];
    let mut trunk_hits: BTreeMap<usize, usize> = BTreeMap::new();

    for detail in pending_detail {
        let at = Anchor {
            x: detail.position.x,
            y: detail.position.y,
            z: detail.position.z,
        };
        let found = trunks.iter().enumerate().find_map(|(index, tree)| {
            let cell = tree.cells.iter().position(|block| block.at == at);
            let mut branch = None;
            for (ordinal, candidate) in tree.branches.iter().enumerate() {
                if let Some(offset) = candidate.path.iter().position(|step| *step == at) {
                    let length = candidate.path.len();
                    branch = Some((ordinal, offset, length, candidate.terminal.repeaters));
                    break;
                }
            }
            (cell.is_some() || branch.is_some()).then_some((index, tree, cell, branch))
        });

        match found {
            None => lines.push(format!(
                "  {:?} {:?} lit={} power={} -- on no trunk this plan routed",
                detail.position, detail.kind, detail.lit, detail.power
            )),
            Some((index, tree, cell, branch)) => {
                *trunk_hits.entry(index).or_default() += 1;
                let signal = plan
                    .trunks
                    .get(index)
                    .map(|trunk| trunk.signal.as_str())
                    .unwrap_or("<unknown>");
                let cells = tree.cells.len();
                let where_on_branch = match branch {
                    Some((ordinal, offset, length, repeaters)) => format!(
                        "branch {ordinal} step {offset}/{length}, terminal repeaters {repeaters}"
                    ),
                    None => "not on any branch path".to_owned(),
                };
                let where_on_trunk = match cell {
                    Some(offset) => format!("conductor {offset}/{cells}"),
                    None => format!("not a conductor of its {cells}"),
                };
                lines.push(format!(
                    "  {:?} {:?} lit={} power={} -- signal {signal}, {:?}, {where_on_trunk}, \
                     {where_on_branch}",
                    detail.position, detail.kind, detail.lit, detail.power, tree.id
                ));
            }
        }
    }

    let shared: Vec<String> = trunk_hits
        .iter()
        .filter(|(_, hits)| **hits > 1)
        .map(|(index, hits)| {
            let signal = plan
                .trunks
                .get(*index)
                .map(|trunk| trunk.signal.as_str())
                .unwrap_or("<unknown>");
            format!("{signal} ({hits} cells)")
        })
        .collect();
    lines.push(if shared.is_empty() {
        "  no two stuck cells share a trunk".to_owned()
    } else {
        format!("  stuck cells share a trunk: {}", shared.join(", "))
    });

    Some(lines.join("\n"))
}

/// **What the recursive producer is, as one fingerprint.**
///
/// A case fingerprint that named only the *path* would let this producer be
/// rewritten -- a different grain, a different split, a different packing --
/// and still report the resulting circuit under the identity a baseline was
/// recorded with. Naming the producer closes that: a case compiled by a
/// different recursive generator is a different case.
///
/// [`TERMINAL_GATES`] is folded in by value because it is the one knob whose
/// change is silent otherwise: move it and the same netlist is split
/// differently, with no edit anywhere else to notice. Everything else the
/// producer does -- the halving split, the packed bridge geometry, the lane
/// pitch -- is structure rather than a number, so it answers to the revision
/// string, which must be bumped deliberately when any of it changes. That is
/// a discipline, and this doc is where it is written down.
///
/// Deliberately not derived from any output: a revision read off a compiled
/// world would change whenever the world did, which is the opposite of an
/// identity a baseline can be found under.
///
/// v2: a packed root builds every candidate in a fixed list and ships the
/// one [`pick`] chooses, instead of the first that certifies; the unpinned
/// list ends with the wide fabric candidate at [`WIDE_LEAF_GATES`]. A direct
/// root leaf is chosen the same way from the planner's refresh reserve and
/// exact refresh placement.
pub(crate) fn producer_revision() -> Fingerprint {
    canonical_fingerprint(
        format!(
            "recursive-contract-producer-v2:terminal-gates={TERMINAL_GATES}:\
             wide-leaf-gates={WIDE_LEAF_GATES}:selection=dominance:\
             direct-leaf-refresh=reserve,exact"
        )
        .as_bytes(),
    )
}

/// Which certified candidate a packed root ships.
///
/// `keys` holds each candidate's quality in list order, `None` where the
/// candidate was refused. The reference is the first that certified -- the
/// product the first-success rule shipped before. Only a candidate no worse
/// than the reference on both settle ticks and non-air blocks may replace it,
/// and among those the smallest `(QualityKey, index)` wins. Equivalently:
/// ticks-first [`QualityKey`] order, restricted to candidates with no more
/// blocks than the reference.
///
/// Dominance rather than plain lexicographic order because the acceptance
/// gates are `new <= legacy` on ticks and on blocks separately: a shipped
/// product that is no worse on either than the reference cannot turn a
/// passing gate red, whatever the baseline is, and this function never reads
/// one. Plain lexicographic order would trade any number of blocks for one
/// tick.
///
/// Reads only integer keys and indices -- never time, worker count or a
/// baseline -- so the choice is the same on every machine.
pub(crate) fn pick(keys: &[Option<QualityKey>]) -> Option<usize> {
    let reference = keys.iter().flatten().next()?;
    keys.iter()
        .enumerate()
        .filter_map(|(index, key)| key.as_ref().map(|key| (index, key)))
        .filter(|(_, key)| {
            key.observed_settle <= reference.observed_settle
                && key.non_air_blocks <= reference.non_air_blocks
        })
        .min_by(|(left_index, left), (right_index, right)| {
            left.cmp(right).then(left_index.cmp(right_index))
        })
        .map(|(index, _)| index)
}

/// One packed-root candidate: its label, and how to build and adapt it.
type Candidate<'a> = (
    String,
    Box<dyn FnOnce() -> Result<RecursiveProduct, RecursiveError> + 'a>,
);

/// Build every candidate in `candidates`, in order, and ship the one [`pick`]
/// chooses.
///
/// Each candidate runs to completion on the whole worker budget, one after
/// another, so what each one builds is exactly what it builds alone. Every
/// outcome is recorded on the shipped product's diagnostics. `Err` carries
/// every refusal, in list order, when nothing certified.
fn ship_best(candidates: Vec<Candidate<'_>>) -> Result<RecursiveProduct, Vec<RecursiveError>> {
    let mut labels = Vec::with_capacity(candidates.len());
    let mut outcomes = Vec::with_capacity(candidates.len());
    for (label, build) in candidates {
        let outcome = build();
        if let (Err(error), true) = (&outcome, std::env::var_os("REDA_TRACE_PINNED").is_some()) {
            eprintln!("reda: candidate {label} refused: {error}");
        }
        labels.push(label);
        outcomes.push(outcome);
    }
    let keys: Vec<Option<QualityKey>> = outcomes
        .iter()
        .map(|outcome| outcome.as_ref().ok().map(|product| product.metrics.quality))
        .collect();
    let summary: Vec<CandidateOutcome> = labels
        .into_iter()
        .zip(&outcomes)
        .map(|(label, outcome)| CandidateOutcome {
            label,
            quality: match outcome {
                Ok(product) => Ok(product.metrics.quality),
                Err(error) => Err(error.to_string()),
            },
        })
        .collect();
    let Some(chosen) = pick(&keys) else {
        return Err(outcomes.into_iter().filter_map(Result::err).collect());
    };
    let mut product = outcomes
        .into_iter()
        .nth(chosen)
        .expect("pick chose a listed candidate")
        .expect("pick chose a certified candidate");
    let diagnostics = product.diagnostics.get_or_insert_with(|| RecursiveDiagnostics {
        leaves: Vec::new(),
        root_trunks: Vec::new(),
        candidates: Vec::new(),
        chosen: None,
    });
    diagnostics.candidates = summary;
    diagnostics.chosen = Some(chosen);
    Ok(product)
}

pub(crate) fn compile(
    lowered: &Netlist,
    pins: Option<&PortPlacements>,
    search: &SearchConfig,
) -> Result<RecursiveProduct, RecursiveError> {
    let workers = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(MAX_RECURSIVE_WORKERS);
    compile_with_workers(lowered, pins, search, workers)
}

/// A root small enough to be one leaf: planned and routed directly, with no
/// contract above it.
///
/// A parent exists to hand pieces to children and join them with a corridor.
/// With one piece there is nothing to join, and the corridor, its trunks and
/// its portals are pure overhead -- measured on `and4`, most of the world.
/// This is the same planner and the same leaf router every other node ends at,
/// called once, and certified by the same authority the composed shape answers
/// to: `certify_root_world`, which now takes the root ports rather than a plan
/// precisely so both shapes can reach it.
fn compile_root_leaf(
    lowered: &Netlist,
    pins: &RootPins<'_>,
    search: &SearchConfig,
    workers: usize,
) -> Result<RecursiveProduct, RecursiveError> {
    // Two candidates, as a packed root builds its list: the planner's own
    // refresh reserve first, which is what this leaf always shipped, then
    // exact refresh placement. [`pick`] keeps the second only when it is no
    // worse on ticks and blocks; a refusal of both reports the first.
    let reserved = root_leaf_candidate(lowered, pins, search, workers, false);
    let exact = root_leaf_candidate(lowered, pins, search, workers, true);
    let keys = [
        reserved.as_ref().ok().map(|product| product.metrics.quality),
        exact.as_ref().ok().map(|product| product.metrics.quality),
    ];
    match pick(&keys) {
        Some(1) => exact,
        _ => reserved,
    }
}

/// The direct root leaf, planned with the planner's refresh reserve, or with
/// exact refresh placement when `exact`
/// ([`crate::compile::routing::with_exact_refresh`]).
// ponytail: unboxed like `compile_root_leaf`; see `compile_with_cutoff`.
#[allow(clippy::result_large_err)]
fn root_leaf_candidate(
    lowered: &Netlist,
    pins: &RootPins<'_>,
    search: &SearchConfig,
    workers: usize,
    exact: bool,
) -> Result<RecursiveProduct, RecursiveError> {
    // Caller geometry is refused the same way whatever shape builds it: a pin
    // this contract cannot honour is an `UnsupportedRootPin`, not something the
    // planner is asked to make sense of. The placement itself is unused here --
    // there is no body to put behind the pins -- but the refusal is the same
    // one `honours_pins` reports.
    let caller = pins;
    let pins = caller.compiled();
    root_placement(lowered, pins)?;
    let root = root_chunk_id(lowered)?;
    // A planner refusal here is the leaf router refusing this node, which is
    // the shape `ScheduleError::Leaf` already carries for every other leaf.
    let refuse = |error| {
        RecursiveError::Schedule(ScheduleError::Leaf {
            index: 0,
            chunk: root.clone(),
            error: LeafError::Planner(error),
        })
    };
    let placements = pins.cloned().unwrap_or_default();
    // The planner is asked for a buildable world: no route may step under a
    // cell of its own path, which certification would refuse.
    let plan = || planner::plan_from_netlist(lowered, &placements);
    let candidate = crate::compile::routing::refusing_own_crush(|| {
        if exact {
            crate::compile::routing::with_exact_refresh(plan)
        } else {
            plan()
        }
    })
    .map_err(refuse)?;
    let realised = planner::realise_and_verify(
        &candidate,
        lowered,
        planner::candidate_world_size(&candidate),
    )
    .map_err(refuse)?;

    let mut world = realised.world;
    let mut input_positions = BTreeMap::new();
    let mut output_positions = BTreeMap::new();
    let mut root_ports = Vec::with_capacity(lowered.inputs.len() + lowered.outputs.len());
    // Declared order, inputs then outputs: what `validate_root_ports` compares
    // against, and what a transition's bits are zipped with.
    for signal in &lowered.inputs {
        let at = *realised
            .ports
            .input_positions
            .get(signal)
            .ok_or_else(|| RecursiveError::MissingGate(signal.clone()))?;
        input_positions.insert(signal.clone(), at);
        // The same installer the composed shape uses. An input the planner
        // placed already has the planner's lever, which is kept; one this
        // root pinned for the caller was planned as a caller's cell and is
        // bare, so the lever is stood now.
        //
        // Invariant relied on: the planner realises an unpinned primary input
        // as exactly a `Lever` in the reported input cell, and a pinned one as
        // an empty cell. Those are the two cases the installer accepts. Should
        // the planner ever realise an input a third way, the installer refuses
        // it as `LeverCollision` rather than overwriting it, and this call is
        // where that case must then be decided.
        if !caller.caller_pinned(signal) {
            install_root_lever(&mut world, signal, at)?;
        }
        root_ports.push(root_port(signal, PortRole::Input, at, pins));
    }
    for signal in &lowered.outputs {
        let at = *realised
            .ports
            .output_positions
            .get(signal)
            .ok_or_else(|| RecursiveError::MissingGate(signal.clone()))?;
        output_positions.insert(signal.clone(), at);
        // An output the caller did not pin has the caller-facing receiver in
        // its cell, not the planner's own hardware -- the same lamp the
        // composed shape hangs there, and the convention the acceptance
        // evaluator reads.
        if !caller.caller_pinned(signal) {
            world.set(at.0, at.1, at.2, compile::lamp());
        }
        root_ports.push(root_port(signal, PortRole::Output, at, pins));
    }

    let certificate = certify_root_world(
        &world,
        lowered,
        &root_ports,
        &CertificationConfig::from_search(search),
        CertificationWorkers::bounded(workers),
    )?;

    // The same derivation `synthesise_leaf` uses for every other leaf.
    let gate_facings = (0..lowered.gates.len())
        .map(|gate| candidate.facing_of(gate))
        .collect::<Vec<_>>();
    let gate_output_positions = realised.ports.gate_output_positions.clone();
    for gate in &lowered.gates {
        if !gate_output_positions.contains_key(&gate.output) {
            return Err(RecursiveError::MissingGate(gate.output.clone()));
        }
    }

    Ok(assemble_product(
        RootAssembly {
            world,
            trunks: &[],
            input_positions,
            output_positions,
            gate_output_positions,
            gate_facings,
            depth: 1,
            peak_workers: 1,
            diagnostics: None,
        },
        certificate,
        search,
    ))
}

/// Everything a finished root has, before it is priced.
pub(crate) struct RootAssembly<'a> {
    pub world: World,
    /// The trunks this root laid, in the order it laid them.  The timing
    /// fingerprint is taken over exactly this list, so a shape that laid none
    /// says so with an empty one rather than by skipping the field.
    pub trunks: &'a [RealisedRouteTree],
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_facings: Vec<CellFacing>,
    pub depth: usize,
    pub peak_workers: usize,
    /// What the packed recursive shape built, for a measurement to check
    /// against; the other shapes carry none.
    pub diagnostics: Option<RecursiveDiagnostics>,
}

/// Price a certified root and hand back the product contract.
///
/// The one place the recursive-contract fingerprint scheme, the timing
/// fingerprint and [`CandidateMetrics`] are assembled, so a second shape that
/// reaches this point cannot price itself differently by accident.
pub(crate) fn assemble_product(
    assembly: RootAssembly<'_>,
    certificate: RootCertificate,
    search: &SearchConfig,
) -> RecursiveProduct {
    let RootAssembly {
        world,
        trunks,
        input_positions,
        output_positions,
        gate_output_positions,
        gate_facings,
        depth,
        peak_workers,
        diagnostics,
    } = assembly;
    let timing_fingerprint = canonical_fingerprint(
        &serde_json::to_vec(trunks).expect("a realised route tree list serializes"),
    );
    // The certificate's world fingerprint is authoritative: it names the
    // world that was proven, and pricing over anything else would let a
    // product describe a world other than the one it certified. Rehashing
    // here is a check, not a source, so it is only paid in debug builds.
    debug_assert_eq!(
        certificate.world_fingerprint,
        canonical_world_fingerprint(&world),
        "the certified world is not the world being priced"
    );
    let candidate_fingerprint = canonical_fingerprint(
        format!(
            "recursive-contract-v1:{}:{}",
            certificate.world_fingerprint.as_str(),
            timing_fingerprint.as_str()
        )
        .as_bytes(),
    );
    let worst_transition_indices = certificate
        .measurements
        .iter()
        .filter(|measurement| measurement.settle_game_ticks == certificate.worst_settle_game_ticks)
        .map(|measurement| measurement.manifest_index)
        .collect();
    let metrics = CandidateMetrics {
        quality: QualityKey {
            observed_settle: certificate.worst_settle_game_ticks,
            non_air_blocks: certificate.physical.non_air_blocks,
            occupied_volume: certificate.physical.occupied_volume,
            static_routed_delay: crate::compile::fragment_synth::timing_graph::ExactDelay(
                certificate.worst_settle_game_ticks,
            ),
        },
        transition_manifest_hash: certificate.manifest_fingerprint,
        transition_count: certificate.measurements.len() as u64,
        transition_cap: search.max_certification_transitions,
        worst_transition_indices,
        equivalence_certificate_fingerprint: None,
        realised_timing_graph_fingerprint: timing_fingerprint,
        candidate_fingerprint: candidate_fingerprint.clone(),
        emitted_world_fingerprint: certificate.world_fingerprint,
    };

    RecursiveProduct {
        world,
        input_positions,
        output_positions,
        gate_output_positions,
        gate_facings,
        metrics,
        candidate_fingerprint,
        depth,
        peak_workers,
        diagnostics,
    }
}

/// One root port: the caller's own pin when it pinned one, otherwise the cell
/// the planner realised.
///
/// `toward` is only ever read by geometry this shape does not build --
/// certification reads `pin.at` and the signal -- so an unpinned port carries
/// the placeholder rather than inventing a direction the planner never chose.
fn root_port(
    signal: &str,
    role: PortRole,
    at: (i32, i32, i32),
    pins: Option<&PortPlacements>,
) -> RootPort {
    let pin = pins.and_then(|pins| pins.get(signal)).unwrap_or(PortPin {
        at: Anchor {
            x: at.0,
            y: at.1,
            z: at.2,
        },
        toward: Facing::North,
    });
    RootPort {
        signal: signal.to_owned(),
        role,
        pin,
    }
}

fn compile_with_workers(
    lowered: &Netlist,
    pins: Option<&PortPlacements>,
    search: &SearchConfig,
    workers: usize,
) -> Result<RecursiveProduct, RecursiveError> {
    compile_with_cutoff(lowered, pins, search, workers, TERMINAL_GATES)
}

/// [`compile_with_workers`] with the direct-root-leaf grain named.
///
/// The only thing a caller may vary is where the *direct leaf* arm stops, and
/// only a test does: the packed driver below still splits at its own
/// production grain, so lowering this changes which branch a small netlist
/// takes and nothing about how that branch behaves. Production always passes
/// [`TERMINAL_GATES`].
/// The leaf grids a packed root is built on, in order: every leaf on the
/// densest grid it certifies on, then every leaf on the standard grid. A dense
/// leaf can crowd a trunk its parent then cannot route; the second rung builds
/// exactly what the standard grid always built.
const LEAF_LADDERS: [&[i32]; 2] = [&LEAF_PITCHES, &[STANDARD_PITCH]];

// ponytail: the candidate closures return `RecursiveError` unboxed, like
// every other fallible step here; boxing the large error types is S4's job
// in `docs/optimization-plan.md`, done once for the whole module.
#[allow(clippy::result_large_err)]
fn compile_with_cutoff(
    lowered: &Netlist,
    pins: Option<&PortPlacements>,
    search: &SearchConfig,
    workers: usize,
    direct_leaf_cutoff: usize,
) -> Result<RecursiveProduct, RecursiveError> {
    let gauge = Gauge::default();
    let session = Session {
        search,
        gauge: &gauge,
        workers,
    };
    // The one place a partial pin set is completed, so both root shapes and
    // the packed cutover read the same pins.
    let caller = RootPins::normalise(lowered, pins)?;
    let pins = caller.compiled();
    if lowered.gates.len() <= direct_leaf_cutoff {
        match compile_root_leaf(lowered, &caller, search, workers) {
            // The planner can lay a route's dust on that route's own committed
            // stone (`a_route_lays_dust_on_its_own_committed_stone`), and dust
            // cannot stand on dust. Such a world only runs in the simulator, so
            // the packed shape below builds this root instead.
            Err(RecursiveError::Certification(CandidateCertificationError::Unsupported {
                ..
            })) => {}
            result => return result,
        }
    }
    // **The unpinned cutover.** Above the direct-leaf grain, a root that
    // pinned nothing is built by the packed path: certified children packed by
    // translation, joined by the forced-runway router, certified as a whole,
    // and handed back through the adapter that stands the caller's lever and
    // lamp in the ports it inherited. A root that *did* pin something is not
    // this shape -- packed ports are inherited child cells and cannot satisfy
    // `root_pin_row` -- so it continues down the allocating path untouched.
    // An empty set was normalised to `None` above, so `None` is the whole
    // test.
    if pins.is_none() {
        let root = root_chunk_id(lowered)?;
        // The nested packed producer and the lid fabric, each on dense leaves
        // first and then on the standard grid ([`LEAF_LADDERS`]) -- the order
        // the first-success rule tried them in -- and last the lid fabric on
        // wide leaves, on the same two grids, when that cut differs from the
        // production one. The fabric plans every trunk onto fixed layers with
        // room reserved by demand, so it builds wherever the nested lanes run
        // out of height or room. Every candidate is built and [`pick`]
        // chooses what ships.
        let certification = CertificationConfig::from_search(search);
        let adapt = |product: Result<PackedRecursiveProduct, PackedRecursiveError>|
         -> Result<RecursiveProduct, RecursiveError> {
            Ok(adapt_packed_root(lowered, &product?, None, search, workers)?)
        };
        let mut candidates: Vec<Candidate<'_>> = Vec::new();
        for pitches in LEAF_LADDERS {
            let (root, certification) = (&root, &certification);
            candidates.push((
                format!("nested {pitches:?}"),
                Box::new(move || {
                    adapt(synthesise_packed_recursive_on(
                        lowered,
                        root,
                        &DurablePhysicalRouter,
                        search,
                        certification,
                        workers,
                        pitches,
                    ))
                }),
            ));
            candidates.push((
                format!("fabric {pitches:?}"),
                Box::new(move || {
                    adapt(synthesise_packed_recursive_fabric(
                        lowered,
                        root,
                        None,
                        &DurablePhysicalRouter,
                        search,
                        certification,
                        workers,
                        pitches,
                        LeafCut::PRODUCTION,
                        false,
                    ))
                }),
            ));
        }
        // The refusal a root reports when nothing certifies is the last
        // production fabric rung's, as before the wide candidate existed.
        let reported = candidates.len() - 1;
        if wide_cut_differs(lowered, &root)? {
            for pitches in LEAF_LADDERS {
                let (root, certification) = (&root, &certification);
                candidates.push((
                    format!("fabric wide {pitches:?}"),
                    Box::new(move || {
                        adapt(synthesise_packed_recursive_fabric(
                            lowered,
                            root,
                            None,
                            &DurablePhysicalRouter,
                            search,
                            certification,
                            workers,
                            pitches,
                            LeafCut::WIDE,
                            false,
                        ))
                    }),
                ));
            }
        }
        // Last, the fabric with every leaf placed for timing against the
        // boundary signals the root's critical path crosses (T3a), on the
        // production cut and, when it differs, the wide one.
        let cuts = [(LeafCut::PRODUCTION, "fabric timed"), (LeafCut::WIDE, "fabric wide timed")];
        let wide_differs = wide_cut_differs(lowered, &root)?;
        for (cut, label) in cuts {
            if cut == LeafCut::WIDE && !wide_differs {
                continue;
            }
            let (root, certification) = (&root, &certification);
            candidates.push((
                format!("{label} {:?}", &LEAF_PITCHES),
                Box::new(move || {
                    adapt(synthesise_packed_recursive_fabric(
                        lowered,
                        root,
                        None,
                        &DurablePhysicalRouter,
                        search,
                        certification,
                        workers,
                        &LEAF_PITCHES,
                        cut,
                        true,
                    ))
                }),
            ));
        }
        return ship_best(candidates).map_err(|mut refusals| refusals.swap_remove(reported));
    }
    // **The pinned packed shape.** A pinned root is packed like an unpinned
    // one, then placed inside the rectangle its pins draw, with every port
    // built at the caller's own cell -- so the circuit stands between its
    // inputs and outputs rather than behind the southmost pin. Every
    // candidate is built and [`pick`] chooses what ships.
    //
    // ponytail: a layout that does not fit the rectangle falls back to the
    // allocating shape below; stacking upward and growing toward the pin-free
    // sides are what replace that fallback.
    let short = pins.and_then(|compiled| {
        pinned_floors_short(lowered.gates.len(), &PinnedRoom::of(compiled, &lowered.inputs))
    });
    if let (Some((needs, floors)), true) = (short, std::env::var_os("REDA_TRACE_PINNED").is_some()) {
        eprintln!(
            "reda: the pins leave too little room: {} gates need at least {needs} cells, about {floors} floors; allocating instead",
            lowered.gates.len()
        );
    }
    if let (Some(compiled), None) = (pins, short) {
        let root = root_chunk_id(lowered)?;
        let certification = CertificationConfig::from_search(search);
        let supplied = caller.supplied;
        let adapt = |product: Result<PackedRecursiveProduct, PackedRecursiveError>|
         -> Result<RecursiveProduct, RecursiveError> {
            Ok(adapt_packed_root(lowered, &product?, supplied, search, workers)?)
        };
        let mut candidates: Vec<Candidate<'_>> = Vec::new();
        for pitches in LEAF_LADDERS {
            let (root, certification) = (&root, &certification);
            candidates.push((
                format!("pinned packed {pitches:?}"),
                Box::new(move || {
                    adapt(synthesise_packed_recursive_pinned(
                        lowered,
                        root,
                        compiled,
                        &DurablePhysicalRouter,
                        search,
                        certification,
                        workers,
                        pitches,
                    ))
                }),
            ));
            // The lid fabric inside the same room: each pin joined to a foot
            // by an ordinary search, every trunk from there on planned.
            candidates.push((
                format!("pinned fabric {pitches:?}"),
                Box::new(move || {
                    adapt(synthesise_packed_recursive_fabric(
                        lowered,
                        root,
                        Some(compiled),
                        &DurablePhysicalRouter,
                        search,
                        certification,
                        workers,
                        pitches,
                        LeafCut::PRODUCTION,
                        false,
                    ))
                }),
            ));
        }
        if let Ok(product) = ship_best(candidates) {
            return Ok(product);
        }
    }
    let root = root_chunk_id(lowered)?;
    // The root inherits the caller's own row when the caller pinned one, and
    // places its own when it did not. Either way it answers to no enclosing
    // region, so it is the one node compiled without an extent budget.
    let (plan, _, outcomes) =
        solve_subtree(lowered, &root, pins, split_of(lowered), None, session)?;

    let mut gate_metadata = BTreeMap::new();
    let mut facing_by_output = BTreeMap::new();
    lift_subtree_gates(&plan, &outcomes, &mut gate_metadata, &mut facing_by_output);
    let depth = levels(&outcomes);
    let leaves = outcomes
        .into_iter()
        .map(|outcome| outcome.leaf)
        .collect::<Vec<_>>();
    let composed = compose(&plan, &leaves, &DurablePhysicalRouter, search.router_limits)?;

    let mut world = composed.world;
    let mut input_positions = BTreeMap::new();
    let mut output_positions = BTreeMap::new();
    for port in &plan.root_ports {
        let at = (port.pin.at.x, port.pin.at.y, port.pin.at.z);
        // A cell the caller pinned is the caller's and ships empty; one this
        // root chose, whether the caller pinned nothing or only some ports,
        // gets the caller-facing hardware.
        match port.role {
            PortRole::Input => {
                input_positions.insert(port.signal.clone(), at);
                if !caller.caller_pinned(&port.signal) {
                    install_root_lever(&mut world, &port.signal, at)?;
                }
            }
            PortRole::Output => {
                output_positions.insert(port.signal.clone(), at);
                if !caller.caller_pinned(&port.signal) {
                    world.set(at.0, at.1, at.2, compile::lamp());
                }
            }
        }
    }

    // The only functional authority in the whole recursion.  Children are
    // certified structurally -- their contract, their region -- and never
    // simulated on their own; what is proven correct is the root world.
    // Certification is the last phase and owns the machine on its own: the
    // whole tree has been composed by now, so it may use every worker this
    // synthesis was given. The gauge counts compile-time width, not this.
    let certificate = match certify_root_world(
        &world,
        lowered,
        &plan.root_ports,
        &CertificationConfig::from_search(search),
        CertificationWorkers::bounded(workers),
    ) {
        Ok(certificate) => certificate,
        Err(error) => {
            // Failure path only: say where the stuck cells sit before handing
            // the refusal up unchanged.
            if let Some(mapping) = describe_pending_routes(&plan, &composed.trunks, &error) {
                eprintln!("reda: {mapping}");
            }
            return Err(error.into());
        }
    };

    let gate_facings = lowered
        .gates
        .iter()
        .map(|gate| {
            facing_by_output
                .get(&gate.output)
                .copied()
                .ok_or_else(|| RecursiveError::MissingGate(gate.output.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for gate in &lowered.gates {
        if !gate_metadata.contains_key(&gate.output) {
            return Err(RecursiveError::MissingGate(gate.output.clone()));
        }
    }

    // Priced by the same authority the direct leaf and the packed adapter
    // answer to, over the trunks in the order this root laid them.
    Ok(assemble_product(
        RootAssembly {
            world,
            trunks: &composed.trunks,
            input_positions,
            output_positions,
            gate_output_positions: gate_metadata,
            gate_facings,
            depth,
            peak_workers: gauge.peak(),
            diagnostics: None,
        },
        certificate,
        search,
    ))
}

fn allocation_limits(root: &Netlist, chunks: &[Chunk], search: &SearchConfig) -> AllocationLimits {
    let boundary_signals = chunks
        .iter()
        .flat_map(|chunk| chunk.boundary_inputs.iter().chain(&chunk.boundary_outputs))
        .chain(&root.inputs)
        .chain(&root.outputs)
        .collect::<BTreeSet<_>>()
        .len();
    AllocationLimits {
        delay_budget_ticks: search.max_game_ticks_per_transition as u32,
        corridor_capacity: u32::try_from(boundary_signals.max(3)).unwrap_or(u32::MAX),
    }
}

/// How a chunk gets compiled decides how much room it needs, so the rule that
/// picks terminal-or-parent is written once and read by both the sizing pass
/// and the synthesis.
pub(crate) fn split_of(netlist: &Netlist) -> usize {
    netlist.gates.len().div_ceil(2).max(1)
}

/// Is `needs` inside `budget` on every axis?
fn fits(needs: Anchor, budget: Anchor) -> bool {
    needs.x <= budget.x && needs.y <= budget.y && needs.z <= budget.z
}

fn widen(left: Anchor, right: Anchor) -> Anchor {
    Anchor {
        x: left.x.max(right.x),
        y: left.y.max(right.y),
        z: left.z.max(right.z),
    }
}

fn region_escape(chunk: ChunkId, available: Anchor, at: Anchor) -> RecursiveError {
    if at.x > available.x || at.y > available.y || at.z > available.z {
        RecursiveError::ExtentRefused {
            chunk,
            needed: widen(available, at),
            available,
        }
    } else {
        RecursiveError::Compose(ComposeError::Escape { chunk, at })
    }
}

/// Sizes a child by what this module will actually build inside it.
///
/// A terminal chunk becomes a leaf and needs exactly the world its leaf will
/// realise: the planner candidate [`synthesise_leaf`] runs on these same
/// ports, measured by [`planner::candidate_world_size`].  Not the seed's
/// envelope -- `seed_extent` measures the sparse starting layout relaxation
/// begins from, which on `segment_a`'s halves is several times the footprint
/// the candidate actually settles into, so every sibling was allocated
/// around empty space and every parent trunk spanned it.  Any larger chunk
/// becomes a contract parent, and the first answer to how much room that
/// needs is the layout it will run: the same partition, the same limits, the
/// same inherited caller row, allocated with this same sizer. That recursion
/// terminates because [`split_of`] keeps every level at the preferred grain or
/// raises it just enough to bound fanout.
///
/// A terminal chunk the planner refuses has no extent.  That is reported as
/// [`AllocationError::ChildUnplannable`] naming the chunk, and
/// [`solve_subtree`] repairs it by splitting exactly that child, the same way
/// it answers a child that refused at synthesis.  A refusal from deeper in a
/// nonterminal's sizing names the nonterminal, since that is the only chunk
/// this node's list contains -- the fold [`classify_child_error`] already
/// applies to a nested `ChildRefused`.
///
/// That answer is exact for a subtree that compiles as partitioned, and only
/// for that.  A subtree that has to repair splits a chunk it could not build
/// and lays out wider than it was sized for, which no sizing pass can know in
/// advance without compiling.  `overrides` is where the parent writes down
/// what such a child turned out to need, so the next allocation reserves it:
/// negotiation after the fact, not a worst case reserved for everyone up
/// front.
struct ContractExtent<'a> {
    search: &'a SearchConfig,
    overrides: &'a BTreeMap<ChunkId, Anchor>,
}

impl ChildExtent for ContractExtent<'_> {
    fn extent(&self, chunk: &Chunk, ports: &PortPlacements) -> Result<Anchor, AllocationError> {
        let predicted = if chunk.netlist.gates.len() <= TERMINAL_GATES {
            terminal_extent(chunk, ports)?
        } else {
            let children = partition(&chunk.netlist, &chunk.id, split_of(&chunk.netlist)).map_err(
                |error| AllocationError::ChildExtent {
                    chunk: chunk.id.clone(),
                    error: error.to_string(),
                },
            )?;
            let plan = allocate_with(
                &chunk.netlist,
                &children,
                allocation_limits(&chunk.netlist, &children, self.search),
                Some(ports),
                self,
            )
            .map_err(|error| match error {
                AllocationError::ChildUnplannable {
                    chunk: inner,
                    error,
                } => AllocationError::ChildUnplannable {
                    chunk: chunk.id.clone(),
                    error: format!("nested {inner:?}: {error}"),
                },
                other => other,
            })?;
            plan.local_extent()
        };
        Ok(match self.overrides.get(&chunk.id) {
            Some(negotiated) => widen(predicted, *negotiated),
            None => predicted,
        })
    }
}

/// The far corner of the world [`synthesise_leaf`] will realise for `chunk`
/// on `ports`: the candidate the leaf runs, measured the way the root leaf
/// measures its own.  The allocator still lifts this to its own minimums for
/// portal access and the router ceiling.
fn terminal_extent(chunk: &Chunk, ports: &PortPlacements) -> Result<Anchor, AllocationError> {
    let candidate = planner::plan_from_netlist(&chunk.netlist, ports).map_err(|error| {
        AllocationError::ChildUnplannable {
            chunk: chunk.id.clone(),
            error: error.to_string(),
        }
    })?;
    let (x, y, z) = planner::candidate_world_size(&candidate);
    Ok(Anchor {
        x: x - 1,
        y: y - 1,
        z: z - 1,
    })
}

/// Halve the refused child in place, or report the one-gate refusal that
/// ends the repair.  Shared by the sizing and synthesis refusals so both
/// shrink a child by exactly the same rule.
///
/// The floor is one gate, deliberately, not `TERMINAL_GATES`: the preferred
/// grain says how large a leaf the router is *asked* for, and a refusal is
/// the router saying it could not build that one.  Stopping at the preferred
/// grain would hand the same refused eight-gate chunk back unchanged;
/// splitting to 4, then 2, then 1 is the whole repair.
fn split_refused_child(
    chunks: &mut Vec<Chunk>,
    index: usize,
    chunk: ChunkId,
    error: String,
) -> Result<(), RecursiveError> {
    let at = chunks
        .iter()
        .position(|candidate| candidate.id == chunk)
        .expect("refused chunk exists");
    let gates = chunks[at].netlist.gates.len();
    if gates <= 1 {
        return Err(RecursiveError::ChildRefused {
            index,
            chunk,
            error,
        });
    }
    let replacement = partition(
        &chunks[at].netlist,
        &chunks[at].id,
        gates.div_ceil(2).max(1),
    )?;
    chunks.splice(at..=at, replacement);
    chunks.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(())
}

/// This node's own contract levels: one for itself, plus the deepest child.
fn levels(outcomes: &[NodeOutcome]) -> usize {
    1 + outcomes
        .iter()
        .map(|outcome| outcome.depth)
        .max()
        .unwrap_or(0)
}

/// Translate every child's subtree metadata into this node's frame.
///
/// `plan.children` and `outcomes` are the same list in the same order --
/// [`synthesise_recursive_children`] files its results by plan index -- so the
/// zip is the allocation each outcome was compiled against.
fn lift_subtree_gates(
    plan: &AllocationPlan,
    outcomes: &[NodeOutcome],
    positions: &mut BTreeMap<String, (i32, i32, i32)>,
    facings: &mut BTreeMap<String, CellFacing>,
) {
    for (allocation, outcome) in plan.children.iter().zip(outcomes) {
        for (gate, &(x, y, z)) in &outcome.leaf.gate_output_positions {
            positions.insert(
                gate.clone(),
                (
                    x + allocation.origin.x,
                    y + allocation.origin.y,
                    z + allocation.origin.z,
                ),
            );
        }
        facings.extend(
            outcome
                .facings
                .iter()
                .map(|(gate, facing)| (gate.clone(), *facing)),
        );
    }
}

/// One node's entire contract, run identically by the root and by every nested
/// node: partition into children, allocate them, recurse, and settle with the
/// two children who cannot build what they were given.
///
/// **Repair**, when a child refuses to compile at all: the refused chunk is
/// split in half in this node's own child list.  Nothing above is re-
/// partitioned and no sibling's gates move, but the whole list is reallocated,
/// because a longer child list is a different layout -- siblings do shift, and
/// are rebuilt against the contract they actually get.  Each repair strictly
/// reduces the refused chunk, so a one-gate refusal is the terminal typed
/// failure and wall time never participates in termination.  That refusal
/// propagates to the immediate parent, which repairs *its* child -- this
/// node's own chunk -- by the same rule, on something strictly smaller again.
///
/// **Negotiation**, when a child compiled but its repaired layout no longer
/// fits the region this node reserved for it: the child returns
/// [`RecursiveError::ExtentRefused`] naming what it now needs, this node
/// records that against its chunk id, and reallocates.  The alternative is to
/// reserve every child's worst case permanently, which every sibling then pays
/// for in a wider corridor and longer trunks whether or not anyone repairs.
///
/// Termination is explicit rather than inherited.  Repairs are bounded by the
/// gate count, since each strictly shrinks a chunk that cannot go below one
/// gate.  Negotiations are bounded by [`negotiation_cap`] and refused by type
/// when it is reached, so neither loop depends on the other converging.
///
/// Returns the allocation this node's trunks belong to, the child list after
/// repair, and one outcome per child in plan order.
fn solve_subtree(
    netlist: &Netlist,
    id: &ChunkId,
    fixed_root_ports: Option<&PortPlacements>,
    first_split: usize,
    budget: Option<Anchor>,
    session: Session<'_>,
) -> Result<(AllocationPlan, Vec<Chunk>, Vec<NodeOutcome>), RecursiveError> {
    let mut chunks = partition(netlist, id, first_split)?;
    let mut overrides: BTreeMap<ChunkId, Anchor> = BTreeMap::new();
    let cap = negotiation_cap(netlist);
    let mut negotiations = 0usize;
    loop {
        let plan = match allocate_with(
            netlist,
            &chunks,
            allocation_limits(netlist, &chunks, session.search),
            fixed_root_ports,
            &ContractExtent {
                search: session.search,
                overrides: &overrides,
            },
        ) {
            Ok(plan) => plan,
            // A child the planner cannot lay out at all has no extent, and
            // so no allocation: the same refusal the leaf router would give
            // at synthesis, caught one phase earlier, and repaired the same
            // way.
            Err(AllocationError::ChildUnplannable { chunk, error }) => {
                let index = chunks
                    .iter()
                    .position(|candidate| candidate.id == chunk)
                    .expect("unplannable chunk exists");
                split_refused_child(&mut chunks, index, chunk, error)?;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        // Refused before any child is compiled: what this node would build no
        // longer fits what its own parent reserved, and only that parent can
        // widen it.
        if let Some(budget) = budget {
            let needed = plan.local_extent();
            if !fits(needed, budget) {
                return Err(RecursiveError::ExtentRefused {
                    chunk: id.clone(),
                    needed,
                    available: budget,
                });
            }
        }
        match synthesise_recursive_children(&chunks, &plan, session) {
            Ok(outcomes) => return Ok((plan, chunks, outcomes)),
            Err(RecursiveError::ExtentRefused {
                chunk,
                needed,
                available,
            }) => {
                negotiations += 1;
                if negotiations > cap {
                    return Err(RecursiveError::ExtentNegotiationExhausted {
                        chunk,
                        attempts: negotiations,
                    });
                }
                let widened = widen(needed, available);
                overrides
                    .entry(chunk)
                    .and_modify(|room| *room = widen(*room, widened))
                    .or_insert(widened);
            }
            Err(RecursiveError::ChildRefused {
                index,
                chunk,
                error,
            }) => split_refused_child(&mut chunks, index, chunk, error)?,
            Err(error) => return Err(error),
        }
    }
}

/// How many times one node will reallocate for children that outgrew their
/// regions before it refuses by type.
///
/// Every negotiation is caused by one child repair, and repairs across a
/// subtree are bounded by its gate count because each one strictly shrinks a
/// chunk that cannot go below a single gate.  The `+ 2` covers the smallest
/// netlists, where the bound would otherwise be tighter than a single
/// legitimate round.
fn negotiation_cap(netlist: &Netlist) -> usize {
    2 * netlist.gates.len() + 2
}

/// Compile one level in stable plan order.  Each child may recurse again, but
/// results are filed by index and only then returned, so worker count and
/// completion timing cannot affect composition.
fn synthesise_recursive_children(
    chunks: &[Chunk],
    plan: &AllocationPlan,
    session: Session<'_>,
) -> Result<Vec<NodeOutcome>, RecursiveError> {
    if session.workers == 0 {
        return Err(RecursiveError::Schedule(ScheduleError::ZeroWorkers));
    }
    let by_id = chunks
        .iter()
        .map(|chunk| (&chunk.id, chunk))
        .collect::<BTreeMap<_, _>>();
    let jobs = plan
        .children
        .iter()
        .map(|allocation| {
            by_id
                .get(&allocation.chunk)
                .copied()
                .map(|chunk| (chunk, allocation))
                .ok_or_else(|| {
                    RecursiveError::Schedule(ScheduleError::MissingChunk {
                        chunk: allocation.chunk.clone(),
                    })
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let workers = session.workers.min(jobs.len().max(1));
    session.gauge.record(workers);
    let nested = Session { workers, ..session }.nested();
    let mut slots = (0..jobs.len()).map(|_| None).collect::<Vec<_>>();
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|worker| {
                let jobs = &jobs;
                scope.spawn(move || {
                    (worker..jobs.len())
                        .step_by(workers)
                        .map(|index| {
                            let (chunk, allocation) = jobs[index];
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    synthesise_node(chunk, allocation, nested)
                                }))
                                .map_err(|_| RecursiveError::ChildPanicked {
                                    index,
                                    chunk: chunk.id.clone(),
                                })
                                .and_then(|result| {
                                    result.map_err(|error| {
                                        classify_child_error(index, &chunk.id, error)
                                    })
                                });
                            (index, result)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            for (index, result) in handle
                .join()
                .expect("recursive worker panicked outside guard")
            {
                slots[index] = Some(result);
            }
        }
    });
    let mut outcomes = Vec::with_capacity(slots.len());
    for (index, result) in slots.into_iter().enumerate() {
        outcomes.push(
            result
                .expect("every recursive job files a result")
                .map_err(|error| match error {
                    RecursiveError::ChildRefused { error, .. } => RecursiveError::ChildRefused {
                        index,
                        chunk: jobs[index].0.id.clone(),
                        error,
                    },
                    RecursiveError::ChildPanicked { .. } => RecursiveError::ChildPanicked {
                        index,
                        chunk: jobs[index].0.id.clone(),
                    },
                    other => other,
                })?,
        );
    }
    Ok(outcomes)
}

fn classify_child_error(
    index: usize,
    immediate_chunk: &ChunkId,
    error: RecursiveError,
) -> RecursiveError {
    match error {
        // Negotiable, and only this node can answer it: pass it up as it
        // stands rather than as one more refusal.
        RecursiveError::ExtentRefused {
            chunk,
            needed,
            available,
        } if &chunk == immediate_chunk => RecursiveError::ExtentRefused {
            chunk,
            needed,
            available,
        },
        other => RecursiveError::ChildRefused {
            index,
            chunk: immediate_chunk.clone(),
            error: other.to_string(),
        },
    }
}

/// Terminal nodes use the existing leaf router.  Non-terminals run the same
/// [`solve_subtree`] the root runs -- with the parent's exact caller row as
/// their fixed ports and the parent's region as their extent budget -- then
/// compose the returned artifacts using the one existing parent router, and
/// hand their parent a single artifact.
fn synthesise_node(
    chunk: &Chunk,
    allocation: &ChildAllocation,
    session: Session<'_>,
) -> Result<NodeOutcome, RecursiveError> {
    if chunk.netlist.gates.len() <= TERMINAL_GATES {
        let leaf = synthesise_leaf(chunk, allocation).map_err(|error| {
            RecursiveError::Schedule(ScheduleError::Leaf {
                index: 0,
                chunk: chunk.id.clone(),
                error,
            })
        })?;
        let facings = chunk
            .netlist
            .gates
            .iter()
            .map(|gate| gate.output.clone())
            .zip(leaf.gate_facings.iter().copied())
            .collect();
        return Ok(NodeOutcome {
            leaf,
            facings,
            depth: 1,
        });
    }

    // A nested node must keep the exact portal cells its parent owns, so it
    // inherits the caller row instead of placing one.
    let ports = allocation.port_placements();
    let region = allocation.local_region();
    let (plan, _, outcomes) = solve_subtree(
        &chunk.netlist,
        &chunk.id,
        Some(&ports),
        split_of(&chunk.netlist),
        Some(region.max),
        session,
    )?;

    let mut gate_output_positions = BTreeMap::new();
    let mut facings = BTreeMap::new();
    lift_subtree_gates(&plan, &outcomes, &mut gate_output_positions, &mut facings);
    let depth = levels(&outcomes);
    let leaves = outcomes
        .into_iter()
        .map(|outcome| outcome.leaf)
        .collect::<Vec<_>>();
    let composed = compose(
        &plan,
        &leaves,
        &DurablePhysicalRouter,
        session.search.router_limits,
    )?;

    // This node is a child like any other: everything it built, including the
    // trunks it owns, has to stay inside the region its parent allocated. The
    // extent check above is what makes this reachable only by a genuine
    // routing escape rather than by a layout that was never going to fit.
    let (sx, sy, sz) = composed.world.size();
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                if composed.world.get(x, y, z).kind == BlockKind::Air {
                    continue;
                }
                let at = Anchor { x, y, z };
                if !region.contains(at) {
                    return Err(region_escape(chunk.id.clone(), region.max, at));
                }
            }
        }
    }

    let gate_facings = chunk
        .netlist
        .gates
        .iter()
        .map(|gate| {
            facings
                .get(&gate.output)
                .copied()
                .ok_or_else(|| RecursiveError::MissingGate(gate.output.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NodeOutcome {
        leaf: LeafArtifact {
            chunk: chunk.id.clone(),
            world: composed.world,
            gate_output_positions,
            gate_facings,
        },
        facings,
        depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::fragment_synth::allocation::SeedExtent;
    use crate::compile::fragment_synth::benchmark::baked_benchmark_evaluator;
    use crate::compile::planner::PortPlacements;
    use crate::compile::{Gate, Netlist};
    use crate::redstone::world::block::Facing;

    fn key(ticks: u64, blocks: u64, volume: u64) -> Option<QualityKey> {
        Some(QualityKey {
            observed_settle: ticks,
            non_air_blocks: blocks,
            occupied_volume: volume,
            static_routed_delay: crate::compile::fragment_synth::timing_graph::ExactDelay(ticks),
        })
    }

    /// The candidate lists measured on the three packed acceptance cases at
    /// v1, in list order: the first-success product is the only one no
    /// candidate after it dominates, so it is what ships.
    #[test]
    fn pick_keeps_the_first_certified_candidate_when_nothing_dominates_it() {
        let segment_a = [None, key(124, 9_305, 391_680), None, key(148, 10_973, 443_592)];
        let pinned = [None, key(190, 20_410, 1_025_780), None, key(240, 24_204, 1_161_440)];
        let seven_segment = [None, None, None, key(198, 21_847, 707_850)];
        assert_eq!(pick(&segment_a), Some(1));
        assert_eq!(pick(&pinned), Some(1));
        assert_eq!(pick(&seven_segment), Some(3));
        assert_eq!(pick(&[key(9, 9, 9)]), Some(0));
    }

    #[test]
    fn pick_does_not_trade_blocks_for_ticks() {
        assert_eq!(pick(&[key(124, 9_305, 1), key(100, 9_400, 1)]), Some(0));
        assert_eq!(pick(&[key(124, 9_305, 1), key(130, 6_000, 1)]), Some(0));
    }

    #[test]
    fn pick_ships_a_dominating_candidate_by_quality_then_index() {
        // The wide fabric candidate appended after the production four.
        let segment_a = [
            None,
            key(124, 9_305, 391_680),
            None,
            key(148, 10_973, 443_592),
            key(80, 4_183, 1),
        ];
        assert_eq!(pick(&segment_a), Some(4));
        // Among dominating candidates, ticks first, then blocks, then volume.
        assert_eq!(pick(&[key(124, 9_305, 9), key(110, 9_000, 9), key(110, 8_000, 9)]), Some(2));
        assert_eq!(pick(&[key(124, 9_305, 9), key(124, 9_305, 8)]), Some(1));
        // A full tie keeps the earlier candidate.
        assert_eq!(pick(&[key(7, 7, 7), key(7, 7, 7)]), Some(0));
    }

    #[test]
    fn pick_chooses_nothing_when_nothing_certified() {
        assert_eq!(pick(&[None, None]), None);
        assert_eq!(pick(&[]), None);
    }

    /// Whatever the legacy baseline is, the shipped product passes every
    /// ticks and blocks gate the first-success product passes.
    #[test]
    fn pick_never_turns_a_passing_gate_red() {
        let values = [10u64, 20, 30];
        let options: Vec<Option<QualityKey>> = std::iter::once(None)
            .chain(values.iter().flat_map(|&t| values.iter().map(move |&b| key(t, b, 1))))
            .collect();
        for a in &options {
            for b in &options {
                for c in &options {
                    let keys = [*a, *b, *c];
                    let Some(chosen) = pick(&keys) else { continue };
                    let reference = keys.iter().flatten().next().unwrap();
                    let shipped = keys[chosen].unwrap();
                    for legacy in [5u64, 10, 15, 20, 25, 30, 35] {
                        assert!(
                            reference.observed_settle > legacy || shipped.observed_settle <= legacy
                        );
                        assert!(
                            reference.non_air_blocks > legacy || shipped.non_air_blocks <= legacy
                        );
                    }
                }
            }
        }
    }

    /// Compile through the production entry point with only the direct-leaf
    /// cutoff lowered, so a small netlist provably takes the packed branch.
    ///
    /// Nothing else moves: `TERMINAL_GATES` is untouched, the packed driver
    /// still splits at its own production grain, and the branch under test is
    /// the one production takes.
    fn compile_packed_branch(
        net: &Netlist,
        workers: usize,
    ) -> Result<RecursiveProduct, RecursiveError> {
        compile_with_cutoff(
            net,
            None,
            &SearchConfig::checked_defaults(),
            workers,
            // One gate: anything with two or more is above the direct-leaf
            // arm and reaches the cutover.
            1,
        )
    }

    /// Drive every declared input from its lever and read every declared
    /// output from its lamp, in the shipped world.
    fn observe_product(
        product: &RecursiveProduct,
        inputs: &[(&str, bool)],
    ) -> BTreeMap<String, bool> {
        let mut world = product.world.clone();
        for (signal, bit) in inputs {
            let at = product.input_positions[*signal];
            let mut lever = world.get(at.0, at.1, at.2).clone();
            assert_eq!(
                lever.kind,
                crate::redstone::world::block::BlockKind::Lever,
                "{signal} must be a lever"
            );
            lever.lit = *bit;
            world.set(at.0, at.1, at.2, lever);
        }
        let mut simulator = crate::redstone::simulator::Simulator::new(world);
        simulator
            .run_until_stable(600)
            .expect("the circuit settles");
        product
            .output_positions
            .iter()
            .map(|(signal, at)| (signal.clone(), simulator.world().get(at.0, at.1, at.2).lit))
            .collect()
    }

    /// **The cutover.** An unpinned root above the direct-leaf grain is built
    /// by the packed path, and what comes back is a complete product: real
    /// hardware at every port, the whole truth table, and the same answer at
    /// one worker and at four.
    #[test]
    fn an_unpinned_root_above_the_direct_leaf_grain_takes_the_packed_path() {
        // Split operands: `z` reads one of its two inputs from each half, so
        // the old allocating path and the packed path build visibly different
        // shapes and the packed one is the one under test.
        let net = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["z".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["y"]),
                Gate::nor("z", &["a", "b"]),
            ],
        };

        let product = compile_packed_branch(&net, 1).expect("the packed branch builds");

        // The packed path's evidence: it recurses, so it is not the direct
        // leaf arm, and every field the contract declares is filled.
        assert!(
            product.depth > 1,
            "depth {} is not a contract",
            product.depth
        );
        assert_eq!(product.gate_facings.len(), net.gates.len());
        assert_eq!(product.gate_output_positions.len(), net.gates.len());
        assert_eq!(product.input_positions.len(), 2);
        assert_eq!(product.output_positions.len(), 1);
        assert!(product.metrics.transition_count > 0);
        assert_eq!(
            product.metrics.candidate_fingerprint,
            product.candidate_fingerprint
        );
        assert_eq!(
            product.metrics.emitted_world_fingerprint,
            canonical_world_fingerprint(&product.world)
        );
        for at in product.output_positions.values() {
            assert_eq!(
                product.world.get(at.0, at.1, at.2).kind,
                crate::redstone::world::block::BlockKind::Lamp
            );
        }

        // The whole truth table, read where a caller reads it.
        for x in [false, true] {
            for y in [false, true] {
                // a = !x, b = !y, z = !(!x | !y) = x AND y.
                assert_eq!(
                    observe_product(&product, &[("x", x), ("y", y)])["z"],
                    x && y,
                    "x={x} y={y}"
                );
            }
        }

        // Worker count is not an input.
        let parallel = compile_packed_branch(&net, 4).expect("the packed branch builds");
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
    }

    /// A pinned root above the same grain still goes down the allocating
    /// path: the packed shape cannot honour a pin, and the cutover says so by
    /// not taking it.
    #[test]
    fn a_pinned_root_whose_pins_leave_no_room_falls_back_to_the_allocating_path() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        };
        let mut pins = PortPlacements::default();
        pins.pin("x", Anchor { x: 1, y: 1, z: 0 }, Facing::South);
        pins.pin("b", Anchor { x: 4, y: 1, z: 0 }, Facing::North);

        // Pins three cells apart leave no rectangle a layout fits, so the
        // packed branch refuses and the allocating path answers: whatever it
        // returns, it is not a packed refusal.
        let outcome =
            compile_with_cutoff(&net, Some(&pins), &SearchConfig::checked_defaults(), 1, 1);
        assert!(
            !matches!(
                outcome,
                Err(RecursiveError::PackedAdapter(_)) | Err(RecursiveError::PackedRecursive(_))
            ),
            "a pinned root that cannot fit surfaced a packed refusal"
        );
    }

    /// A pinned root with room is packed inside the rectangle its pins draw:
    /// every port stands at the caller's own cell and ships empty, every gate
    /// stands inside the rectangle, and the root world certifies.
    #[test]
    fn a_pinned_root_with_room_is_packed_between_its_pins() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        };
        let (input, output) = (Anchor { x: 2, y: 1, z: 2 }, Anchor { x: 120, y: 1, z: 120 });
        let mut pins = PortPlacements::default();
        pins.pin("x", input, Facing::East);
        pins.pin("b", output, Facing::East);

        let product =
            compile_with_cutoff(&net, Some(&pins), &SearchConfig::checked_defaults(), 1, 1)
                .expect("the pinned packed branch builds");
        assert_eq!(product.input_positions["x"], (input.x, input.y, input.z));
        assert_eq!(product.output_positions["b"], (output.x, output.y, output.z));
        for at in [input, output] {
            assert_eq!(product.world.get(at.x, at.y, at.z).kind, BlockKind::Air);
        }
        for (gate, &(x, _, z)) in &product.gate_output_positions {
            assert!(
                (input.x..=output.x).contains(&x) && (input.z..=output.z).contains(&z),
                "gate {gate} at ({x}, {z}) stands outside the pins' rectangle"
            );
        }
    }

    /// **The pinned allocating branch is priced by the one authority.** A
    /// pinned root above the direct-leaf grain composes its children and
    /// routes its own trunks, and what it hands back must carry exactly what
    /// `assemble_product` says it does: the candidate fingerprint derived
    /// from the shipped world and the trunks it laid, the metrics naming that
    /// same fingerprint, and the certificate's own world fingerprint. It must
    /// say the same thing at one worker and at many.
    #[test]
    fn a_pinned_allocating_root_is_priced_the_same_at_one_worker_and_many() {
        // Four gates: above a cutoff of one, so not the direct leaf; split in
        // half by `split_of`, so two children a parallel arm can run at once.
        let netlist = chain(4);
        let pins = caller_row(&netlist, 7);
        let search = SearchConfig::checked_defaults();
        let compile = |workers| {
            compile_with_cutoff(&netlist, Some(&pins), &search, workers, 1)
                .expect("the pinned allocating branch builds")
        };

        let serial = compile(1);
        let parallel = compile(many_workers());

        // The allocating path, not the direct leaf and not the packed shape.
        assert!(serial.depth > 1, "depth {}", serial.depth);
        assert_eq!(serial.peak_workers, 1, "the serial arm must be serial");
        assert!(
            parallel.peak_workers > 1,
            "nothing ran in parallel: peak {}",
            parallel.peak_workers
        );

        // `assemble_product`'s invariants, on each arm.
        for product in [&serial, &parallel] {
            let world_fingerprint = canonical_world_fingerprint(&product.world);
            assert_eq!(product.metrics.emitted_world_fingerprint, world_fingerprint);
            assert_eq!(
                product.metrics.candidate_fingerprint,
                product.candidate_fingerprint
            );
            assert_eq!(
                product.candidate_fingerprint,
                canonical_fingerprint(
                    format!(
                        "recursive-contract-v1:{}:{}",
                        world_fingerprint.as_str(),
                        product.metrics.realised_timing_graph_fingerprint.as_str()
                    )
                    .as_bytes()
                ),
                "the candidate fingerprint is not the recursive-contract scheme"
            );
            assert_eq!(
                product.metrics.quality.static_routed_delay.0,
                product.metrics.quality.observed_settle
            );
            assert_eq!(
                product.metrics.transition_cap,
                search.max_certification_transitions
            );
            assert!(product.metrics.transition_count > 0);
            // A root that laid trunks does not carry the empty list's timing
            // fingerprint.
            assert_ne!(
                product.metrics.realised_timing_graph_fingerprint,
                canonical_fingerprint(&serde_json::to_vec::<[RealisedRouteTree]>(&[]).unwrap()),
                "the pinned allocating root priced no trunks"
            );
            let worst = &product.metrics.worst_transition_indices;
            assert!(!worst.is_empty());
            assert!(
                worst.windows(2).all(|pair| pair[0] < pair[1]),
                "worst transition indices are not sorted and unique: {worst:?}"
            );
            assert!(
                worst
                    .iter()
                    .all(|&index| (index as u64) < product.metrics.transition_count),
                "worst transition index out of range: {worst:?}"
            );
            assert_eq!(product.metrics.equivalence_certificate_fingerprint, None);
            assert_eq!(product.gate_facings.len(), netlist.gates.len());
            assert_eq!(product.gate_output_positions.len(), netlist.gates.len());
            // Pinned ports are reported where the caller put them, under the
            // role the netlist declares for them.
            for (signal, pin) in pins.iter() {
                let at = (pin.at.x, pin.at.y, pin.at.z);
                if netlist.inputs.contains(signal) {
                    assert_eq!(
                        product.input_positions.get(signal),
                        Some(&at),
                        "input {signal} was relocated"
                    );
                    assert!(!product.output_positions.contains_key(signal));
                } else {
                    assert!(netlist.outputs.contains(signal), "{signal} is not a port");
                    assert_eq!(
                        product.output_positions.get(signal),
                        Some(&at),
                        "output {signal} was relocated"
                    );
                    assert!(!product.input_positions.contains_key(signal));
                }
            }
        }

        // Worker count is not an input to the product.
        assert_eq!(serial.candidate_fingerprint, parallel.candidate_fingerprint);
        assert_eq!(serial.metrics, parallel.metrics);
        assert_eq!(serial.depth, parallel.depth);
        assert_eq!(serial.world.size(), parallel.world.size());
        assert_eq!(serial.world.cells(), parallel.world.cells());
        assert_eq!(serial.input_positions, parallel.input_positions);
        assert_eq!(serial.output_positions, parallel.output_positions);
        assert_eq!(serial.gate_output_positions, parallel.gate_output_positions);
        assert_eq!(serial.gate_facings, parallel.gate_facings);
    }

    fn chain(gates: usize) -> Netlist {
        let mut names = (0..=gates).map(|i| format!("s{i}")).collect::<Vec<_>>();
        let input = names.remove(0);
        let output = names.last().cloned().unwrap();
        let gates = names
            .iter()
            .enumerate()
            .map(|(index, output)| {
                let input = if index == 0 {
                    input.clone()
                } else {
                    names[index - 1].clone()
                };
                Gate::nor(output, &[&input])
            })
            .collect();
        Netlist {
            inputs: vec![input],
            outputs: vec![output],
            gates,
        }
    }

    /// Every worker this machine has, and never fewer than two -- a "parallel"
    /// arm that silently ran one worker would assert nothing.
    fn many_workers() -> usize {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .max(2)
    }

    fn session_for<'a>(search: &'a SearchConfig, gauge: &'a Gauge) -> Session<'a> {
        Session {
            search,
            gauge,
            workers: 1,
        }
    }

    /// A caller row this contract can honour: one row, ports one portal pitch
    /// apart, each handing over into the corridor behind it.
    fn caller_row(netlist: &Netlist, z: i32) -> PortPlacements {
        let mut pins = PortPlacements::default();
        let declared = netlist
            .inputs
            .iter()
            .map(|signal| (signal, Facing::South))
            .chain(netlist.outputs.iter().map(|signal| (signal, Facing::North)));
        for (slot, (signal, toward)) in declared.enumerate() {
            pins.pin(
                signal.clone(),
                Anchor {
                    x: 1 + 3 * slot as i32,
                    y: 1,
                    z,
                },
                toward,
            );
        }
        pins
    }

    /// A pinned small root keeps the caller's own cells untouched.
    #[test]
    fn a_pinned_small_root_leaves_its_caller_cells_alone() {
        let netlist = chain(2);
        let pins = caller_row(&netlist, 7);
        let product =
            compile_with_workers(&netlist, Some(&pins), &SearchConfig::checked_defaults(), 1)
                .unwrap();

        assert_eq!(product.depth, 1);
        for (signal, pin) in pins.iter() {
            assert_eq!(
                product.world.get(pin.at.x, pin.at.y, pin.at.z).kind,
                BlockKind::Air,
                "{signal}'s caller cell must ship empty"
            );
            let at = (pin.at.x, pin.at.y, pin.at.z);
            assert!(
                product.input_positions.get(signal) == Some(&at)
                    || product.output_positions.get(signal) == Some(&at),
                "{signal} must be reported at the cell the caller pinned"
            );
        }
    }

    /// An empty pin set is the unpinned root, not a contract that pinned
    /// nothing: the acceptance fixtures hand one over for every unpinned case.
    #[test]
    fn an_empty_pin_set_compiles_as_the_unpinned_root() {
        let netlist = chain(2);
        let search = SearchConfig::checked_defaults();
        let empty = PortPlacements::default();
        let pinned = compile_with_workers(&netlist, Some(&empty), &search, 1)
            .expect("an empty pin set compiles");
        let unpinned = compile_with_workers(&netlist, None, &search, 1).unwrap();
        assert_eq!(pinned.candidate_fingerprint, unpinned.candidate_fingerprint);
        assert_eq!(pinned.input_positions, unpinned.input_positions);
        assert_eq!(pinned.output_positions, unpinned.output_positions);
    }

    /// A partial pin set is honoured cell for cell, and every port the caller
    /// left out is placed once, deterministically, where the same validator
    /// the complete set answers to accepts it.
    #[test]
    fn partial_pins_are_honoured_and_missing_ports_placed_deterministically() {
        use crate::compile::fragment_synth::allocation::{RootAccess, RootPlacement};

        let netlist = chain(2);
        let input = netlist.inputs[0].clone();
        let output = netlist.outputs[0].clone();
        let mut partial = PortPlacements::default();
        partial.pin(input.clone(), Anchor { x: 1, y: 1, z: 7 }, Facing::South);

        // The completion is the caller's pin plus the first free cell along
        // its row: one portal pitch east, on the same row, facing north.
        let completed = normalise_root_pins(&netlist, Some(&partial))
            .unwrap()
            .expect("a partial set is completed");
        assert_eq!(
            completed.get(&input),
            partial.get(&input),
            "the supplied pin was moved"
        );
        assert_eq!(
            completed.get(&output),
            Some(PortPin {
                at: Anchor { x: 4, y: 1, z: 7 },
                toward: Facing::North,
            })
        );
        assert!(matches!(
            root_placement(&netlist, Some(&completed)),
            Ok(RootPlacement {
                caller_row_z: 7,
                access: RootAccess::CallerRow,
            })
        ));
        assert_eq!(
            normalise_root_pins(&netlist, Some(&partial))
                .unwrap()
                .unwrap()
                .get(&output),
            completed.get(&output),
            "completion is not deterministic"
        );

        let search = SearchConfig::checked_defaults();
        let product = compile_with_workers(&netlist, Some(&partial), &search, 1)
            .expect("a partial pin set compiles");
        assert_eq!(product.depth, 1);
        assert_eq!(product.input_positions.get(&input), Some(&(1, 1, 7)));
        assert_eq!(product.output_positions.get(&output), Some(&(4, 1, 7)));
        // The caller's cell is the caller's; the one this root chose for the
        // caller carries the receiver an unpinned root ships.
        assert_eq!(product.world.get(1, 1, 7).kind, BlockKind::Air);
        assert_eq!(product.world.get(4, 1, 7).kind, compile::lamp().kind);
        let again = compile_with_workers(&netlist, Some(&partial), &search, 1).unwrap();
        assert_eq!(product.candidate_fingerprint, again.candidate_fingerprint);
    }

    /// Leaving ports unpinned does not soften the check on the ones that are
    /// pinned: an unsupported or undeclared supplied pin is refused by type,
    /// naming the caller's pin, with nothing placed around it.
    #[test]
    fn an_unsupported_supplied_pin_refuses_however_many_ports_are_missing() {
        let netlist = chain(2);
        let output = netlist.outputs[0].clone();
        let search = SearchConfig::checked_defaults();

        let mut off_plane = PortPlacements::default();
        off_plane.pin(output.clone(), Anchor { x: 4, y: 0, z: 7 }, Facing::North);
        assert!(!honours_pins(&netlist, Some(&off_plane)));
        match compile_with_workers(&netlist, Some(&off_plane), &search, 1) {
            Err(RecursiveError::Allocation(AllocationError::UnsupportedRootPin {
                signal,
                at,
                ..
            })) => {
                assert_eq!(signal, output);
                assert_eq!(at, Anchor { x: 4, y: 0, z: 7 });
            }
            Err(other) => panic!("expected the supplied pin's refusal, got {other:?}"),
            Ok(_) => panic!("an unsupported supplied pin compiled"),
        }

        let mut undeclared = PortPlacements::default();
        undeclared.pin("not_declared", Anchor { x: 4, y: 1, z: 7 }, Facing::North);
        assert!(matches!(
            compile_with_workers(&netlist, Some(&undeclared), &search, 1),
            Err(RecursiveError::Allocation(
                AllocationError::InvalidRootPort {
                    refusal: crate::compile::planner::PinRefusal::UndeclaredPort,
                    ..
                }
            ))
        ));
    }

    /// A partial pin set above the direct-leaf grain takes the allocating
    /// branch on its completed pins, and that branch says the same thing at
    /// one worker and at many.
    #[test]
    fn a_partially_pinned_allocating_root_is_priced_the_same_at_one_worker_and_many() {
        let netlist = chain(4);
        let input = netlist.inputs[0].clone();
        let mut partial = PortPlacements::default();
        partial.pin(input.clone(), Anchor { x: 1, y: 1, z: 7 }, Facing::South);
        let search = SearchConfig::checked_defaults();
        let compile = |workers| {
            compile_with_cutoff(&netlist, Some(&partial), &search, workers, 1)
                .expect("the partially pinned allocating branch builds")
        };

        let serial = compile(1);
        let parallel = compile(many_workers());

        assert!(serial.depth > 1, "depth {}", serial.depth);
        assert_eq!(serial.peak_workers, 1);
        assert!(parallel.peak_workers > 1, "peak {}", parallel.peak_workers);
        for product in [&serial, &parallel] {
            assert_eq!(product.input_positions.get(&input), Some(&(1, 1, 7)));
            assert_eq!(product.world.get(1, 1, 7).kind, BlockKind::Air);
            let output = product.output_positions[&netlist.outputs[0]];
            assert_eq!(output.1, 1, "the placed output left the port plane");
            assert_eq!(output.2, 7, "the placed output left the caller row");
            assert_eq!(
                product.world.get(output.0, output.1, output.2).kind,
                compile::lamp().kind
            );
        }
        assert_eq!(serial.candidate_fingerprint, parallel.candidate_fingerprint);
        assert_eq!(serial.metrics, parallel.metrics);
        assert_eq!(serial.world.cells(), parallel.world.cells());
        assert_eq!(serial.input_positions, parallel.input_positions);
        assert_eq!(serial.output_positions, parallel.output_positions);
        assert_eq!(serial.gate_output_positions, parallel.gate_output_positions);
    }

    /// An input this root placed for the caller ships a lever with a floor
    /// under it, in both root shapes, through the one installer.
    #[test]
    fn a_root_placed_input_lever_stands_on_a_floor_in_both_root_shapes() {
        let search = SearchConfig::checked_defaults();
        // Pin only the output, so the input is the root's to place: one pitch
        // east of the pinned cell, on its row.
        let shapes: [(Netlist, usize); 2] = [(chain(2), TERMINAL_GATES), (chain(4), 1)];
        for (netlist, cutoff) in &shapes {
            let input = netlist.inputs[0].clone();
            let mut partial = PortPlacements::default();
            partial.pin(
                netlist.outputs[0].clone(),
                Anchor { x: 4, y: 1, z: 7 },
                Facing::North,
            );
            let product = compile_with_cutoff(netlist, Some(&partial), &search, 1, *cutoff)
                .expect("a partial set with an unpinned input compiles");
            let expected_depth = if *cutoff == 1 { 2 } else { 1 };
            assert!(
                product.depth >= expected_depth,
                "cutoff {cutoff}: depth {}",
                product.depth
            );
            let at = product.input_positions[&input];
            assert_eq!(at, (7, 1, 7), "cutoff {cutoff}: input placed elsewhere");
            assert_eq!(
                product.world.get(at.0, at.1, at.2).kind,
                BlockKind::Lever,
                "cutoff {cutoff}: no lever"
            );
            assert_eq!(
                product.world.get(at.0, at.1 - 1, at.2).kind,
                BlockKind::Solid,
                "cutoff {cutoff}: the lever has no floor"
            );
            // The caller's own cell is still the caller's.
            assert_eq!(product.world.get(4, 1, 7).kind, BlockKind::Air);
        }
    }

    /// The installer takes an empty cell or the planner's own lever, floors
    /// only an empty floor, and refuses everything else by type without
    /// touching the world.
    #[test]
    fn the_root_lever_installer_never_overwrites_what_stands_there() {
        let fresh = || World::new(6, 4, 6);

        // Empty cell, empty floor: lever on stone.
        let mut world = fresh();
        install_root_lever(&mut world, "a", (2, 1, 2)).unwrap();
        assert_eq!(world.get(2, 1, 2).kind, BlockKind::Lever);
        assert_eq!(world.get(2, 0, 2).kind, BlockKind::Solid);

        // Empty cell, occupied floor: the floor is kept.
        let mut world = fresh();
        world.set(2, 0, 2, compile::dust());
        install_root_lever(&mut world, "a", (2, 1, 2)).unwrap();
        assert_eq!(world.get(2, 1, 2).kind, BlockKind::Lever);
        assert_eq!(world.get(2, 0, 2).kind, BlockKind::RedstoneWire);

        // The planner's lever: kept as placed, floor untouched.
        let mut world = fresh();
        world.set(2, 1, 2, compile::lever(true));
        install_root_lever(&mut world, "a", (2, 1, 2)).unwrap();
        assert!(world.get(2, 1, 2).lit, "the existing lever was replaced");
        assert_eq!(world.get(2, 0, 2).kind, BlockKind::Air);

        // Anything else in the cell: refused, and the world is as it was.
        let mut world = fresh();
        world.set(2, 1, 2, compile::stone());
        let before = world.cells().to_vec();
        assert!(matches!(
            install_root_lever(&mut world, "a", (2, 1, 2)),
            Err(RecursiveError::LeverCollision {
                signal,
                at: Anchor { x: 2, y: 1, z: 2 },
                kind: BlockKind::Solid,
            }) if signal == "a"
        ));
        assert_eq!(world.cells(), &before[..]);

        // Outside the world, or with no floor cell under it: refused.
        let mut world = fresh();
        assert!(matches!(
            install_root_lever(&mut world, "a", (6, 1, 2)),
            Err(RecursiveError::LeverOutOfWorld {
                at: Anchor { x: 6, y: 1, z: 2 },
                ..
            })
        ));
        assert!(matches!(
            install_root_lever(&mut world, "a", (2, 0, 2)),
            Err(RecursiveError::LeverOutOfWorld {
                at: Anchor { x: 2, y: -1, z: 2 },
                ..
            })
        ));
        assert_eq!(
            world.cells(),
            fresh().cells(),
            "a refusal touched the world"
        );
    }

    /// A pinned root whose flat planner refuses as a leaf may still be solved
    /// by the existing recursive split path; typed leaf refusal is the only
    /// root error that continues into that solver.
    #[test]
    #[ignore = "pinned seven-segment recursive acceptance is a large measurement"]
    fn a_pinned_root_leaf_refusal_continues_into_recursive_split() {
        let evaluator = baked_benchmark_evaluator().unwrap();
        let fixture = evaluator.fixture("pinned:verilog:seven_segment").unwrap();
        let search = SearchConfig::checked_defaults();
        assert!(matches!(
            compile_root_leaf(
                fixture.lowered_netlist(),
                &RootPins::normalise(fixture.lowered_netlist(), Some(fixture.placements()))
                    .unwrap(),
                &search,
                1,
            ),
            Err(RecursiveError::Schedule(ScheduleError::Leaf { .. }))
        ));
        let product = compile_with_workers(
            fixture.lowered_netlist(),
            Some(fixture.placements()),
            &search,
            1,
        )
        .unwrap();
        assert!(product.depth > 1, "depth {}", product.depth);
    }

    #[test]
    fn and4_compiles_through_recursive_contracts() {
        let (netlist, _) = build_and4_netlist();
        let product = compile(&netlist, None, &SearchConfig::checked_defaults()).unwrap();
        assert_eq!(product.gate_output_positions.len(), netlist.gates.len());
        assert_eq!(product.gate_facings.len(), netlist.gates.len());
    }

    /// A pinned case is compiled on the caller's own row, not relocated to one
    /// the allocator preferred.
    ///
    /// The recursive path used to accept `pins`, ignore them for layout, and
    /// report whatever cells it chose as the circuit's ports. That is not a
    /// compile failure a caller can see -- it is a different circuit wearing
    /// the caller's signal names.
    #[test]
    fn a_pinned_caller_row_is_honoured_cell_for_cell() {
        // Above the preferred leaf grain, so the pinned row crosses a real
        // contract boundary.
        let netlist = chain(65);
        let pins = caller_row(&netlist, 7);
        let product =
            compile_with_workers(&netlist, Some(&pins), &SearchConfig::checked_defaults(), 1)
                .unwrap();

        for (signal, pin) in pins.iter() {
            let at = product
                .input_positions
                .get(signal)
                .or_else(|| product.output_positions.get(signal))
                .unwrap_or_else(|| panic!("{signal} is a declared port"));
            assert_eq!(
                *at,
                (pin.at.x, pin.at.y, pin.at.z),
                "{signal} was relocated"
            );
        }
        // The caller's cells stay the caller's: nothing is installed in them.
        for (_, pin) in pins.iter() {
            assert_eq!(
                product.world.get(pin.at.x, pin.at.y, pin.at.z).kind,
                BlockKind::Air
            );
        }
        assert!(product.depth > 1, "depth {}", product.depth);
    }

    /// Geometry this contract cannot build is refused by type, with the cell
    /// and the reason, and never quietly moved.
    #[test]
    fn caller_geometry_this_contract_cannot_build_is_refused_by_type() {
        use crate::compile::fragment_synth::allocation::{RootAccess, RootPlacement};

        let netlist = chain(2);
        let good = caller_row(&netlist, 4);
        assert!(honours_pins(&netlist, Some(&good)));
        assert!(honours_pins(&netlist, None));

        // Two rows is not one caller row, but it is buildable: the pins are
        // honoured literally and the body lands behind them.
        let mut split_row = good.clone();
        let output = netlist.outputs[0].clone();
        split_row.pin(output.clone(), Anchor { x: 4, y: 1, z: 9 }, Facing::North);
        assert!(honours_pins(&netlist, Some(&split_row)));
        assert!(matches!(
            root_placement(&netlist, Some(&split_row)),
            Ok(RootPlacement {
                access: RootAccess::Landed { .. },
                ..
            })
        ));

        let mut wrong_plane = good.clone();
        wrong_plane.pin(output.clone(), Anchor { x: 4, y: 0, z: 4 }, Facing::North);
        assert!(!honours_pins(&netlist, Some(&wrong_plane)));
        assert!(matches!(
            compile_with_workers(
                &netlist,
                Some(&wrong_plane),
                &SearchConfig::checked_defaults(),
                1
            ),
            Err(RecursiveError::Allocation(
                AllocationError::UnsupportedRootPin { .. }
            ))
        ));

        // A south-facing output hands over in front of itself, which no caller
        // row admits -- but landed it is just a pin with its approach to the
        // north, which this contract can reach.
        let mut facing_away = good.clone();
        facing_away.pin(output.clone(), Anchor { x: 4, y: 1, z: 4 }, Facing::South);
        assert!(honours_pins(&netlist, Some(&facing_away)));

        let mut parent_column = good.clone();
        parent_column.pin(output, Anchor { x: 0, y: 1, z: 4 }, Facing::North);
        assert!(!honours_pins(&netlist, Some(&parent_column)));

        let mut extra = good.clone();
        extra.pin("not_declared", Anchor { x: 7, y: 1, z: 4 }, Facing::North);
        assert!(!honours_pins(&netlist, Some(&extra)));
        assert!(matches!(
            compile_with_workers(&netlist, Some(&extra), &SearchConfig::checked_defaults(), 1),
            Err(RecursiveError::Allocation(
                AllocationError::InvalidRootPort {
                    refusal: crate::compile::planner::PinRefusal::UndeclaredPort,
                    ..
                }
            ))
        ));
    }

    /// A child that compiles as partitioned is sized exactly, not estimated.
    ///
    /// A parent sizes a child by asking what that child will build, so the
    /// room it allocates has to hold the plan the child then runs. (A child
    /// that has to repair lays out wider than this and renegotiates; that is
    /// `an_extent_refusal_reaches_the_parent_as_itself`.) Sizing a
    /// recursive child as a flat seed is what this guards: measured on this
    /// same chain, `seed_extent` says 34 columns and the child's own plan
    /// fills 294, so every nonterminal escaped its region and bounded repair
    /// halved it away until nothing recursed at all and the tree was two
    /// levels of leaves wearing a parent's name.
    #[test]
    fn a_recursive_child_is_allocated_room_for_the_plan_it_will_run() {
        // One hundred twenty-nine gates still leave a recursive child after
        // the root split, so at least one child remains a contract parent.
        let netlist = chain(129);
        let search = SearchConfig::checked_defaults();
        let overrides = BTreeMap::new();
        let sizer = ContractExtent {
            search: &search,
            overrides: &overrides,
        };
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, split_of(&netlist)).unwrap();
        let plan = allocate_with(
            &netlist,
            &chunks,
            allocation_limits(&netlist, &chunks, &search),
            None,
            &sizer,
        )
        .unwrap();

        let mut checked = 0;
        for (chunk, allocation) in chunks.iter().zip(&plan.children) {
            if chunk.netlist.gates.len() <= TERMINAL_GATES {
                continue;
            }
            let ports = allocation.port_placements();
            let children = partition(&chunk.netlist, &chunk.id, split_of(&chunk.netlist)).unwrap();
            let nested = allocate_with(
                &chunk.netlist,
                &children,
                allocation_limits(&chunk.netlist, &children, &search),
                Some(&ports),
                &sizer,
            )
            .unwrap();
            let region = allocation.local_region();
            assert!(
                region.contains(nested.local_extent()),
                "region {region:?} cannot hold the plan its child runs, which reaches {:?}",
                nested.local_extent()
            );
            checked += 1;
        }
        assert!(checked > 0, "no child of this chain was a contract parent");
    }

    /// A terminal child's region holds the world its leaf realises, and is
    /// sized by that world rather than by the seed's sparse starting layout.
    ///
    /// The circuit that exposed this was `segment_a`: sized as a seed, each
    /// half was allocated several times the floor area its candidate settles
    /// into, so its sibling sat beyond empty space and every parent trunk
    /// crossed it -- 21692 blocks and 296 game ticks against a 6416-block,
    /// 72-tick baseline. A forty-gate chain shows the same gap in well under
    /// a second: the seed stacks twenty rows one pitch apart, the candidate
    /// folds them.
    #[test]
    fn a_terminal_child_is_allocated_the_world_its_leaf_realises() {
        let netlist = chain(40);
        let search = SearchConfig::checked_defaults();
        let overrides = BTreeMap::new();
        let sizer = ContractExtent {
            search: &search,
            overrides: &overrides,
        };
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, split_of(&netlist)).unwrap();
        let plan = allocate_with(
            &netlist,
            &chunks,
            allocation_limits(&netlist, &chunks, &search),
            None,
            &sizer,
        )
        .unwrap();

        let mut checked = 0;
        for chunk in &chunks {
            assert!(
                chunk.netlist.gates.len() <= TERMINAL_GATES,
                "both halves of a forty-gate chain are leaves"
            );
            let allocation = plan
                .children
                .iter()
                .find(|allocation| allocation.chunk == chunk.id)
                .expect("every chunk is allocated");
            // The leaf's own placements: the same pins and the same lower
            // bound the sizer was handed, or the two plans would differ.
            let ports = allocation.port_placements();
            let candidate = planner::plan_from_netlist(&chunk.netlist, &ports).unwrap();
            let (x, y, z) = planner::candidate_world_size(&candidate);
            let region = allocation.local_region();
            let far = Anchor {
                x: x - 1,
                y: y - 1,
                z: z - 1,
            };
            assert!(
                region.contains(far),
                "region {region:?} cannot hold the {x}x{y}x{z} world its leaf realises"
            );

            let seed = SeedExtent.extent(chunk, &ports).unwrap();
            let sized = sizer.extent(chunk, &ports).unwrap();
            assert_eq!(sized, far, "the sizer measures the leaf's own candidate");
            assert!(
                sized.x * sized.z * 2 <= seed.x * seed.z,
                "candidate footprint {sized:?} is not materially smaller than seed extent {seed:?}"
            );
            checked += 1;
        }
        assert_eq!(checked, 2);
    }

    /// A planner refusal while sizing is the child's refusal, by chunk id, not
    /// a failure of the whole allocation.
    #[test]
    fn a_planner_refusal_while_sizing_names_the_child() {
        let netlist = chain(2);
        let search = SearchConfig::checked_defaults();
        let overrides = BTreeMap::new();
        let sizer = ContractExtent {
            search: &search,
            overrides: &overrides,
        };
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, 1).unwrap();
        let mut ports = PortPlacements::default();
        ports.pin("undeclared", Anchor { x: 2, y: 1, z: 0 }, Facing::South);
        match sizer.extent(&chunks[0], &ports) {
            Err(AllocationError::ChildUnplannable { chunk, .. }) => assert_eq!(chunk, chunks[0].id),
            other => panic!("a sizing refusal must name the child: {other:?}"),
        }
    }

    /// The repair for a refused child halves exactly that child, leaves its
    /// siblings' gates alone, and stops by type at one gate.
    #[test]
    fn a_refusal_splits_exactly_the_refused_child_down_to_one_gate() {
        let netlist = chain(8);
        let root = root_chunk_id(&netlist).unwrap();
        let mut chunks = partition(&netlist, &root, 2).unwrap();
        assert_eq!(chunks.len(), 4);
        let refused = chunks[1].clone();
        let siblings = chunks
            .iter()
            .filter(|chunk| chunk.id != refused.id)
            .cloned()
            .collect::<Vec<_>>();

        split_refused_child(&mut chunks, 1, refused.id.clone(), "refused".into()).unwrap();
        assert_eq!(chunks.len(), 5);
        assert!(chunks.iter().all(|chunk| chunk.id != refused.id));
        for sibling in &siblings {
            assert!(
                chunks.contains(sibling),
                "sibling {:?} was disturbed",
                sibling.id
            );
        }
        let halves = chunks
            .iter()
            .filter(|chunk| !siblings.contains(chunk))
            .collect::<Vec<_>>();
        assert_eq!(halves.len(), 2);
        let mut regrouped = halves
            .iter()
            .flat_map(|half| half.netlist.gates.iter().map(|gate| gate.output.clone()))
            .collect::<Vec<_>>();
        regrouped.sort();
        let mut original = refused
            .netlist
            .gates
            .iter()
            .map(|gate| gate.output.clone())
            .collect::<Vec<_>>();
        original.sort();
        assert_eq!(regrouped, original);

        let single = halves[0].id.clone();
        match split_refused_child(&mut chunks, 3, single.clone(), "still refused".into()) {
            Err(RecursiveError::ChildRefused {
                index,
                chunk,
                error,
            }) => {
                assert_eq!(index, 3);
                assert_eq!(chunk, single);
                assert_eq!(error, "still refused");
            }
            other => panic!("a one-gate refusal must end the repair by type: {other:?}"),
        }
    }

    /// Column `x = 0` of every frame belongs to that frame's parent.
    ///
    /// `compose` may lay trunk hardware anywhere in a child's halo, and a
    /// nested node's region starts at `x = 1`. A halo or corridor touching
    /// column 0 is therefore a node licensed to place blocks outside the
    /// region it was allocated -- the leaf router's own escape rule, one level
    /// up, and invisible until a trunk happens to want that column.
    #[test]
    fn no_frame_admits_the_column_its_parent_owns() {
        let netlist = chain(8);
        let search = SearchConfig::checked_defaults();
        let overrides = BTreeMap::new();
        let sizer = ContractExtent {
            search: &search,
            overrides: &overrides,
        };
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, split_of(&netlist)).unwrap();
        let plan = allocate_with(
            &netlist,
            &chunks,
            allocation_limits(&netlist, &chunks, &search),
            None,
            &sizer,
        )
        .unwrap();

        assert!(
            plan.corridor.region.min.x >= 1,
            "corridor {:?} admits column 0",
            plan.corridor.region
        );
        for child in &plan.children {
            assert!(
                child.halo.min.x >= 1,
                "halo {:?} admits column 0",
                child.halo
            );
        }
    }

    /// The recursion has to be real, not a second level that re-wraps the
    /// whole netlist and then calls the leaf router.
    ///
    /// A sixty-five-gate chain crosses the preferred leaf boundary, so the
    /// root must partition, allocate, recurse and compose instead of returning
    /// a direct leaf. The assertion is the boundary rather than the exact
    /// number, because split arity and grain are tuning constants.
    #[test]
    fn a_chain_recurses_past_depth_two() {
        let netlist = chain(65);
        let product =
            compile_with_workers(&netlist, None, &SearchConfig::checked_defaults(), 1).unwrap();
        assert!(
            product.depth > 1,
            "a sixty-five-gate chain must recurse below the root, got depth {}",
            product.depth
        );
        assert_eq!(product.gate_output_positions.len(), netlist.gates.len());
        assert_eq!(product.gate_facings.len(), netlist.gates.len());
    }

    /// One worker and N workers must produce the same circuit -- on a netlist
    /// where N workers genuinely run.
    ///
    /// A level with one sibling collapses to one worker whatever the cap says.
    /// Forty-nine gates is the smallest chain above both the 32-gate leaf
    /// threshold and the 48-gate wide leaf, so whichever candidate ships, the
    /// root has two children to hand out and the parallel arm cannot silently
    /// compare another serial run.
    #[test]
    fn one_worker_and_many_compose_the_same_recursive_circuit() {
        let netlist = chain(49);
        let search = SearchConfig::checked_defaults();
        let serial = compile_with_workers(&netlist, None, &search, 1).unwrap();
        let parallel = compile_with_workers(&netlist, None, &search, many_workers()).unwrap();

        assert_eq!(serial.peak_workers, 1, "the serial arm must be serial");
        assert!(
            parallel.peak_workers > 1,
            "nothing ran in parallel, so this compared a serial run with itself: peak {}",
            parallel.peak_workers
        );
        assert!(serial.depth > 1, "depth {}", serial.depth);
        assert_eq!(serial.depth, parallel.depth);
        assert_eq!(serial.candidate_fingerprint, parallel.candidate_fingerprint);
        assert_eq!(
            canonical_world_fingerprint(&serial.world),
            canonical_world_fingerprint(&parallel.world)
        );
        assert_eq!(serial.world.size(), parallel.world.size());
        assert_eq!(serial.world.cells(), parallel.world.cells());
        assert_eq!(serial.gate_output_positions, parallel.gate_output_positions);
        assert_eq!(serial.gate_facings, parallel.gate_facings);
        assert_eq!(serial.input_positions, parallel.input_positions);
        assert_eq!(serial.output_positions, parallel.output_positions);
        assert_eq!(
            serial.metrics.realised_timing_graph_fingerprint,
            parallel.metrics.realised_timing_graph_fingerprint
        );
        assert_eq!(
            serial.metrics.transition_manifest_hash,
            parallel.metrics.transition_manifest_hash
        );
        // The metrics are read off the root certificate, which the parallel
        // arm certified on every worker it was given and the serial arm on
        // one, so this is also the end-to-end check on parallel certification.
        assert_eq!(serial.metrics, parallel.metrics);
        assert_eq!(serial.gate_output_positions.len(), netlist.gates.len());
    }

    /// The same gates declared in a different order, which is the same
    /// netlist: reversed, then two swapped so the result is neither the
    /// original nor its mirror.
    fn redeclared(netlist: &Netlist) -> Netlist {
        let mut gates = netlist.gates.clone();
        gates.reverse();
        if gates.len() > 2 {
            gates.swap(0, 2);
        }
        assert_ne!(gates, netlist.gates, "the reordering has to be a real one");
        Netlist {
            inputs: netlist.inputs.clone(),
            outputs: netlist.outputs.clone(),
            gates,
        }
    }

    /// Gate facings keyed by the gate they belong to, so two netlists that
    /// declare the same gates in different orders can be compared.
    fn facings_by_gate(
        netlist: &Netlist,
        product: &RecursiveProduct,
    ) -> BTreeMap<String, CellFacing> {
        assert_eq!(product.gate_facings.len(), netlist.gates.len());
        netlist
            .gates
            .iter()
            .map(|gate| gate.output.clone())
            .zip(product.gate_facings.iter().copied())
            .collect()
    }

    /// The order gates are declared in is not part of the circuit, so it must
    /// not be part of anything the recursive path produces.
    ///
    /// Partition and allocation each hold this for themselves; this holds it
    /// end to end through `compile`, where a leaf laid out from a chunk that
    /// kept declaration order, or a parent that filed outcomes by it, would
    /// move blocks that every earlier check reports as identical. Every
    /// artifact and fingerprint the product carries is compared, not only the
    /// candidate fingerprint that summarises them.
    #[test]
    fn redeclaring_gates_in_another_order_compiles_the_identical_circuit() {
        let (declared, _) = build_and4_netlist();
        let redeclared = redeclared(&declared);
        let search = SearchConfig::checked_defaults();
        let first = compile(&declared, None, &search).unwrap();
        let second = compile(&redeclared, None, &search).unwrap();

        assert_eq!(first.depth, second.depth);
        assert_eq!(first.candidate_fingerprint, second.candidate_fingerprint);
        assert_eq!(first.metrics, second.metrics);
        assert_eq!(
            first.metrics.emitted_world_fingerprint,
            second.metrics.emitted_world_fingerprint
        );
        assert_eq!(
            first.metrics.realised_timing_graph_fingerprint,
            second.metrics.realised_timing_graph_fingerprint
        );
        assert_eq!(
            first.metrics.transition_manifest_hash,
            second.metrics.transition_manifest_hash
        );
        assert_eq!(
            canonical_world_fingerprint(&first.world),
            canonical_world_fingerprint(&second.world)
        );
        assert_eq!(first.world.size(), second.world.size());
        assert_eq!(first.world.cells(), second.world.cells());
        assert_eq!(first.input_positions, second.input_positions);
        assert_eq!(first.output_positions, second.output_positions);
        assert_eq!(first.gate_output_positions, second.gate_output_positions);
        assert_eq!(
            facings_by_gate(&declared, &first),
            facings_by_gate(&redeclared, &second)
        );
    }

    /// A child that outgrows its region is renegotiated, not halved away.
    ///
    /// `solve_subtree` refuses by type the moment its own plan stops fitting
    /// what its parent reserved, before compiling anything, and the parent
    /// widens that one chunk and reallocates. The refusal has to carry a need
    /// strictly larger than the room, or the parent would record an override
    /// that changes nothing and spend its whole negotiation budget.
    #[test]
    fn a_child_that_outgrows_its_region_refuses_by_type_before_it_compiles() {
        let netlist = chain(4);
        let search = SearchConfig::checked_defaults();
        let gauge = Gauge::default();
        let session = session_for(&search, &gauge);
        let root = root_chunk_id(&netlist).unwrap();

        let honest = solve_subtree(&netlist, &root, None, split_of(&netlist), None, session)
            .expect("the chain solves with no budget at all");
        let needed = honest.0.local_extent();

        let squeezed = Anchor {
            x: needed.x - 1,
            ..needed
        };
        let refusal = solve_subtree(
            &netlist,
            &root,
            None,
            split_of(&netlist),
            Some(squeezed),
            session,
        );
        match refusal {
            Err(RecursiveError::ExtentRefused {
                needed: reported,
                available,
                ..
            }) => {
                assert_eq!(available, squeezed);
                assert!(
                    !fits(reported, available),
                    "a refusal that already fits would make the parent reallocate for nothing"
                );
            }
            Err(other) => panic!("expected a typed extent refusal, got {other}"),
            Ok(_) => panic!("a plan one column too wide for its budget must be refused"),
        }
    }

    /// An extent refusal has to reach the parent as itself.
    ///
    /// Every other failure a child reports is folded into `ChildRefused`,
    /// which the parent answers by halving that child away. An extent refusal
    /// answered that way would destroy a subtree that compiled perfectly well
    /// and only needed a wider region, so it has to survive the worker
    /// boundary intact -- and nothing else in the module would notice if it
    /// stopped doing so.
    #[test]
    fn an_extent_refusal_reaches_the_parent_as_itself() {
        /// Reports every nonterminal one column narrower than it needs.
        struct Pinched<'a>(ContractExtent<'a>);

        impl ChildExtent for Pinched<'_> {
            fn extent(
                &self,
                chunk: &Chunk,
                ports: &PortPlacements,
            ) -> Result<Anchor, AllocationError> {
                let honest = self.0.extent(chunk, ports)?;
                Ok(if chunk.netlist.gates.len() <= TERMINAL_GATES {
                    honest
                } else {
                    Anchor {
                        x: honest.x - 1,
                        ..honest
                    }
                })
            }
        }

        // `Pinched` only narrows nonterminals, so the chain has to be large
        // enough that a child is one.
        let netlist = chain(65);
        let search = SearchConfig::checked_defaults();
        let gauge = Gauge::default();
        let overrides = BTreeMap::new();
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, split_of(&netlist)).unwrap();
        let plan = allocate_with(
            &netlist,
            &chunks,
            allocation_limits(&netlist, &chunks, &search),
            None,
            &Pinched(ContractExtent {
                search: &search,
                overrides: &overrides,
            }),
        )
        .unwrap();

        match synthesise_recursive_children(&chunks, &plan, session_for(&search, &gauge)) {
            Err(RecursiveError::ExtentRefused {
                chunk,
                needed,
                available,
            }) => {
                assert!(chunks.iter().any(|candidate| candidate.id == chunk));
                assert!(!fits(needed, available));
            }
            Err(other) => panic!("an undersized child must not read as a plain refusal: {other}"),
            Ok(_) => panic!("a child given one column too few must refuse"),
        }
    }

    #[test]
    fn a_nested_compose_escape_is_a_direct_child_refusal() {
        let netlist = chain(2);
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, 1).unwrap();
        let direct = chunks[0].id.clone();
        let grandchild = chunks[1].id.clone();
        match classify_child_error(
            2,
            &direct,
            RecursiveError::Compose(ComposeError::Escape {
                chunk: grandchild.clone(),
                at: Anchor { x: 9, y: 1, z: 1 },
            }),
        ) {
            RecursiveError::ChildRefused {
                index,
                chunk,
                error,
            } => {
                assert_eq!(index, 2);
                assert_eq!(chunk, direct);
                assert!(error.contains("outside"));
            }
            other => panic!("a nested compose escape must remain a child refusal: {other:?}"),
        }

        match classify_child_error(
            2,
            &direct,
            RecursiveError::ExtentRefused {
                chunk: grandchild,
                needed: Anchor { x: 9, y: 1, z: 1 },
                available: Anchor { x: 4, y: 1, z: 1 },
            },
        ) {
            RecursiveError::ChildRefused { chunk, .. } => assert_eq!(chunk, direct),
            other => panic!("a descendant extent refusal leaked through: {other:?}"),
        }
    }

    #[test]
    fn low_side_region_escape_is_not_a_noop_extent_negotiation() {
        let netlist = chain(2);
        let root = root_chunk_id(&netlist).unwrap();
        let chunks = partition(&netlist, &root, 1).unwrap();
        let available = Anchor { x: 4, y: 8, z: 12 };
        let low = Anchor { x: 0, y: 1, z: 3 };
        match region_escape(chunks[0].id.clone(), available, low) {
            RecursiveError::Compose(ComposeError::Escape { chunk, at }) => {
                assert_eq!(chunk, chunks[0].id);
                assert_eq!(at, low);
            }
            other => panic!("low-side escape was misclassified: {other:?}"),
        }
    }

    /// What the recursive path costs on the circuit the acceptance corpus
    /// measures, next to what the seed path costs for the same netlist.
    ///
    /// Reported rather than asserted: the corpus owns the shipping thresholds,
    /// and a second set of numbers here would either duplicate them or quietly
    /// contradict them. This uses the built-in `and4` netlist, so it runs
    /// without the Verilog frontend.
    /// What the recursive path spends on building `segment_a` against what it
    /// spends proving the result, as two numbers rather than one.
    ///
    /// Compilation and certification have different costs and different fixes:
    /// a slow compile is routing or partitioning, a slow certification is the
    /// transition manifest and the simulator. One total hides which is which.
    #[test]
    #[ignore = "a measurement, not a gate: run it explicitly"]
    fn measure_segment_a_compile_against_certification() {
        use std::time::Instant;

        let (netlist, _) = crate::circuits::seven_segment::build_single_segment_netlist(0);
        let search = SearchConfig::checked_defaults();
        let gauge = Gauge::default();
        let session = session_for(&search, &gauge);
        let root = root_chunk_id(&netlist).unwrap();

        let started = Instant::now();
        let (plan, _, outcomes) =
            solve_subtree(&netlist, &root, None, split_of(&netlist), None, session)
                .expect("segment_a solves");
        let leaves = outcomes
            .into_iter()
            .map(|outcome| outcome.leaf)
            .collect::<Vec<_>>();
        let composed = compose(&plan, &leaves, &DurablePhysicalRouter, search.router_limits)
            .expect("segment_a composes");
        let compiled = started.elapsed();

        let started = Instant::now();
        certify_root_world(
            &composed.world,
            &netlist,
            &plan.root_ports,
            &CertificationConfig::from_search(&search),
            CertificationWorkers::serial(),
        )
        .expect("segment_a certifies");
        let certified = started.elapsed();

        println!(
            "segment_a: compile {:.2}s, certification {:.2}s",
            compiled.as_secs_f64(),
            certified.as_secs_f64()
        );
    }

    /// Measures this module's product against what the public API ships.
    ///
    /// It compared against the seed producer until the public path was cut
    /// over to the recursive contract; both sides are now this path, so what
    /// it reads is the cost of the adapter and the harness rather than a
    /// difference between two generators.
    #[test]
    #[ignore = "a measurement, not a gate: run it explicitly"]
    fn measure_and4_against_the_public_path() {
        use crate::compile::fragment_synth::{
            compile_fragment_synth, SynthesisBudget, SynthesisInput,
        };

        let (netlist, _) = build_and4_netlist();
        let recursive = compile(&netlist, None, &SearchConfig::checked_defaults()).unwrap();
        let public = compile_fragment_synth(
            SynthesisInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SynthesisBudget::Evaluations(1),
        )
        .expect("the public path compiles the built-in and4");

        let (rx, ry, rz) = recursive.world.size();
        let (sx, sy, sz) = public.compiled.world.size();
        eprintln!(
            "and4 gates={} depth={} peak_workers={}",
            netlist.gates.len(),
            recursive.depth,
            recursive.peak_workers
        );
        eprintln!(
            "  recursive: {rx}x{ry}x{rz} blocks={} volume={} settle={}gt",
            recursive.metrics.quality.non_air_blocks,
            recursive.metrics.quality.occupied_volume,
            recursive.metrics.quality.observed_settle
        );
        eprintln!(
            "  public:    {sx}x{sy}x{sz} blocks={} volume={} settle={}gt",
            public.metrics.quality.non_air_blocks,
            public.metrics.quality.occupied_volume,
            public.metrics.quality.observed_settle
        );
    }

    #[test]
    #[ignore = "whole seven_segment through partition, leaves, compose and certification: minutes"]
    fn seven_segment_compiles_through_recursive_contracts() {
        let (netlist, _) = crate::circuits::seven_segment::build_seven_segment_netlist();
        let product = compile(&netlist, None, &SearchConfig::checked_defaults()).unwrap();
        assert_eq!(product.gate_output_positions.len(), netlist.gates.len());
        assert!(product.depth > 1, "depth {}", product.depth);
    }

    /// How many leaf workers ran is not allowed to reach the composed world.
    ///
    /// `one_worker_and_many_compose_the_same_recursive_circuit` holds that on
    /// a chain small enough to run in the release gate. This holds it where
    /// the parent's own trunks are laid one at a time against every earlier
    /// one, on the largest circuit this path composes -- because a leaf
    /// artifact that is bit-identical can still be handed to `compose` in a
    /// different order and move every trunk after it.
    ///
    /// Certification is deliberately not run twice: what is being compared is
    /// the composition, and the world fingerprint already covers every block
    /// either one placed.
    #[test]
    #[ignore = "composes seven_segment twice: minutes"]
    fn seven_segment_composition_is_the_same_at_one_worker_and_many() {
        let (netlist, _) = crate::circuits::seven_segment::build_seven_segment_netlist();
        let search = SearchConfig::checked_defaults();
        let gauge = Gauge::default();
        let root = root_chunk_id(&netlist).expect("the root chunk names itself");
        let (plan, chunks, _) = solve_subtree(
            &netlist,
            &root,
            None,
            split_of(&netlist),
            None,
            session_for(&search, &gauge),
        )
        .expect("every recursive contract is satisfiable");

        let compose_with = |workers| {
            let outcomes = synthesise_recursive_children(
                &chunks,
                &plan,
                Session {
                    search: &search,
                    gauge: &gauge,
                    workers,
                },
            )
            .unwrap_or_else(|error| panic!("{workers} workers synthesise the children: {error}"));
            let leaves = outcomes
                .into_iter()
                .map(|outcome| outcome.leaf)
                .collect::<Vec<_>>();
            let composed = compose(&plan, &leaves, &DurablePhysicalRouter, search.router_limits)
                .unwrap_or_else(|error| panic!("{workers} workers compose: {error}"));
            (
                canonical_world_fingerprint(&composed.world),
                canonical_fingerprint(
                    &serde_json::to_vec(&composed.trunks).expect("route trees serialize"),
                ),
            )
        };

        let serial = compose_with(1);
        let parallel = compose_with(many_workers());
        assert_eq!(
            serial, parallel,
            "world and trunk fingerprints must not depend on the worker count"
        );
    }
}
