use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::Serialize;
use thiserror::Error;

use crate::compile::equivalence::{
    prove_combinational_equivalence_with_identity, EquivalenceCertificate, EquivalenceError,
};
use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use crate::compile::fragment_synth::candidate::{
    CandidateError, CompatibilityViews, ExpandedPhysicalCandidate,
};
use crate::compile::fragment_synth::config::CertificationConfig;
use crate::compile::fragment_synth::manifest::{Transition, TransitionManifest};
use crate::compile::fragment_synth::realise::{
    realise_and_verify_expanded_with_identity, CertificationError as PhysicalCertificationError,
    CertifiedWorld,
};
use crate::compile::fragment_synth::timing_graph::{
    ExactDelay, RealisedTimingGraph, TimingGraphError,
};
use crate::compile::fragment_synth::verify::CertificationIdentity;
use crate::compile::metrics::{physical_metrics, Fingerprint};
use crate::compile::topology::Library;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::block_signal_at;
use crate::redstone::simulator::{BoundedSimulationError, SimulationError, Simulator};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

/// Bytes one compile may spend on the worlds its certification workers own.
///
/// Runtime scheduling state, not a fingerprinted config field:
/// `REDA_CERT_MEMORY_BYTES` overrides it for an operator or a test.
const CERT_WORKER_MEMORY_BUDGET_BYTES: usize = 1 << 30;
/// Worlds one certification worker is assumed to hold at once.
///
/// A worker owns the certified world it sweeps plus the simulator copy it
/// drives, and a copy may be in flight while the next one is built, so four
/// dense worlds is the starting estimate. The benchmark task checks peak
/// working set against `workers * per_worker_bytes` and raises this number if
/// the measurement does not fit inside it; it is not measured yet.
const WORLD_COPY_HEADROOM: usize = 4;
/// Minimum canonical exhaustive vectors one certification worker must own.
///
/// Measured on this host with a release build: the real exhaustive sweep at one
/// and two workers over 4 to 256 canonical vectors lost at 4 and 8 vectors and
/// won from 16 upward, repeatably across two independent runs, so 16 vectors
/// over two workers is the first crossover.
const MIN_VECTORS_PER_CERTIFICATION_WORKER: usize = 8;
/// Minimum manifest transitions one certification worker must own.
///
/// A deliberate choice measured on large sweeps, not a measured crossover.
/// Sharing the exhaustive threshold capped ripple_adder8's 68-transition top
/// manifest at 8 of its 12 granted workers; one transition per worker restores
/// the 12 the retained revision used, and the aggregate `cargo test` time shows
/// no regression from the extra workers the smaller sweeps now open.
///
/// ponytail: 68 transitions is the smallest case measured, so every sweep
/// narrower than that is unmeasured. The win at 68 is evidence for wide sweeps
/// only, not proof that one is optimal everywhere. Benchmark narrow manifest
/// sweeps before raising this above one.
const MIN_TRANSITIONS_PER_CERTIFICATION_WORKER: usize = 1;
static CERTIFICATION_SWEEP_LOCK: Mutex<()> = Mutex::new(());

/// One compile's worker budget plus the only thing a sweep needs to know
/// about where it came from.
///
/// `explicit` carries the operator's `REDA_CERT_THREADS` override so a sweep
/// can memory-clamp an automatic budget without second-guessing a deliberate
/// one. `Copy`, so the thread-local below stays a plain `Cell`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScopedWorkerBudget {
    workers: usize,
    explicit: Option<usize>,
}

