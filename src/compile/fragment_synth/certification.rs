use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::compile::equivalence::{
    prove_combinational_equivalence, EquivalenceCertificate, EquivalenceError,
};
use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
use crate::compile::fragment_synth::candidate::{
    CandidateError, CompatibilityViews, ExpandedPhysicalCandidate,
};
use crate::compile::fragment_synth::config::CertificationConfig;
use crate::compile::fragment_synth::manifest::{Transition, TransitionManifest};
use crate::compile::fragment_synth::realise::{
    realise_and_verify_expanded, CertificationError as PhysicalCertificationError, CertifiedWorld,
};
use crate::compile::fragment_synth::timing_graph::{
    ExactDelay, RealisedTimingGraph, TimingGraphError,
};
use crate::compile::metrics::{physical_metrics, Fingerprint};
use crate::compile::topology::Library;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::block_signal_at;
use crate::redstone::simulator::{BoundedSimulationError, SimulationError, Simulator};
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

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

pub struct ExpandedCandidateCertifier;

impl ExpandedCandidateCertifier {
    pub fn certify(
        candidate: ExpandedPhysicalCandidate,
        lowered: &Netlist,
        library: &Library,
        config: &CertificationConfig,
    ) -> Result<CertifiedCandidate, CandidateCertificationError> {
        let world = realise_and_verify_expanded(&candidate, lowered, library)?;
        let timing_graph = RealisedTimingGraph::derive(&candidate, world.structural_certificate())?;
        let static_timing = timing_graph.analyse()?;
        let equivalence = prove_combinational_equivalence(
            lowered,
            &candidate,
            library,
            config.max_equivalence_proof_steps,
        )?;
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

        if lowered.inputs.len() <= usize::from(config.exhaustive_input_threshold) {
            certify_exhaustive_truth(world.world(), &candidate, lowered, &compatibility, config)?;
        }

        let measurements = sweep_manifest(
            world.world(),
            &candidate,
            lowered,
            &compatibility,
            &manifest,
            config,
        )?;
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
            candidate_fingerprint: candidate.fingerprint(),
            emitted_world_fingerprint: canonical_world_fingerprint(world.world()),
        };
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
) -> Result<(), CandidateCertificationError> {
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
    Ok(())
}

fn sweep_manifest(
    world: &World,
    candidate: &ExpandedPhysicalCandidate,
    lowered: &Netlist,
    compatibility: &CompatibilityViews,
    manifest: &TransitionManifest,
    config: &CertificationConfig,
) -> Result<Vec<TransitionMeasurement>, CandidateCertificationError> {
    manifest
        .transitions()
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
        .collect()
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
        external_signal_is_high, settle, CandidateCertificationError, ExpandedCandidateCertifier,
        TransitionPhase,
    };
    use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
    use crate::compile::fragment_synth::legacy_adapter::LegacyCandidateAdapter;
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

    fn not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
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

        let certified = ExpandedCandidateCertifier::certify(candidate, &netlist, &library, &config)
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
            ExpandedCandidateCertifier::certify(
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
            ExpandedCandidateCertifier::certify(
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
