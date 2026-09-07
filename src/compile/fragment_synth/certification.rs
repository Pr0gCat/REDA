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

const MAX_MANIFEST_SWEEP_THREADS: usize = 12;
static MANIFEST_SWEEP_LOCK: Mutex<()> = Mutex::new(());

type ManifestChunkOutcome =
    std::thread::Result<Result<Vec<TransitionMeasurement>, CandidateCertificationError>>;

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
    let state_count = 1usize
        .checked_shl(u32::try_from(lowered.inputs.len()).unwrap_or(u32::MAX))
        .ok_or(CandidateCertificationError::CounterOverflow)?;
    for mask in 0..state_count {
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
        )?;
    }
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
    let available = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let requested = std::env::var("REDA_CERT_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let threads = manifest_sweep_threads(available, requested);
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

fn manifest_sweep_threads(available: usize, requested: Option<usize>) -> usize {
    requested
        .unwrap_or(available)
        .clamp(1, available.clamp(1, MAX_MANIFEST_SWEEP_THREADS))
}

fn manifest_sweep_guard() -> std::sync::MutexGuard<'static, ()> {
    MANIFEST_SWEEP_LOCK
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
    let transitions = manifest.transitions();
    let worker_count = threads.max(1).min(transitions.len().max(1));
    if worker_count == 1 {
        return transitions
            .iter()
            .enumerate()
            .map(|(manifest_index, transition)| {
                measure_transition(
                    world,
                    candidate,
                    lowered,
                    compatibility,
                    transition,
                    manifest_index,
                    config,
                )
            })
            .collect();
    }

    // ponytail: one sweep owns the worker budget; use a shared pool only if
    // concurrent compile throughput becomes more important than one compile's latency.
    let _sweep = manifest_sweep_guard();
    let chunk_len = transitions.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        let handles = transitions
            .chunks(chunk_len)
            .enumerate()
            .map(|(chunk_index, chunk)| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(index, transition)| {
                            measure_transition(
                                world,
                                candidate,
                                lowered,
                                compatibility,
                                transition,
                                chunk_index * chunk_len + index,
                                config,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect::<Vec<_>>();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join())
            .collect::<Vec<_>>();
        collect_manifest_chunks(outcomes, transitions.len())
    })
}

fn collect_manifest_chunks(
    outcomes: impl IntoIterator<Item = ManifestChunkOutcome>,
    capacity: usize,
) -> Result<Vec<TransitionMeasurement>, CandidateCertificationError> {
    let mut measurements = Vec::with_capacity(capacity);
    for outcome in outcomes {
        match outcome {
            Ok(chunk) => measurements.extend(chunk?),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }
    Ok(measurements)
}

fn measure_transition(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    transition: &Transition,
    manifest_index: usize,
    config: &CertificationConfig,
) -> Result<TransitionMeasurement, CandidateCertificationError> {
    let mut simulator = fresh_simulator(world, candidate, lowered);
    let events_before = simulator.work_done();
    drive_vector(
        &mut simulator,
        candidate,
        lowered,
        compatibility,
        &transition.from,
    )?;
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
        &transition.from,
        manifest_index,
    )?;
    simulator.attach_typed_observer(
        candidate
            .observations
            .values()
            .map(|observation| observation.site.clone()),
    );
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
            compile::drive_caller_cell(simulator.world_mut(), (pin.at.x, pin.at.y, pin.at.z), bit);
        } else {
            let mut state = simulator
                .world()
                .get(position.0, position.1, position.2)
                .clone();
            state.lit = bit;
            simulator
                .world_mut()
                .set(position.0, position.1, position.2, state);
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
        collect_manifest_chunks, external_signal_is_high, manifest_sweep_guard,
        manifest_sweep_threads, settle, sweep_manifest_with_threads, CandidateCertificationError,
        CompleteCandidateCertifier, ExpandedCandidateCertifier, RealisedTimingGraph,
        TimingGraphError, TransitionManifest, TransitionPhase, MANIFEST_SWEEP_LOCK,
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

    #[test]
    fn manifest_worker_count_is_bounded_and_tunable() {
        assert_eq!(manifest_sweep_threads(8, None), 8);
        assert_eq!(manifest_sweep_threads(8, Some(4)), 4);
        assert_eq!(manifest_sweep_threads(8, Some(0)), 1);
        assert_eq!(manifest_sweep_threads(32, Some(32)), 12);
    }

    #[test]
    fn earlier_manifest_error_wins_over_a_later_worker_panic() {
        let outcomes = vec![
            Ok(Err(CandidateCertificationError::CounterOverflow)),
            Err(Box::new("later panic") as Box<dyn std::any::Any + Send>),
        ];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            collect_manifest_chunks(outcomes, 0)
        }));

        assert!(matches!(
            result,
            Ok(Err(CandidateCertificationError::CounterOverflow))
        ));

        let outcomes = vec![
            Err(Box::new("first panic") as Box<dyn std::any::Any + Send>),
            Ok(Err(CandidateCertificationError::CounterOverflow)),
            Err(Box::new("later panic") as Box<dyn std::any::Any + Send>),
        ];
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            collect_manifest_chunks(outcomes, 0)
        }))
        .expect_err("the earliest panic must be resumed");
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"first panic"));
    }

    #[test]
    fn manifest_sweeps_serialize_and_recover_after_poison() {
        let held = manifest_sweep_guard();
        std::thread::scope(|scope| {
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
            scope.spawn(move || {
                started_tx.send(()).unwrap();
                let _guard = manifest_sweep_guard();
                acquired_tx.send(()).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(acquired_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err());
            drop(held);
            acquired_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap();
        });

        let panic = std::panic::catch_unwind(|| {
            let _guard = manifest_sweep_guard();
            panic!("poison the sweep lock");
        });
        assert!(panic.is_err());
        assert!(MANIFEST_SWEEP_LOCK.is_poisoned());
        drop(manifest_sweep_guard());
        MANIFEST_SWEEP_LOCK.clear_poison();
    }

    fn not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        }
    }

    fn two_input_not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        }
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
        let netlist = two_input_not_netlist();
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

        assert_eq!(parallel, serial);
        assert_eq!(
            parallel
                .iter()
                .map(|measurement| measurement.manifest_index)
                .collect::<Vec<_>>(),
            (0..manifest.transitions().len()).collect::<Vec<_>>()
        );

        config.max_simulator_events_per_transition = 0;
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
        assert_eq!(parallel_fields, serial_fields);
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