thread_local! {
    /// One compile's certification worker budget, owned by the thread that
    /// opened the scope. This is runtime scheduling state: it is never
    /// fingerprinted, and a spawned worker starts without it rather than
    /// inheriting a second full budget.
    static CERTIFICATION_THREADS: Cell<Option<ScopedWorkerBudget>> = const { Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct QualityKey {
    pub observed_settle: u64,
    pub non_air_blocks: u64,
    pub occupied_volume: u64,
    pub static_routed_delay: ExactDelay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateMetrics {
    pub quality: QualityKey,
    pub transition_manifest_hash: Fingerprint,
    pub transition_count: u64,
    pub transition_cap: u64,
    pub worst_transition_indices: Vec<usize>,
    pub equivalence_certificate_fingerprint: Option<Fingerprint>,
    pub realised_timing_graph_fingerprint: Fingerprint,
    pub candidate_fingerprint: Fingerprint,
    pub emitted_world_fingerprint: Fingerprint,
}

impl CandidateMetrics {
    pub fn is_strict_improvement_over(&self, incumbent: &Self) -> bool {
        self.quality < incumbent.quality
    }

    pub fn stable_selection_order(&self, other: &Self) -> Ordering {
        self.quality
            .cmp(&other.quality)
            .then_with(|| self.candidate_fingerprint.cmp(&other.candidate_fingerprint))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TransitionMeasurement {
    pub manifest_index: usize,
    pub start_tick: u64,
    pub settle_game_ticks: u64,
    pub simulator_events: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionPhase {
    Source,
    Destination,
    ExhaustiveVector,
}

#[derive(Debug, Error)]
pub enum CandidateCertificationError {
    #[error(transparent)]
    Physical(#[from] PhysicalCertificationError),
    #[error(transparent)]
    Candidate(#[from] CandidateError),
    #[error(transparent)]
    Timing(#[from] TimingGraphError),
    #[error(transparent)]
    Equivalence(#[from] EquivalenceError),
    #[error("transition manifest has {count} entries, exceeding cap {limit}")]
    TransitionCapExceeded { count: u64, limit: u64 },
    #[error("transition {manifest_index} did not settle during {phase:?}: {simulation_error:?}")]
    TransitionDidNotSettle {
        manifest_index: usize,
        phase: TransitionPhase,
        simulation_error: SimulationError,
    },
    #[error("transition {manifest_index} used {used} simulator events, exceeding cap {limit}")]
    SimulatorEventCapExceeded {
        manifest_index: usize,
        used: u64,
        limit: u64,
    },
    #[error("input vector width {actual} does not match {expected} declared inputs")]
    InputWidthMismatch { expected: usize, actual: usize },
    #[error("netlist has a combinational cycle")]
    CombinationalCycle,
    #[error("signal `{signal}` is unresolved while evaluating the lowered netlist")]
    UnresolvedLogicalSignal { signal: String },
    #[error(
        "functional mismatch at transition {manifest_index}, output `{output}`: expected {expected}, got {actual}"
    )]
    FunctionalMismatch {
        manifest_index: usize,
        output: String,
        expected: bool,
        actual: bool,
    },
    #[error("certification counter overflow")]
    CounterOverflow,
}

#[derive(Debug)]
pub struct CertifiedCandidate {
    candidate: ExpandedPhysicalCandidate,
    world: CertifiedWorld,
    equivalence: EquivalenceCertificate,
    timing_graph: RealisedTimingGraph,
    manifest: TransitionManifest,
    measurements: Vec<TransitionMeasurement>,
    metrics: CandidateMetrics,
}

impl CertifiedCandidate {
    pub fn candidate(&self) -> &ExpandedPhysicalCandidate {
        &self.candidate
    }

    pub fn world(&self) -> &World {
        self.world.world()
    }

    pub fn equivalence_certificate(&self) -> &EquivalenceCertificate {
        &self.equivalence
    }

    pub fn timing_graph(&self) -> &RealisedTimingGraph {
        &self.timing_graph
    }

    pub fn manifest(&self) -> &TransitionManifest {
        &self.manifest
    }

    pub fn measurements(&self) -> &[TransitionMeasurement] {
        &self.measurements
    }

    pub fn metrics(&self) -> &CandidateMetrics {
        &self.metrics
    }
}

pub trait ExpandedCandidateCertifier {
    fn certify(
        &self,
        candidate: ExpandedPhysicalCandidate,
        lowered: &Netlist,
        library: &Library,
        config: &CertificationConfig,
    ) -> Result<CertifiedCandidate, CandidateCertificationError>;
}

pub struct CompleteCandidateCertifier;

impl ExpandedCandidateCertifier for CompleteCandidateCertifier {
    fn certify(
        &self,
        candidate: ExpandedPhysicalCandidate,
        lowered: &Netlist,
        library: &Library,
        config: &CertificationConfig,
    ) -> Result<CertifiedCandidate, CandidateCertificationError> {
        let identity = CertificationIdentity::seal(&candidate, library);
        self.certify_with_identity(candidate, lowered, library, config, &identity)
    }
}

impl CompleteCandidateCertifier {
    /// Certify under one already-sealed identity.
    ///
    /// The candidate and the library revision are fingerprinted once, by
    /// whoever built `identity`; structural certification, timing derivation,
    /// the equivalence proof and the metrics all borrow that seal instead of
    /// serializing the candidate again per certificate.
    ///
    /// # Precondition
    ///
    /// `identity` must have been sealed from the very `candidate` and `library`
    /// passed to this call, as `CertificationIdentity::seal(&candidate,
    /// library)` does. Every certificate returned here stamps the seal's
    /// fingerprints rather than recomputing them, so an identity sealed from
    /// some other candidate would label all of them with that other candidate
    /// and no later check could notice. That is why this stays private: the
    /// only callers are `certify` above, which seals immediately before
    /// calling, and this module's own tests.
    fn certify_with_identity(
        &self,
        candidate: ExpandedPhysicalCandidate,
        lowered: &Netlist,
        library: &Library,
        config: &CertificationConfig,
        identity: &CertificationIdentity,
    ) -> Result<CertifiedCandidate, CandidateCertificationError> {
        let timing = std::env::var_os("REDA_PHASE_TIMING").is_some();
        let mut phase_started = std::time::Instant::now();
        let phase = |name: &str, started: &mut std::time::Instant| {
            if timing {
                eprintln!("PHASE {name} {}", started.elapsed().as_millis());
            }
            *started = std::time::Instant::now();
        };
        let world =
            realise_and_verify_expanded_with_identity(&candidate, lowered, library, identity)?;
        phase("structure+emit+verify", &mut phase_started);
        let timing_graph = RealisedTimingGraph::derive_with_identity(
            &candidate,
            world.structural_certificate(),
            identity,
        )?;
        let static_timing = timing_graph.analyse()?;
        phase("timing", &mut phase_started);
        let equivalence = prove_combinational_equivalence_with_identity(
            lowered,
            &candidate,
            library,
            config.max_equivalence_proof_steps,
            identity,
        )?;
        phase("equivalence", &mut phase_started);
        let compatibility = candidate.compatibility_views(lowered)?;
        let manifest =
            TransitionManifest::for_kind(lowered.inputs.clone(), config.transition_manifest_kind);
        let transition_count = u64::try_from(manifest.transitions().len())
            .map_err(|_| CandidateCertificationError::CounterOverflow)?;
        if transition_count > config.max_certification_transitions {
            return Err(CandidateCertificationError::TransitionCapExceeded {
                count: transition_count,
                limit: config.max_certification_transitions,
            });
        }

        // Manifest construction belongs to neither simulation phase, so it
        // gets its own label rather than inflating one of them.
        phase("compatibility+manifest_build", &mut phase_started);
        let exhaustive = lowered.inputs.len() <= usize::from(config.exhaustive_input_threshold);
        let exhaustive_vectors = if exhaustive {
            certify_exhaustive_truth(world.world(), &candidate, lowered, &compatibility, config)?
        } else {
            0
        };
        phase("exhaustive", &mut phase_started);
        if timing {
            eprintln!("WORK exhaustive_vectors {exhaustive_vectors}");
        }

        let measurements = sweep_manifest(
            world.world(),
            &candidate,
            lowered,
            &compatibility,
            &manifest,
            config,
        )?;
        phase("manifest", &mut phase_started);
        if timing {
            eprintln!("WORK manifest_transitions {transition_count}");
        }
        let worst = measurements
            .iter()
            .map(|measurement| measurement.settle_game_ticks)
            .max()
            .unwrap_or(0);
        let worst_transition_indices = measurements
            .iter()
            .filter(|measurement| measurement.settle_game_ticks == worst)
            .map(|measurement| measurement.manifest_index)
            .collect();
        let physical = physical_metrics(world.world(), lowered.gates.len() as u64);
        let metrics = CandidateMetrics {
            quality: QualityKey {
                observed_settle: worst,
                non_air_blocks: physical.non_air_blocks,
                occupied_volume: physical.occupied_volume,
                static_routed_delay: static_timing.critical_delay,
            },
            transition_manifest_hash: manifest.fingerprint(),
            transition_count,
            transition_cap: config.max_certification_transitions,
            worst_transition_indices,
            equivalence_certificate_fingerprint: (lowered.inputs.len()
                > usize::from(config.exhaustive_input_threshold))
            .then(|| equivalence.fingerprint.clone()),
            realised_timing_graph_fingerprint: timing_graph.fingerprint(),
            candidate_fingerprint: identity.candidate.clone(),
            emitted_world_fingerprint: canonical_world_fingerprint(world.world()),
        };
        phase("metrics+fingerprints", &mut phase_started);
        Ok(CertifiedCandidate {
            candidate,
            world,
            equivalence,
            timing_graph,
            manifest,
            measurements,
            metrics,
        })
    }
}

pub fn external_signal_is_high(strength: u8) -> bool {
    strength > 0
}

fn certify_exhaustive_truth(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    config: &CertificationConfig,
) -> Result<usize, CandidateCertificationError> {
    let threads = certification_sweep_threads(world);
    if std::env::var_os("REDA_PHASE_TIMING").is_some() {
        eprintln!(
            "WORK exhaustive_workers {}",
            certification_workers(
                exhaustive_state_count(lowered)?,
                threads,
                MIN_VECTORS_PER_CERTIFICATION_WORKER
            )
        );
    }
    certify_exhaustive_truth_with_threads(world, candidate, lowered, compatibility, config, threads)
}

fn exhaustive_state_count(lowered: &Netlist) -> Result<usize, CandidateCertificationError> {
    1usize
        .checked_shl(u32::try_from(lowered.inputs.len()).unwrap_or(u32::MAX))
        .ok_or(CandidateCertificationError::CounterOverflow)
}

fn certify_exhaustive_truth_with_threads(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    config: &CertificationConfig,
    threads: usize,
) -> Result<usize, CandidateCertificationError> {
    let state_count = exhaustive_state_count(lowered)?;
    // The canonical mask range is the only thing retained per vector: each
    // worker owns one simulator at a time and reduces `()`, so a wide sweep
    // costs workers, not one world per mask.
    let masks = (0..state_count).collect::<Vec<_>>();
    run_certification_chunks(
        &masks,
        threads,
        MIN_VECTORS_PER_CERTIFICATION_WORKER,
        |_, &mask| {
            let vector = bits_of(mask, lowered.inputs.len());
            let mut simulator = fresh_simulator(world, candidate, lowered);
            drive_vector(&mut simulator, candidate, lowered, compatibility, &vector)?;
            settle(
                &mut simulator,
                mask,
                TransitionPhase::ExhaustiveVector,
                0,
                config,
            )?;
            enforce_event_cap(&simulator, 0, mask, config)?;
            check_outputs(
                simulator.world(),
                candidate,
                lowered,
                compatibility,
                &vector,
                mask,
            )
        },
    )?;
    Ok(state_count)
}

fn sweep_manifest(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    manifest: &TransitionManifest,
    config: &CertificationConfig,
) -> Result<Vec<TransitionMeasurement>, CandidateCertificationError> {
    let threads = certification_sweep_threads(world);
    sweep_manifest_with_threads(
        world,
        candidate,
        lowered,
        compatibility,
        manifest,
        config,
        threads,
    )
}

/// One compilation-wide budget rule.
///
/// Auto and a deliberate `REDA_CERT_THREADS` override both clamp to at least
/// one worker and to the host's own parallelism. There is no machine-derived
/// ceiling above that: an operator asking for 32 workers on a 32-core host
/// gets 32, and on a one-core host gets one.
pub(super) fn certification_thread_budget(available: usize, requested: Option<usize>) -> usize {
    let available = available.max(1);
    requested.unwrap_or(available).clamp(1, available)
}

/// The only parser for `REDA_CERT_THREADS`.
///
/// Anything that is not a plain decimal count -- empty, signed, fractional or
/// wider than `usize` -- is not an override, so the compile stays automatic.
fn parse_certification_threads(raw: Option<&str>) -> Option<usize> {
    raw.and_then(|value| value.parse::<usize>().ok())
}

/// The only parser for `REDA_CERT_MEMORY_BYTES`, falling back to the default
/// policy constant. A malformed budget is not a reason to abandon the ceiling.
fn certification_memory_budget_bytes(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(CERT_WORKER_MEMORY_BUDGET_BYTES)
}

/// Bytes one certification worker is estimated to own for `size`.
///
/// The formula estimates the dense cell vector directly: `World` stores one
/// palette index per cell, so one copy is `size_of::<u32>() * volume`. A
/// `World` also carries `positions_by_kind` and `dirty`, which this does not
/// model cell by cell; their clone cost is folded conservatively into
/// `WORLD_COPY_HEADROOM` along with the copies a worker holds at once.
///
/// The arithmetic saturates rather than overflowing: an absurd world reports
/// `usize::MAX` and therefore one worker, and an empty world still reports one
/// byte so it can never become a zero divisor.
fn world_worker_memory_bytes(size: (i32, i32, i32)) -> usize {
    let axis = |value: i32| usize::try_from(value).unwrap_or(0);
    let (x, y, z) = size;
    let volume = axis(x).saturating_mul(axis(y)).saturating_mul(axis(z));
    std::mem::size_of::<u32>()
        .saturating_mul(volume)
        .saturating_mul(WORLD_COPY_HEADROOM)
        .max(1)
}

/// How many workers `budget_bytes` pays for, clamped to at least one.
fn memory_worker_ceiling(per_worker_bytes: usize, budget_bytes: usize) -> usize {
    (budget_bytes / per_worker_bytes.max(1)).max(1)
}

/// The worker budget one sweep of `world_size` may actually open.
///
/// An automatic budget is clamped by the byte ceiling, so a 64- or 128-core
/// host cannot multiply cloned worlds without bound. A deliberate operator
/// override is a decision, not a guess, and is left alone.
fn sweep_worker_budget(
    budget: usize,
    world_size: (i32, i32, i32),
    explicit: Option<usize>,
    memory_budget_bytes: usize,
) -> usize {
    let budget = budget.max(1);
    if explicit.is_some() {
        return budget;
    }
    budget.min(memory_worker_ceiling(
        world_worker_memory_bytes(world_size),
        memory_budget_bytes,
    ))
}

/// Run `body` with `threads` as this thread's certification worker budget.
///
/// The budget is restored on normal return and on unwind, so a nested scope
/// cannot widen or narrow its parent's budget after it finishes. Whether this
/// compile's count came from an operator override is inherited, because
/// narrowing a budget does not turn a deliberate one into a guess.
pub(super) fn with_certification_threads<T>(threads: usize, body: impl FnOnce() -> T) -> T {
    let explicit = CERTIFICATION_THREADS
        .get()
        .and_then(|budget| budget.explicit);
    let scoped = ScopedWorkerBudget {
        workers: threads,
        explicit,
    };
    let _restore = CertificationThreadsReset(CERTIFICATION_THREADS.replace(Some(scoped)));
    body()
}

/// Parse and clamp this compile's worker budget once, then own it for the
/// whole compile.
///
/// A public entry reached inside a budget that is already owned -- a leaf
/// worker compiling its own module, or the hierarchical flat fast path --
/// keeps that budget instead of reopening the machine underneath its parent.
pub(super) fn with_compile_worker_budget<T>(body: impl FnOnce(usize) -> T) -> T {
    if let Some(budget) = CERTIFICATION_THREADS.get() {
        return body(budget.workers.max(1));
    }
    let raw = std::env::var("REDA_CERT_THREADS").ok();
    let explicit = parse_certification_threads(raw.as_deref());
    let available = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let workers = certification_thread_budget(available, explicit);
    let _restore = CertificationThreadsReset(CERTIFICATION_THREADS.replace(Some(
        ScopedWorkerBudget { workers, explicit },
    )));
    body(workers)
}

/// Private reset guard: it restores the previous budget when `body` returns or
/// panics, and cannot be moved to another thread because it never leaves
/// `with_certification_threads` or `with_compile_worker_budget`.
struct CertificationThreadsReset(Option<ScopedWorkerBudget>);

impl Drop for CertificationThreadsReset {
    fn drop(&mut self) {
        // Ignore a destroyed thread-local: panicking inside a drop that already
        // runs during unwinding would abort the process.
        let _ = CERTIFICATION_THREADS.try_with(|threads| threads.set(self.0));
    }
}

pub(super) fn scoped_certification_threads() -> Option<usize> {
    CERTIFICATION_THREADS.get().map(|budget| budget.workers)
}

/// The requested worker budget for one certification sweep.
///
/// A scope owns the whole compile's budget. Without one -- a sweep reached
/// outside any public entry, which only tests do -- the same policy is applied
/// to the environment directly, so there is one parse and one clamp either way.
fn requested_certification_threads() -> usize {
    #[cfg(test)]
    record_observed_worker_budget(scoped_certification_threads());
    if let Some(threads) = scoped_certification_threads() {
        return threads.max(1);
    }
    let raw = std::env::var("REDA_CERT_THREADS").ok();
    let available = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    certification_thread_budget(available, parse_certification_threads(raw.as_deref()))
}

/// The deliberate override this sweep is running under, if any.
fn scoped_explicit_threads() -> Option<usize> {
    match CERTIFICATION_THREADS.get() {
        Some(budget) => budget.explicit,
        None => {
            let raw = std::env::var("REDA_CERT_THREADS").ok();
            parse_certification_threads(raw.as_deref())
        }
    }
}

/// The budget one sweep over `world` may open: this compile's budget, with the
/// byte memory ceiling applied to an automatic one.
///
/// Both sweeps come through here, so exhaustive and manifest work can never
/// disagree about the budget or about the memory that pays for it.
fn certification_sweep_threads(world: &World) -> usize {
    let raw_bytes = std::env::var("REDA_CERT_MEMORY_BYTES").ok();
    sweep_worker_budget(
        requested_certification_threads(),
        world.size(),
        scoped_explicit_threads(),
        certification_memory_budget_bytes(raw_bytes.as_deref()),
    )
}

/// Test-only: the worker budget every certification sweep on THIS thread
/// observed while `body` ran.
///
/// Per calling thread on purpose -- a concurrent compile in another test
/// cannot pollute the record, and a leaf worker's own budget is observed where
/// that worker runs, not here.
#[cfg(test)]
pub(super) fn record_caller_certification_budgets<T>(
    body: impl FnOnce() -> T,
) -> (T, Vec<Option<usize>>) {
    /// Restores whatever recorder was running before, on return or on unwind.
    struct RecorderReset(Option<Vec<Option<usize>>>);

    impl Drop for RecorderReset {
        fn drop(&mut self) {
            let _ = BUDGET_RECORDER.try_with(|recorder| *recorder.borrow_mut() = self.0.take());
        }
    }

    let _restore =
        RecorderReset(BUDGET_RECORDER.with(|recorder| recorder.borrow_mut().replace(Vec::new())));
    let value = body();
    let recorded = BUDGET_RECORDER
        .with(|recorder| recorder.borrow_mut().take())
        .unwrap_or_default();
    (value, recorded)
}

#[cfg(test)]
thread_local! {
    static BUDGET_RECORDER: std::cell::RefCell<Option<Vec<Option<usize>>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn record_observed_worker_budget(budget: Option<usize>) {
    let _ = BUDGET_RECORDER.try_with(|recorder| {
        if let Some(observed) = recorder.borrow_mut().as_mut() {
            observed.push(budget);
        }
    });
}

/// The one actual-worker rule shared by every certification sweep.
///
/// Empty work reports zero workers and runs no closure. Otherwise workers are
/// the requested budget capped by whole `min_items_per_worker` chunks, clamped
/// to at least one.
///
/// Each caller passes the threshold its own unit needs, and the two rest on
/// different evidence: `MIN_VECTORS_PER_CERTIFICATION_WORKER` is a measured
/// crossover that keeps exhaustive work too small to amortize thread startup on
/// the caller thread, while `MIN_TRANSITIONS_PER_CERTIFICATION_WORKER` is one
/// because that measured better on the wide manifest sweeps -- narrower ones
/// are unmeasured, so it is not a claim that a transition always pays for its
/// own worker. `requested` already carries the compile-wide budget, the memory
/// ceiling and the host's parallelism, so this only ever narrows it.
fn certification_workers(items: usize, requested: usize, min_items_per_worker: usize) -> usize {
    if items == 0 {
        return 0;
    }
    requested.min(items / min_items_per_worker.max(1)).max(1)
}

/// Apply the shared worker policy to `items`, then reduce in logical index order.
///
/// The actual worker count is derived once here and decides both whether this
/// sweep takes the process-wide lock and how `run_indexed_chunks` partitions,
/// so the lock can never disagree with the parallelism it guards.
fn run_certification_chunks<T, U, E, F>(
    items: &[T],
    requested: usize,
    min_items_per_worker: usize,
    work: F,
) -> Result<Vec<U>, E>
where
    T: Sync,
    U: Send,
    E: Send,
    F: Fn(usize, &T) -> Result<U, E> + Sync,
{
    let workers = certification_workers(items.len(), requested, min_items_per_worker);
    // ponytail: the ceiling here is one process-wide lock, so two concurrent
    // compiles serialize their above-threshold sweeps rather than share the
    // machine. Replace it with a process-wide budget arbiter when concurrent
    // compile throughput matters more than one compile's latency.
    let _sweep = (workers > 1).then(certification_sweep_guard);
    run_indexed_chunks(items, workers.max(1), work)
}

/// The process-wide certification-sweep boundary, shared by the exhaustive and
/// manifest sweeps: two independent compiles cannot each open the full worker
/// count at once.
fn certification_sweep_guard() -> std::sync::MutexGuard<'static, ()> {
    CERTIFICATION_SWEEP_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sweep_manifest_with_threads(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    manifest: &TransitionManifest,
    config: &CertificationConfig,
    threads: usize,
) -> Result<Vec<TransitionMeasurement>, CandidateCertificationError> {
    let groups = transition_source_groups(manifest.transitions());
    let workers = certification_workers(
        groups.len(),
        threads,
        MIN_TRANSITIONS_PER_CERTIFICATION_WORKER,
    );
    let batches = transition_group_batches(&groups, workers);
    if std::env::var_os("REDA_PHASE_TIMING").is_some() {
        eprintln!("WORK manifest_workers {}", batches.len());
    }
    let measurements = run_certification_chunks(
        &batches,
        workers,
        MIN_TRANSITIONS_PER_CERTIFICATION_WORKER,
        |_, batch| {
            let groups = batch
                .iter()
                .map(|(manifest_index, transitions)| {
                    measure_transition_group(
                        world,
                        candidate,
                        lowered,
                        compatibility,
                        transitions,
                        *manifest_index,
                        config,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, CandidateCertificationError>(groups.into_iter().flatten().collect::<Vec<_>>())
        },
    )?;
    Ok(measurements.into_iter().flatten().collect())
}

fn transition_source_groups(transitions: &[Transition]) -> Vec<(usize, &[Transition])> {
    let mut manifest_index = 0;
    transitions
        .chunk_by(|left, right| left.from == right.from)
        .map(|group| {
            let indexed = (manifest_index, group);
            manifest_index += group.len();
            indexed
        })
        .collect()
}

fn transition_group_batches<'groups, 'transitions>(
    groups: &'groups [(usize, &'transitions [Transition])],
    workers: usize,
) -> Vec<&'groups [(usize, &'transitions [Transition])]> {
    let batch_count = workers.max(1).min(groups.len());
    let mut remaining_weight = groups.iter().map(|(_, group)| group.len()).sum::<usize>();
    let mut start = 0;
    let mut batches = Vec::with_capacity(batch_count);
    while start < groups.len() {
        let batches_left = batch_count - batches.len();
        if batches_left == 1 {
            batches.push(&groups[start..]);
            break;
        }
        let target = remaining_weight.div_ceil(batches_left);
        let latest_end = groups.len() - (batches_left - 1);
        let mut end = start;
        let mut weight = 0;
        while end < latest_end && weight < target {
            weight += groups[end].1.len();
            end += 1;
        }
        batches.push(&groups[start..end]);
        remaining_weight -= weight;
        start = end;
    }
    batches
}

/// Run `work` over contiguous chunks of `items` and reduce in logical index order.
///
/// One worker, or no items, runs on the caller thread and spawns nothing. Every
/// handle is joined before the reduction, so the lowest-index typed error wins
/// over any later error, and the lowest-index panic is resumed when no earlier
/// typed error exists, whatever order the workers finished in.
fn run_indexed_chunks<T, U, E, F>(items: &[T], threads: usize, work: F) -> Result<Vec<U>, E>
where
    T: Sync,
    U: Send,
    E: Send,
    F: Fn(usize, &T) -> Result<U, E> + Sync,
{
    let worker_count = threads.max(1).min(items.len().max(1));
    if worker_count == 1 {
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| work(index, item))
            .collect();
    }

    let work = &work;
    let chunk_len = items.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        let handles = items
            .chunks(chunk_len)
            .enumerate()
            .map(|(chunk_index, chunk)| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(index, item)| work(chunk_index * chunk_len + index, item))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>();
        let mut values = Vec::with_capacity(items.len());
        for outcome in outcomes {
            match outcome {
                Ok(chunk) => values.extend(chunk?),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        Ok(values)
    })
}

fn measure_transition_group(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    transitions: &[Transition],
    first_manifest_index: usize,
    config: &CertificationConfig,
) -> Result<Vec<TransitionMeasurement>, CandidateCertificationError> {
    let Some(first) = transitions.first() else {
        return Ok(Vec::new());
    };
    debug_assert!(transitions
        .iter()
        .all(|transition| transition.from == first.from));
    let (simulator, events_before) = prepare_transition_source(
        world,
        candidate,
        lowered,
        compatibility,
        &first.from,
        first_manifest_index,
        config,
    )?;
    let (last, prefix) = transitions.split_last().expect("the group is not empty");
    let mut measurements = prefix
        .iter()
        .enumerate()
        .map(|(offset, transition)| {
            measure_transition_from_source(
                (simulator.clone(), events_before),
                candidate,
                lowered,
                compatibility,
                transition,
                first_manifest_index + offset,
                config,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    measurements.push(measure_transition_from_source(
        (simulator, events_before),
        candidate,
        lowered,
        compatibility,
        last,
        first_manifest_index + prefix.len(),
        config,
    )?);
    Ok(measurements)
}

fn prepare_transition_source(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    from: &[bool],
    manifest_index: usize,
    config: &CertificationConfig,
) -> Result<(Simulator, u64), CandidateCertificationError> {
    let mut simulator = fresh_simulator(world, candidate, lowered);
    let events_before = simulator.work_done();
    drive_vector(&mut simulator, candidate, lowered, compatibility, from)?;
    settle(
        &mut simulator,
        manifest_index,
        TransitionPhase::Source,
        events_before,
        config,
    )?;
    enforce_event_cap(&simulator, events_before, manifest_index, config)?;
    check_outputs(
        simulator.world(),
        candidate,
        lowered,
        compatibility,
        from,
        manifest_index,
    )?;
    Ok((simulator, events_before))
}

fn measure_transition_from_source(
    source: (Simulator, u64),
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    transition: &Transition,
    manifest_index: usize,
    config: &CertificationConfig,
) -> Result<TransitionMeasurement, CandidateCertificationError> {
    let (mut simulator, events_before) = source;
    let start_tick = simulator.current_tick();
    drive_vector(
        &mut simulator,
        candidate,
        lowered,
        compatibility,
        &transition.to,
    )?;
    settle(
        &mut simulator,
        manifest_index,
        TransitionPhase::Destination,
        events_before,
        config,
    )?;
    let simulator_events = simulator
        .work_done()
        .checked_sub(events_before)
        .ok_or(CandidateCertificationError::CounterOverflow)?;
    enforce_event_cap(&simulator, events_before, manifest_index, config)?;
    check_outputs(
        simulator.world(),
        candidate,
        lowered,
        compatibility,
        &transition.to,
        manifest_index,
    )?;
    Ok(TransitionMeasurement {
        manifest_index,
        start_tick,
        settle_game_ticks: simulator
            .current_tick()
            .checked_sub(start_tick)
            .ok_or(CandidateCertificationError::CounterOverflow)?,
        simulator_events,
    })
}

#[cfg(test)]
fn measure_transition_fresh(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    transition: &Transition,
    manifest_index: usize,
    config: &CertificationConfig,
) -> Result<TransitionMeasurement, CandidateCertificationError> {
    let (simulator, events_before) = prepare_transition_source(
        world,
        candidate,
        lowered,
        compatibility,
        &transition.from,
        manifest_index,
        config,
    )?;
    measure_transition_from_source(
        (simulator, events_before),
        candidate,
        lowered,
        compatibility,
        transition,
        manifest_index,
        config,
    )
}

fn enforce_event_cap(
    simulator: &Simulator,
    events_before: u64,
    manifest_index: usize,
    config: &CertificationConfig,
) -> Result<(), CandidateCertificationError> {
    let used = simulator
        .work_done()
        .checked_sub(events_before)
        .ok_or(CandidateCertificationError::CounterOverflow)?;
    if used > config.max_simulator_events_per_transition {
        return Err(CandidateCertificationError::SimulatorEventCapExceeded {
            manifest_index,
            used,
            limit: config.max_simulator_events_per_transition,
        });
    }
    Ok(())
}

fn settle(
    simulator: &mut Simulator,
    manifest_index: usize,
    phase: TransitionPhase,
    events_before: u64,
    config: &CertificationConfig,
) -> Result<(), CandidateCertificationError> {
    let used = simulator
        .work_done()
        .checked_sub(events_before)
        .ok_or(CandidateCertificationError::CounterOverflow)?;
    let remaining = config
        .max_simulator_events_per_transition
        .saturating_sub(used);
    match simulator.run_until_stable_bounded(config.max_game_ticks_per_transition, remaining) {
        Ok(_) => Ok(()),
        Err(BoundedSimulationError::WorkLimitExceeded { .. }) => {
            Err(CandidateCertificationError::SimulatorEventCapExceeded {
                manifest_index,
                used: simulator
                    .work_done()
                    .checked_sub(events_before)
                    .ok_or(CandidateCertificationError::CounterOverflow)?,
                limit: config.max_simulator_events_per_transition,
            })
        }
        Err(BoundedSimulationError::Simulation(simulation_error)) => {
            Err(CandidateCertificationError::TransitionDidNotSettle {
                manifest_index,
                phase,
                simulation_error,
            })
        }
    }
}

fn fresh_simulator(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
) -> Simulator {
    let mut world = world.clone();
    for output in &lowered.outputs {
        if let Some(pin) = candidate.pins.get(output) {
            compile::probe_caller_cell(&mut world, (pin.at.x, pin.at.y, pin.at.z));
        }
    }
    Simulator::new(world)
}

fn drive_vector(
    simulator: &mut Simulator,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    vector: &[bool],
) -> Result<(), CandidateCertificationError> {
    if vector.len() != lowered.inputs.len() {
        return Err(CandidateCertificationError::InputWidthMismatch {
            expected: lowered.inputs.len(),
            actual: vector.len(),
        });
    }
    for ((name, &bit), position) in lowered.inputs.iter().zip(vector).zip(
        lowered
            .inputs
            .iter()
            .map(|name| compatibility.input_positions[name]),
    ) {
        if let Some(pin) = candidate.pins.get(name) {
            let current = simulator.world().get(pin.at.x, pin.at.y, pin.at.z).kind;
            let already_driven = matches!(
                (bit, current),
                (true, BlockKind::RedstoneBlock) | (false, BlockKind::Air)
            );
            if !already_driven {
                compile::drive_caller_cell(
                    simulator.world_mut(),
                    (pin.at.x, pin.at.y, pin.at.z),
                    bit,
                );
            }
        } else {
            let current = simulator.world().get(position.0, position.1, position.2);
            if current.lit != bit {
                let mut state = current.clone();
                state.lit = bit;
                simulator
                    .world_mut()
                    .set(position.0, position.1, position.2, state);
            }
        }
    }
    Ok(())
}

fn check_outputs(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    vector: &[bool],
    manifest_index: usize,
) -> Result<(), CandidateCertificationError> {
    let expected = evaluate_lowered(lowered, vector)?;
    for ((name, &expected), &(x, y, z)) in lowered.outputs.iter().zip(&expected).zip(
        lowered
            .outputs
            .iter()
            .map(|name| &compatibility.output_positions[name]),
    ) {
        let state = world.get(x, y, z);
        let actual = if candidate.pins.get(name).is_some() {
            external_signal_is_high(block_signal_at(world, Position::new(x, y, z)).1)
        } else if state.kind == BlockKind::RedstoneWire {
            external_signal_is_high(state.power)
        } else {
            state.lit
        };
        if actual != expected {
            return Err(CandidateCertificationError::FunctionalMismatch {
                manifest_index,
                output: name.clone(),
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn evaluate_lowered(
    lowered: &Netlist,
    vector: &[bool],
) -> Result<Vec<bool>, CandidateCertificationError> {
    if vector.len() != lowered.inputs.len() {
        return Err(CandidateCertificationError::InputWidthMismatch {
            expected: lowered.inputs.len(),
            actual: vector.len(),
        });
    }
    let mut values = lowered
        .inputs
        .iter()
        .cloned()
        .zip(vector.iter().copied())
        .collect::<BTreeMap<_, _>>();
    for gate_index in lowered
        .combinational_order()
        .ok_or(CandidateCertificationError::CombinationalCycle)?
    {
        let gate = &lowered.gates[gate_index];
        let inputs = gate
            .inputs
            .iter()
            .map(|name| {
                values.get(name).copied().ok_or_else(|| {
                    CandidateCertificationError::UnresolvedLogicalSignal {
                        signal: name.clone(),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        values.insert(gate.output.clone(), gate.kind.evaluate(&inputs));
    }
    lowered
        .outputs
        .iter()
        .map(|name| {
            values.get(name).copied().ok_or_else(|| {
                CandidateCertificationError::UnresolvedLogicalSignal {
                    signal: name.clone(),
                }
            })
        })
        .collect()
}

fn bits_of(mask: usize, width: usize) -> Vec<bool> {
    (0..width)
        .map(|index| (mask >> (width - 1 - index)) & 1 == 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_world_fingerprint, certification_memory_budget_bytes, certification_sweep_guard,
        certification_thread_budget, certification_workers, certify_exhaustive_truth_with_threads,
        drive_vector, external_signal_is_high, fresh_simulator, memory_worker_ceiling,
        measure_transition_fresh, parse_certification_threads, run_certification_chunks,
        run_indexed_chunks, scoped_certification_threads, settle, sweep_manifest_with_threads,
        sweep_worker_budget, transition_group_batches, transition_source_groups,
        with_certification_threads, with_compile_worker_budget, world_worker_memory_bytes,
        CandidateCertificationError, CompleteCandidateCertifier, ExpandedCandidateCertifier,
        RealisedTimingGraph, TimingGraphError, TransitionManifest, TransitionMeasurement,
        TransitionPhase, CERTIFICATION_SWEEP_LOCK, CERT_WORKER_MEMORY_BUDGET_BYTES,
        MIN_TRANSITIONS_PER_CERTIFICATION_WORKER, MIN_VECTORS_PER_CERTIFICATION_WORKER,
        WORLD_COPY_HEADROOM,
    };
    use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
    use crate::compile::fragment_synth::legacy_adapter::LegacyCandidateAdapter;
    use crate::compile::fragment_synth::realise::realise_and_verify_expanded;
    use crate::compile::fragment_synth::verify::CertificationIdentity;
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::topology::Library;
    use crate::compile::{compile_legacy, Gate, Netlist};
    use crate::redstone::simulator::position::Position;
    use crate::redstone::simulator::{SimulationError, Simulator};
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};
    use crate::redstone::world::storage::World;

    #[test]
    fn external_high_is_any_nonzero_strength() {
        assert!(!external_signal_is_high(0));
        assert!(external_signal_is_high(1));
        assert!(external_signal_is_high(15));
    }

    /// One compile-wide budget policy, shared by the flat and hierarchical
    /// entries and by both certification sweeps.
    ///
    /// The old machine-derived 12-worker ceiling is gone: a deliberate
    /// operator override is honoured up to the host parallelism, never past it.
    #[test]
    fn certification_thread_budget_replaces_the_twelve_worker_ceiling() {
        assert_eq!(certification_thread_budget(8, None), 8);
        assert_eq!(certification_thread_budget(8, Some(4)), 4);
        assert_eq!(certification_thread_budget(8, Some(0)), 1);
        assert_eq!(
            certification_thread_budget(32, Some(32)),
            32,
            "an explicit override clamps only to available parallelism"
        );
        assert_eq!(
            certification_thread_budget(1, Some(32)),
            1,
            "a one-core host never opens 32 workers"
        );
        assert_eq!(certification_thread_budget(64, None), 64);
        assert_eq!(certification_thread_budget(0, None), 1);
    }

    /// `World` is dense, so one worker costs
    /// `size_of::<u32>() * volume * WORLD_COPY_HEADROOM` bytes. Auto
    /// parallelism divides the byte budget by that estimate; a deliberate
    /// operator override is not memory-clamped.
    #[test]
    fn auto_workers_obey_the_dense_world_byte_memory_ceiling() {
        let size = (16, 8, 32);
        let volume = 16 * 8 * 32;
        assert!(WORLD_COPY_HEADROOM >= 1, "a worker owns at least one world");
        assert_eq!(
            world_worker_memory_bytes(size),
            std::mem::size_of::<u32>() * volume * WORLD_COPY_HEADROOM
        );
        assert_eq!(
            world_worker_memory_bytes((i32::MAX, i32::MAX, i32::MAX)),
            usize::MAX,
            "an absurd dense volume saturates instead of overflowing"
        );
        assert!(
            world_worker_memory_bytes((0, 0, 0)) >= 1,
            "an empty world must not become a zero divisor"
        );

        let per_worker = world_worker_memory_bytes(size);
        assert_eq!(memory_worker_ceiling(per_worker, per_worker * 3), 3);
        assert_eq!(
            memory_worker_ceiling(per_worker, per_worker - 1),
            1,
            "the ceiling is clamped to at least one worker"
        );
        assert_eq!(memory_worker_ceiling(per_worker, 0), 1);
        assert!(
            memory_worker_ceiling(0, 1024) >= 1,
            "a zero-byte estimate must not divide by zero"
        );

        assert_eq!(sweep_worker_budget(32, size, None, per_worker * 3), 3);
        assert_eq!(sweep_worker_budget(32, size, None, per_worker * 64), 32);
        assert_eq!(
            sweep_worker_budget(32, size, Some(32), per_worker),
            32,
            "an explicit operator override is not memory-clamped"
        );
        assert_eq!(sweep_worker_budget(32, size, None, 0), 1);
        assert_eq!(sweep_worker_budget(0, size, None, usize::MAX), 1);

        // ponytail: the memory ceiling is applied where a `World` exists -- the
        // sweep -- because no public entry holds one yet. The entry contributes
        // only the parse and the parallelism clamp. Move the ceiling to the
        // entry when a compile can estimate its emitted world up front.

        let world = World::new(4, 3, 5);
        assert_eq!(
            world_worker_memory_bytes(world.size()),
            std::mem::size_of::<u32>() * 4 * 3 * 5 * WORLD_COPY_HEADROOM,
            "the estimate reads the certified world's own dense size"
        );
    }

    /// One parser for both certification environment overrides. Anything
    /// that is not a plain decimal count is not an override at all.
    #[test]
    fn the_certification_environment_overrides_are_parsed_by_one_policy() {
        assert_eq!(parse_certification_threads(None), None);
        assert_eq!(parse_certification_threads(Some("4")), Some(4));
        assert_eq!(
            parse_certification_threads(Some("0")),
            Some(0),
            "zero is a deliberate override; the budget policy clamps it to one"
        );
        assert!(
            CERT_WORKER_MEMORY_BUDGET_BYTES >= 1,
            "the measured byte budget must fit at least one worker"
        );
        assert_eq!(
            certification_memory_budget_bytes(None),
            CERT_WORKER_MEMORY_BUDGET_BYTES
        );
        assert_eq!(certification_memory_budget_bytes(Some("1048576")), 1_048_576);
        assert_eq!(certification_memory_budget_bytes(Some("0")), 0);
        for junk in ["", " ", "-1", "two", "4.0", "1e3", "99999999999999999999999999"] {
            assert_eq!(
                parse_certification_threads(Some(junk)),
                None,
                "`{junk}` is not a worker count"
            );
            assert_eq!(
                certification_memory_budget_bytes(Some(junk)),
                CERT_WORKER_MEMORY_BUDGET_BYTES,
                "`{junk}` is not a byte budget"
            );
        }
    }

    /// A public entry parses and clamps once, owns the budget for the whole
    /// compile, and never widens a budget it was handed.
    #[test]
    fn one_compile_scope_owns_the_parsed_and_clamped_worker_budget() {
        let available = std::thread::available_parallelism().map_or(1, |count| count.get());
        let expected = certification_thread_budget(
            available,
            parse_certification_threads(std::env::var("REDA_CERT_THREADS").ok().as_deref()),
        );

        let observed = with_compile_worker_budget(|budget| {
            assert_eq!(
                budget, expected,
                "the public entry applies the one shared parse and clamp policy"
            );
            assert_eq!(scoped_certification_threads(), Some(budget));
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    assert_eq!(
                        scoped_certification_threads(),
                        None,
                        "a worker must not inherit an accidental wider budget"
                    )
                });
            });
            budget
        });
        assert_eq!(observed, expected);
        assert_eq!(
            scoped_certification_threads(),
            None,
            "the compile scope closes on return"
        );

        with_certification_threads(1, || {
            with_compile_worker_budget(|budget| {
                assert_eq!(
                    budget, 1,
                    "a public entry inside an owned budget must not reopen the machine"
                )
            });
            assert_eq!(scoped_certification_threads(), Some(1));
        });

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_compile_worker_budget(|_| panic!("entry panic"));
        }));
        assert!(panicked.is_err());
        assert_eq!(
            scoped_certification_threads(),
            None,
            "a panicking entry restores the previous budget"
        );
    }

    #[test]
    fn certification_worker_count_is_one_pure_threshold_rule() {
        let min = MIN_VECTORS_PER_CERTIFICATION_WORKER;
        assert!(
            min >= 2,
            "a measured crossover below two items cannot keep small work on the caller thread"
        );

        assert_eq!(
            certification_workers(0, 8, min),
            0,
            "empty work runs no closure"
        );
        assert_eq!(certification_workers(min - 1, 8, min), 1);
        assert_eq!(certification_workers(min, 8, min), 1);
        assert_eq!(certification_workers(min * 2, 8, min), 2);
        assert_eq!(certification_workers(min * 8, 3, min), 3);
        assert_eq!(certification_workers(min * 8, 1, min), 1);
        assert_eq!(certification_workers(min * 8, 0, min), 1);
    }

    #[test]
    fn worker_policy_scales_each_threshold_independently() {
        // This calls `certification_workers` directly. It pins the policy each
        // threshold produces; it does NOT prove that `sweep_manifest` and
        // `certify_exhaustive_truth` pass the thresholds named here. Reading the
        // constants rather than bare literals keeps the two in step by
        // inspection, and that is the whole of the guarantee -- repointing a
        // production callsite at the other constant would still pass here.
        let transition = MIN_TRANSITIONS_PER_CERTIFICATION_WORKER;
        assert_eq!(
            transition, 1,
            "one transition per worker measured better on the wide manifest \
             sweeps; narrower sweeps are unmeasured"
        );

        // ripple_adder8's top manifest at the auto budget: 68 transitions and a
        // requested 12 must open 12 workers, as the retained serial-exhaustive
        // revision did. The same count of lighter exhaustive vectors keeps the
        // measured eight-per-worker crossover.
        assert_eq!(certification_workers(68, 12, transition), 12);
        assert_eq!(
            certification_workers(68, 12, MIN_VECTORS_PER_CERTIFICATION_WORKER),
            8
        );

        assert_eq!(
            certification_workers(0, 12, transition),
            0,
            "empty work runs no closure at either threshold"
        );
        assert_eq!(
            certification_workers(1, 12, transition),
            1,
            "one transition never opens a worker with nothing to do"
        );
        assert_eq!(certification_workers(3, 12, transition), 3);
        assert_eq!(
            certification_workers(68, 4, transition),
            4,
            "the compile-wide budget still bounds a heavy sweep"
        );
        assert_eq!(
            certification_workers(68, 0, transition),
            1,
            "a zero budget still runs the sweep on the caller thread"
        );
    }

    #[test]
    fn certification_chunks_below_the_threshold_stay_on_the_caller_thread() {
        let caller = std::thread::current().id();
        let items: Vec<usize> = (0..MIN_VECTORS_PER_CERTIFICATION_WORKER - 1).collect();

        let values = run_certification_chunks(
            &items,
            8,
            MIN_VECTORS_PER_CERTIFICATION_WORKER,
            |index, item| {
                assert_eq!(
                    std::thread::current().id(),
                    caller,
                    "work below the measured threshold must not spawn a worker"
                );
                Ok::<_, CandidateCertificationError>(index + item)
            },
        )
        .expect("every chunk succeeds");
        assert_eq!(values, items.iter().map(|item| item * 2).collect::<Vec<_>>());

        let empty: Vec<usize> = Vec::new();
        assert!(
            run_certification_chunks(
                &empty,
                8,
                MIN_TRANSITIONS_PER_CERTIFICATION_WORKER,
                |_, _| Err::<usize, _>(CandidateCertificationError::CounterOverflow)
            )
            .expect("empty work runs no closure")
            .is_empty()
        );
    }

    #[test]
    fn indexed_chunks_return_logical_order_not_completion_order() {
        let items: Vec<usize> = (0..4).collect();
        // Release chunks from the highest logical index downwards, so completion
        // order is the exact reverse of the logical order.
        let next = std::sync::Mutex::new(items.len() - 1);
        let released = std::sync::Condvar::new();

        let values = run_indexed_chunks(&items, items.len(), |index, item| {
            let mut turn = next.lock().unwrap();
            while *turn != index {
                let (guard, timeout) = released
                    .wait_timeout(turn, std::time::Duration::from_secs(5))
                    .unwrap();
                assert!(!timeout.timed_out(), "chunk {index} never ran concurrently");
                turn = guard;
            }
            *turn = index.wrapping_sub(1);
            released.notify_all();
            Ok::<_, CandidateCertificationError>(item * 10)
        })
        .expect("every chunk succeeds");

        assert_eq!(values, vec![0, 10, 20, 30]);
    }

    #[test]
    fn earlier_manifest_error_wins_over_a_later_worker_panic() {
        let items: Vec<usize> = (0..4).collect();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_indexed_chunks(&items, items.len(), |index, _| match index {
                1 => Err(CandidateCertificationError::CounterOverflow),
                2 => Err(CandidateCertificationError::CombinationalCycle),
                3 => panic!("later panic"),
                _ => Ok(index),
            })
        }));

        assert!(matches!(
            result,
            Ok(Err(CandidateCertificationError::CounterOverflow))
        ));

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_indexed_chunks(&items, items.len(), |index, _| match index {
                0 => panic!("first panic"),
                1 => Err(CandidateCertificationError::CounterOverflow),
                3 => panic!("later panic"),
                _ => Ok(index),
            })
        }))
        .expect_err("the earliest panic must be resumed");
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"first panic"));
    }

    #[test]
    fn indexed_chunks_stay_on_the_caller_thread_for_zero_or_one_worker() {
        let caller = std::thread::current().id();
        let empty: Vec<usize> = Vec::new();
        let items: Vec<usize> = (0..3).collect();

        for threads in [0, 1] {
            let none = run_indexed_chunks(&empty, threads, |_, _| {
                Err::<usize, _>(CandidateCertificationError::CounterOverflow)
            })
            .expect("no chunk runs without items");
            assert!(none.is_empty());

            let values = run_indexed_chunks(&items, threads, |index, item| {
                assert_eq!(std::thread::current().id(), caller);
                Ok::<_, CandidateCertificationError>(index + item)
            })
            .expect("every chunk succeeds");
            assert_eq!(values, vec![0, 2, 4]);
        }
    }

    #[test]
    fn certification_thread_scope_restores_the_previous_budget_after_success_and_panic() {
        assert_eq!(
            scoped_certification_threads(),
            None,
            "no compile owns the worker budget by default"
        );

        with_certification_threads(4, || {
            assert_eq!(scoped_certification_threads(), Some(4));
            with_certification_threads(1, || {
                assert_eq!(scoped_certification_threads(), Some(1));
            });
            assert_eq!(
                scoped_certification_threads(),
                Some(4),
                "a finished nested scope restores its parent budget"
            );

            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                with_certification_threads(2, || panic!("nested budget panic"));
            }));
            assert!(panicked.is_err());
            assert_eq!(
                scoped_certification_threads(),
                Some(4),
                "a panicking nested scope restores its parent budget"
            );

            // The budget belongs to one compile on one thread; a worker must not
            // inherit it and reopen the full worker count underneath.
            std::thread::scope(|scope| {
                scope.spawn(|| assert_eq!(scoped_certification_threads(), None));
            });
        });

        assert_eq!(
            scoped_certification_threads(),
            None,
            "the outermost scope clears the budget on return"
        );

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_certification_threads(3, || panic!("outer budget panic"));
        }));
        assert!(panicked.is_err());
        assert_eq!(
            scoped_certification_threads(),
            None,
            "the outermost scope clears the budget on panic"
        );
    }

    #[test]
    fn certification_sweeps_serialize_and_recover_after_poison() {
        let held = certification_sweep_guard();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                let _guard = certification_sweep_guard();
                acquired_tx.send(()).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(acquired_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err());
            drop(held);
            // Blocking, not deadlined: the sweep lock is admission control with
            // an unbounded hold time and no fairness, so a concurrent test's
            // sweep may legitimately win this handoff first. The contract under
            // test is that the waiter is admitted once the lock is free, not
            // that it is admitted within any wall-clock bound.
            acquired_rx.recv().unwrap();
        });

        let panic = std::panic::catch_unwind(|| {
            let _guard = certification_sweep_guard();
            panic!("poison the sweep lock");
        });
        assert!(panic.is_err());
        assert!(CERTIFICATION_SWEEP_LOCK.is_poisoned());
        drop(certification_sweep_guard());
        CERTIFICATION_SWEEP_LOCK.clear_poison();
    }

    fn not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        }
    }

    // One real NOR driven by `i0`, with `width - 1` declared but unused
    // inputs. The canonical state count grows with width while the realised
    // circuit stays the shape legacy routing is known to build, so a test can
    // reach worker counts above the threshold without a routing-shaped refusal.
    fn not_netlist_with_inputs(width: usize) -> Netlist {
        Netlist {
            inputs: (0..width).map(|index| format!("i{index}")).collect(),
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["i0"])],
        }
    }

    fn two_input_not_netlist() -> Netlist {
        not_netlist_with_inputs(2)
    }

    #[test]
    fn drive_vector_only_writes_and_dirties_changed_inputs() {
        let netlist = not_netlist_with_inputs(2);
        let mut placements = crate::compile::planner::PortPlacements::default();
        placements.pin(
            "i0",
            crate::compile::planner::Anchor { x: 10, y: 1, z: 40 },
            Facing::North,
        );
        let plan = crate::compile::planner::plan_from_netlist(&netlist, &placements)
            .expect("mixed pinned fixture must plan");
        let realised = crate::compile::planner::realise_and_verify(
            &plan,
            &netlist,
            crate::compile::planner::candidate_world_size(&plan),
        )
        .expect("mixed pinned fixture must realise");
        let candidate = LegacyCandidateAdapter::adapt_plan(&netlist, &plan, &realised.world)
            .expect("mixed pinned fixture must adapt")
            .candidate;
        let compatibility = candidate
            .compatibility_views(&netlist)
            .expect("fixture compatibility views");
        let mut simulator = fresh_simulator(&realised.world, &candidate, &netlist);
        let pinned = candidate.pins.get("i0").expect("i0 is pinned").at;
        let lever = compatibility.input_positions["i1"];
        let pinned_index = simulator
            .world()
            .index(pinned.x, pinned.y, pinned.z)
            .unwrap();
        let lever_index = simulator
            .world()
            .index(lever.0, lever.1, lever.2)
            .unwrap();

        assert_eq!(
            simulator.world().get(pinned.x, pinned.y, pinned.z).kind,
            BlockKind::Air
        );
        assert!(!simulator.world().get(lever.0, lever.1, lever.2).lit);
        simulator.world_mut().take_dirty();

        drive_vector(
            &mut simulator,
            &candidate,
            &netlist,
            &compatibility,
            &[false, false],
        )
        .expect("unchanged vector drives");
        assert!(
            simulator.world_mut().take_dirty().is_empty(),
            "an unchanged vector must not write either input"
        );

        drive_vector(
            &mut simulator,
            &candidate,
            &netlist,
            &compatibility,
            &[true, false],
        )
        .expect("pinned input changes");
        assert_eq!(
            simulator.world_mut().take_dirty(),
            vec![pinned_index]
        );
        assert_eq!(
            simulator.world().get(pinned.x, pinned.y, pinned.z).kind,
            BlockKind::RedstoneBlock
        );

        drive_vector(
            &mut simulator,
            &candidate,
            &netlist,
            &compatibility,
            &[true, true],
        )
        .expect("lever input changes");
        assert_eq!(simulator.world_mut().take_dirty(), vec![lever_index]);
        assert!(simulator.world().get(lever.0, lever.1, lever.2).lit);
    }

    #[test]
    fn complete_certification_seals_structure_function_manifest_and_metrics() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let fingerprint = candidate.fingerprint();
        let library = Library::default_library();
        let config = CertificationConfig::from_search(&SearchConfig::checked_defaults());

        let certified = CompleteCandidateCertifier
            .certify(candidate, &netlist, &library, &config)
            .expect("a valid NOT must receive complete certification");

        assert_eq!(certified.metrics().candidate_fingerprint, fingerprint);
        assert_eq!(certified.metrics().transition_count, 2);
        assert_eq!(certified.measurements().len(), 2);
        assert_eq!(
            certified
                .measurements()
                .iter()
                .map(|measurement| measurement.manifest_index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(!certified.metrics().worst_transition_indices.is_empty());
        assert_eq!(
            certified.metrics().equivalence_certificate_fingerprint,
            None,
            "small circuits use exhaustive functional certification"
        );
        assert_eq!(
            certified.timing_graph().fingerprint(),
            certified.metrics().realised_timing_graph_fingerprint
        );
    }

    // What this proves: every certificate and metric a run seals carries one
    // shared identity value, and the mutation guards still refuse a foreign
    // certificate. What it cannot prove: that the value was computed once. A
    // certificate exposes only the value, so cloning the seal and recomputing an
    // equal fingerprint are indistinguishable from here, and no counting hook
    // was added to production to make them distinguishable. "Fingerprint the
    // candidate once" is enforced by code review of the call sites and by the
    // REDA_PHASE_TIMING phase benchmarks instead.
    #[test]
    fn certification_identity_is_shared_by_every_certificate() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let library = Library::default_library();
        let config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        let identity = CertificationIdentity {
            candidate: candidate.fingerprint(),
            library_revision: library.revision_fingerprint(),
        };

        let certified = CompleteCandidateCertifier
            .certify_with_identity(candidate, &netlist, &library, &config, &identity)
            .expect("a valid NOT must receive complete certification");

        let structure = certified.world.structural_certificate();
        assert_eq!(structure.candidate_fingerprint, identity.candidate);
        assert_eq!(structure.library_revision, identity.library_revision);
        assert_eq!(
            certified.equivalence_certificate().candidate_fingerprint,
            identity.candidate
        );
        assert_eq!(
            certified.equivalence_certificate().library_revision,
            identity.library_revision
        );
        assert_eq!(certified.metrics().candidate_fingerprint, identity.candidate);
        assert_eq!(
            certified.metrics().realised_timing_graph_fingerprint,
            certified.timing_graph().fingerprint()
        );
        assert_eq!(
            &RealisedTimingGraph::derive(certified.candidate(), structure)
                .expect("the sealed identity must derive its own timing graph"),
            certified.timing_graph(),
            "the sealed timing graph must be the graph this identity derives"
        );

        // The identity is a seal, not a bypass: a certificate carrying another
        // real candidate's identity must still be refused.
        let other = two_input_not_netlist();
        let other_compiled = compile_legacy(&other).expect("legacy migration fixture");
        let other_candidate = LegacyCandidateAdapter::adapt(&other, &other_compiled)
            .expect("typed migration fixture")
            .candidate;
        let other_world = realise_and_verify_expanded(&other_candidate, &other, &library)
            .expect("fixture must realise");
        let other_structure = other_world.structural_certificate();
        assert!(matches!(
            RealisedTimingGraph::derive(certified.candidate(), other_structure),
            Err(TimingGraphError::CertificateMismatch { .. })
        ));
    }

    #[test]
    fn parallel_manifest_sweep_matches_serial_results() {
        let netlist = not_netlist_with_inputs(5);
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let library = Library::default_library();
        let world = realise_and_verify_expanded(&candidate, &netlist, &library)
            .expect("fixture must realise");
        let compatibility = candidate
            .compatibility_views(&netlist)
            .expect("fixture compatibility views");
        let mut config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        let manifest =
            TransitionManifest::for_kind(netlist.inputs.clone(), config.transition_manifest_kind);
        let sweep = |config: &CertificationConfig, threads| {
            sweep_manifest_with_threads(
                world.world(),
                &candidate,
                &netlist,
                &compatibility,
                &manifest,
                config,
                threads,
            )
        };

        let serial = sweep(&config, 1).expect("serial sweep");
        let parallel = sweep(&config, 4).expect("parallel sweep");
        let fresh = manifest
            .transitions()
            .iter()
            .enumerate()
            .map(|(manifest_index, transition)| {
                measure_transition_fresh(
                    world.world(),
                    &candidate,
                    &netlist,
                    &compatibility,
                    transition,
                    manifest_index,
                    &config,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .expect("fresh-per-transition reference sweep");
        assert_eq!(
            fresh[0],
            TransitionMeasurement {
                manifest_index: 0,
                start_tick: 7,
                settle_game_ticks: 10,
                simulator_events: 11,
            },
            "the independent fresh path pins the legacy measurement fields"
        );

        assert_eq!(parallel, serial);
        assert_eq!(serial, fresh);
        assert_eq!(
            parallel
                .iter()
                .map(|measurement| measurement.manifest_index)
                .collect::<Vec<_>>(),
            (0..manifest.transitions().len()).collect::<Vec<_>>()
        );

        config.max_simulator_events_per_transition = 0;
        let fresh = measure_transition_fresh(
            world.world(),
            &candidate,
            &netlist,
            &compatibility,
            &manifest.transitions()[0],
            0,
            &config,
        )
        .expect_err("fresh reference must hit the cap");
        assert!(matches!(
            &fresh,
            CandidateCertificationError::SimulatorEventCapExceeded {
                manifest_index: 0,
                used: 0,
                limit: 0,
            }
        ));
        let serial = sweep(&config, 1).expect_err("serial sweep must hit the cap");
        let parallel = sweep(&config, 4).expect_err("parallel sweep must hit the cap");

        let fields = |error| match error {
            CandidateCertificationError::SimulatorEventCapExceeded {
                manifest_index,
                used,
                limit,
            } => (manifest_index, used, limit),
            other => panic!("unexpected sweep error: {other}"),
        };
        let serial_fields = fields(serial);
        let parallel_fields = fields(parallel);
        let fresh_fields = fields(fresh);
        assert_eq!(parallel_fields, serial_fields);
        assert_eq!(serial_fields, fresh_fields);
        assert_eq!(serial_fields.0, 0);

        let empty = TransitionManifest::new(Vec::new());
        assert!(sweep_manifest_with_threads(
            world.world(),
            &candidate,
            &netlist,
            &compatibility,
            &empty,
            &config,
            4,
        )
        .expect("empty sweep")
        .is_empty());
    }

    #[test]
    fn manifest_source_groups_preserve_every_transition_and_index() {
        let manifest = TransitionManifest::new((0..5).map(|index| format!("i{index}")).collect());
        let groups = transition_source_groups(manifest.transitions());

        assert!(groups.len() < manifest.transitions().len());
        let mut next_index = 0;
        for (start, group) in groups {
            assert_eq!(start, next_index);
            assert!(!group.is_empty());
            assert!(group.iter().all(|transition| transition.from == group[0].from));
            assert_eq!(
                group,
                &manifest.transitions()[start..start + group.len()],
                "grouping must neither reorder nor drop transitions"
            );
            next_index += group.len();
        }
        assert_eq!(next_index, manifest.transitions().len());
    }

    #[test]
    fn sparse_manifest_batches_balance_by_transition_count() {
        let manifest = TransitionManifest::new((0..8).map(|index| format!("i{index}")).collect());
        let groups = transition_source_groups(manifest.transitions());
        let batches = transition_group_batches(&groups, 4);

        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.iter().map(|(_, group)| group.len()).sum::<usize>())
                .collect::<Vec<_>>(),
            vec![8, 8, 8, 8]
        );
        assert_eq!(
            batches
                .into_iter()
                .flat_map(|batch| batch.iter().copied())
                .collect::<Vec<_>>(),
            groups
        );
    }

    #[test]
    fn certified_candidate_is_identical_at_one_two_and_four_workers() {
        let netlist = not_netlist_with_inputs(5);
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let library = Library::default_library();
        let config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        let certify = |workers| {
            with_certification_threads(workers, || {
                CompleteCandidateCertifier
                    .certify(candidate.clone(), &netlist, &library, &config)
                    .expect("the fixture must certify at every worker count")
            })
        };

        let serial = certify(1);
        for workers in [2, 4] {
            let parallel = certify(workers);
            assert_eq!(
                parallel.candidate().fingerprint(),
                serial.candidate().fingerprint(),
                "{workers} workers must certify the same candidate"
            );
            assert_eq!(
                canonical_world_fingerprint(parallel.world()),
                canonical_world_fingerprint(serial.world()),
                "{workers} workers must emit the same world"
            );
            assert_eq!(
                parallel.equivalence_certificate(),
                serial.equivalence_certificate(),
                "{workers} workers must seal the same equivalence certificate"
            );
            assert_eq!(
                parallel.timing_graph(),
                serial.timing_graph(),
                "{workers} workers must derive the same timing graph"
            );
            assert_eq!(
                parallel.manifest(),
                serial.manifest(),
                "{workers} workers must build the same manifest"
            );
            assert_eq!(
                parallel.manifest().fingerprint(),
                serial.manifest().fingerprint(),
                "{workers} workers must seal the same manifest fingerprint"
            );
            assert_eq!(
                parallel.measurements(),
                serial.measurements(),
                "{workers} workers must measure every transition identically"
            );
            assert_eq!(
                parallel.metrics(),
                serial.metrics(),
                "{workers} workers must report the same metrics"
            );
        }
    }

    #[test]
    fn exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count() {
        let netlist = not_netlist_with_inputs(5);
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let library = Library::default_library();
        let world = realise_and_verify_expanded(&candidate, &netlist, &library)
            .expect("fixture must realise");
        let compatibility = candidate
            .compatibility_views(&netlist)
            .expect("fixture compatibility views");
        let mut config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        let exhaustive = |config: &CertificationConfig, threads| {
            certify_exhaustive_truth_with_threads(
                world.world(),
                &candidate,
                &netlist,
                &compatibility,
                config,
                threads,
            )
        };

        for threads in [1, 2, 4] {
            assert_eq!(
                exhaustive(&config, threads).expect("every canonical vector must certify"),
                1 << netlist.inputs.len(),
                "{threads} workers must certify the whole canonical mask range"
            );
        }

        // Every mask refuses under a zero event cap, so the reported mask is a
        // completion-order detector: only ordered reduction keeps reporting the
        // lowest one.
        config.max_simulator_events_per_transition = 0;
        let fields = |error| match error {
            CandidateCertificationError::SimulatorEventCapExceeded {
                manifest_index,
                used,
                limit,
            } => (manifest_index, used, limit),
            other => panic!("unexpected exhaustive error: {other}"),
        };
        let serial = fields(exhaustive(&config, 1).expect_err("serial must hit the event cap"));
        assert_eq!(serial.0, 0, "mask 0 is the lowest failing vector");
        for threads in [2, 4] {
            assert_eq!(
                fields(exhaustive(&config, threads).expect_err("parallel must hit the event cap")),
                serial,
                "{threads} workers must report the same lowest failing mask"
            );
        }
    }

    #[test]
    fn exhaustive_functional_mismatch_names_the_same_output_at_every_worker_count() {
        // The world really realises `y = NOR(i0)`, so it computes NOT i0.
        let netlist = not_netlist_with_inputs(5);
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let library = Library::default_library();
        let world = realise_and_verify_expanded(&candidate, &netlist, &library)
            .expect("fixture must realise");
        let compatibility = candidate
            .compatibility_views(&netlist)
            .expect("fixture compatibility views");
        let config = CertificationConfig::from_search(&SearchConfig::checked_defaults());

        // Same inputs and same output, but the spec is BUF built from two NORs,
        // so it computes i0. Every vector therefore disagrees with the world and
        // the refusal comes from the real `check_outputs`, not a stub.
        let buffered = Netlist {
            inputs: netlist.inputs.clone(),
            outputs: netlist.outputs.clone(),
            gates: vec![Gate::nor("n0", &["i0"]), Gate::nor("y", &["n0"])],
        };

        // By hand: `bits_of` puts `i0` in the mask's high bit, so mask 0 drives
        // every input low. The spec gives `y = i0 = false`; the world gives
        // `y = NOT i0 = true`. `check_outputs` receives the mask as its
        // `manifest_index`, so the lowest failing vector is index 0. Masks with
        // `i0` high disagree the other way round, so every one of the 32 vectors
        // fails and the reported mask is a completion-order detector: only the
        // ordered reduction keeps reporting mask 0.
        let state_count = 1 << netlist.inputs.len();
        for threads in [1, 2, 4] {
            // Without this the coverage above could go silently serial: raising
            // the exhaustive threshold would collapse 2 and 4 workers to 1 and
            // the ordering assertion would still pass, proving nothing.
            assert_eq!(
                certification_workers(state_count, threads, MIN_VECTORS_PER_CERTIFICATION_WORKER),
                threads,
                "{threads} requested workers must actually open over {state_count} vectors"
            );
            let error = certify_exhaustive_truth_with_threads(
                world.world(),
                &candidate,
                &buffered,
                &compatibility,
                &config,
                threads,
            )
            .expect_err("a BUF spec must not certify against a NOT world");
            assert!(
                matches!(
                    error,
                    CandidateCertificationError::FunctionalMismatch {
                        manifest_index: 0,
                        ref output,
                        expected: false,
                        actual: true,
                    } if output == "y"
                ),
                "{threads} workers must refuse with the lowest failing vector: {error:?}"
            );
        }
    }

    #[test]
    fn manifest_cap_refuses_instead_of_returning_a_partial_score() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let mut config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        config.max_certification_transitions = 1;

        assert!(matches!(
            CompleteCandidateCertifier.certify(
                candidate,
                &netlist,
                &Library::default_library(),
                &config
            ),
            Err(CandidateCertificationError::TransitionCapExceeded { count: 2, limit: 1 })
        ));
    }

    #[test]
    fn simulator_event_cap_refuses_instead_of_returning_a_capped_score() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("legacy migration fixture");
        let candidate = LegacyCandidateAdapter::adapt(&netlist, &compiled)
            .expect("typed migration fixture")
            .candidate;
        let mut config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        config.max_simulator_events_per_transition = 0;

        assert!(matches!(
            CompleteCandidateCertifier.certify(
                candidate,
                &netlist,
                &Library::default_library(),
                &config
            ),
            Err(CandidateCertificationError::SimulatorEventCapExceeded {
                used: 0,
                limit: 0,
                ..
            })
        ));
    }

    fn solid() -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::Solid;
        state.name = "minecraft:stone".into();
        state
    }

    fn wall_torch(facing: Facing, lit: bool) -> BlockState {
        let mut state = BlockState::air();
        state.kind = BlockKind::WallTorch;
        state.name = "minecraft:redstone_wall_torch".into();
        state.facing = Some(facing);
        state.lit = lit;
        state
    }

    #[test]
    fn divergence_is_a_named_transition_refusal() {
        let mut world = World::new(5, 5, 5);
        for support in [
            Position::new(0, 0, 0),
            Position::new(1, 1, 0),
            Position::new(0, 1, 1),
        ] {
            world.set(support.x, support.y, support.z, solid());
        }
        world.set(1, 0, 0, wall_torch(Facing::East, true));
        world.set(1, 1, 1, wall_torch(Facing::South, false));
        world.set(0, 0, 1, wall_torch(Facing::Down, true));
        let mut simulator = Simulator::new(world);
        let mut config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        config.max_game_ticks_per_transition = 8;

        assert!(matches!(
            settle(&mut simulator, 7, TransitionPhase::Destination, 0, &config),
            Err(CandidateCertificationError::TransitionDidNotSettle {
                manifest_index: 7,
                phase: TransitionPhase::Destination,
                simulation_error: SimulationError::Diverged { .. }
            })
        ));
    }

    #[test]
    fn candidate_fingerprint_breaks_ties_but_is_not_an_improvement() {
        let quality = super::QualityKey {
            observed_settle: 4,
            non_air_blocks: 20,
            occupied_volume: 40,
            static_routed_delay: crate::compile::fragment_synth::timing_graph::ExactDelay(4),
        };
        let metrics = |fingerprint| super::CandidateMetrics {
            quality,
            transition_manifest_hash: canonical_fingerprint(b"manifest"),
            transition_count: 2,
            transition_cap: 10,
            worst_transition_indices: vec![0, 1],
            equivalence_certificate_fingerprint: None,
            realised_timing_graph_fingerprint: canonical_fingerprint(b"timing"),
            candidate_fingerprint: canonical_fingerprint(fingerprint),
            emitted_world_fingerprint: canonical_fingerprint(b"world"),
        };
        let first = metrics(b"a");
        let second = metrics(b"b");

        assert_ne!(
            first.stable_selection_order(&second),
            std::cmp::Ordering::Equal
        );
        assert!(!first.is_strict_improvement_over(&second));
        assert!(!second.is_strict_improvement_over(&first));
    }
}
