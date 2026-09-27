use std::any::Any;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Mutex;

use serde::Serialize;
use thiserror::Error;

use crate::compile::equivalence::{
    prove_combinational_equivalence, EquivalenceCertificate, EquivalenceError,
};
use crate::compile::fragment_synth::allocation::RootPort;
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
use crate::compile::metrics::{physical_metrics, Fingerprint, PhysicalMetrics};
use crate::compile::planner::PortRole;
use crate::compile::topology::Library;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::component::torch_support_position;
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

#[cfg(test)]
mod driver_tests {
    use super::{run_indexed, CertificationWorkers};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Enough workers to actually overlap, or none if the machine has one core.
    fn wide() -> CertificationWorkers {
        CertificationWorkers::bounded(4)
    }

    #[test]
    fn results_come_back_in_index_order_whatever_ran_them() {
        let serial: Result<Vec<usize>, ()> =
            run_indexed(CertificationWorkers::serial(), 64, |index| Ok(index * 7));
        let parallel: Result<Vec<usize>, ()> = run_indexed(wide(), 64, |index| Ok(index * 7));
        let expected = (0..64).map(|index| index * 7).collect::<Vec<_>>();
        assert_eq!(serial.unwrap(), expected);
        assert_eq!(parallel.unwrap(), expected);
    }

    /// Run `body` with panic reporting off: these tests panic on purpose and
    /// the hook would otherwise print a backtrace for each one.
    ///
    /// The hook is process-global, so it goes back on as soon as `body`
    /// returns *or* unwinds -- a guard, not a pair of statements -- and `body`
    /// should hold nothing but the call that panics. Assertions belong outside
    /// it, where a failure still prints.
    fn without_panic_output<T>(body: impl FnOnce() -> T) -> T {
        type Hook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

        struct Restore(Option<Hook>);

        impl Drop for Restore {
            fn drop(&mut self) {
                if let Some(hook) = self.0.take() {
                    std::panic::set_hook(hook);
                }
            }
        }

        let _restore = Restore(Some(std::panic::take_hook()));
        std::panic::set_hook(Box::new(|_| {}));
        body()
    }

    #[test]
    fn the_lowest_index_error_is_the_one_returned() {
        // Three failures and a panic above them: a serial run would have
        // stopped at 9, so that is the answer at any worker count.
        let run = |index: usize| -> Result<usize, usize> {
            match index {
                9 | 20 | 31 => Err(index),
                40 => panic!("a job above the first failure must not decide the outcome"),
                _ => Ok(index),
            }
        };
        // Only the runs are silenced; the assertions are not.
        let serial = without_panic_output(|| run_indexed(CertificationWorkers::serial(), 64, run));
        let parallel = without_panic_output(|| run_indexed(wide(), 64, run));
        assert_eq!(serial, Err(9));
        assert_eq!(parallel, Err(9));
    }

    #[test]
    fn a_slow_low_failure_still_beats_a_fast_high_one() {
        // The low job is claimed first and takes its time; the high one fails
        // immediately and publishes first. A serial run would have returned
        // the low one, so this must too.
        let run = |index: usize| -> Result<usize, usize> {
            match index {
                0 => {
                    std::thread::sleep(Duration::from_millis(50));
                    Err(0)
                }
                1 => Err(1),
                _ => Ok(index),
            }
        };
        for _ in 0..8 {
            assert_eq!(run_indexed(wide(), 64, run), Err(0));
        }
    }

    #[test]
    fn jobs_far_above_the_first_failure_are_never_run() {
        let ran = AtomicUsize::new(0);
        let highest = AtomicUsize::new(0);
        let jobs = 4096;
        let done: Result<Vec<usize>, usize> = run_indexed(wide(), jobs, |index| {
            ran.fetch_add(1, Ordering::AcqRel);
            highest.fetch_max(index, Ordering::AcqRel);
            if index == 0 {
                return Err(0);
            }
            Ok(index)
        });
        assert_eq!(done, Err(0));
        // A handful of indices are already claimed when index 0 fails; the
        // thousands above them are not, and must never be touched.
        let ran = ran.load(Ordering::Acquire);
        assert!(
            ran < jobs / 2,
            "{ran} of {jobs} jobs ran after the first index failed"
        );
        assert!(
            highest.load(Ordering::Acquire) < jobs / 2,
            "a job far above the failure was claimed"
        );
    }

    #[test]
    fn the_lowest_index_panic_is_the_one_that_propagates() {
        let caught = without_panic_output(|| {
            std::panic::catch_unwind(|| {
                run_indexed(wide(), 32, |index| -> Result<usize, usize> {
                    match index {
                        12 => panic!("first"),
                        25 => panic!("second"),
                        _ => Ok(index),
                    }
                })
            })
        });
        let payload = caught.expect_err("the panic must propagate");
        assert_eq!(
            payload.downcast_ref::<&str>().copied(),
            Some("first"),
            "the lowest-index panic is the one a serial run would have hit"
        );
    }

    #[test]
    fn no_jobs_spawns_nothing_and_runs_nothing() {
        let ran = AtomicUsize::new(0);
        let done: Result<Vec<usize>, ()> = run_indexed(wide(), 0, |index| {
            ran.fetch_add(1, Ordering::Relaxed);
            Ok(index)
        });
        assert_eq!(done.unwrap(), Vec::<usize>::new());
        assert_eq!(ran.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn workers_are_bounded_by_the_machine_and_really_do_overlap() {
        let available = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        assert_eq!(CertificationWorkers::serial().count(), 1);
        assert!(CertificationWorkers::bounded(0).count() >= 1);
        assert!(CertificationWorkers::bounded(1024).count() <= available);

        let workers = wide();
        let in_flight = AtomicUsize::new(0);
        let widest = AtomicUsize::new(0);
        // Each job waits for company, but only briefly: on a machine that
        // cannot overlap this returns after the deadline instead of spinning.
        let deadline = Duration::from_millis(200);
        let done: Result<Vec<usize>, ()> = run_indexed(workers, 16, |index| {
            let now = in_flight.fetch_add(1, Ordering::AcqRel) + 1;
            widest.fetch_max(now, Ordering::AcqRel);
            let until = Instant::now() + deadline;
            while in_flight.load(Ordering::Acquire) < workers.count() && Instant::now() < until {
                std::thread::yield_now();
            }
            in_flight.fetch_sub(1, Ordering::AcqRel);
            Ok(index)
        });
        assert_eq!(done.unwrap().len(), 16);
        if workers.count() > 1 {
            assert!(
                widest.load(Ordering::Acquire) > 1,
                "asked for {} workers and never saw two jobs at once",
                workers.count()
            );
        }
    }
}

#[cfg(test)]
mod root_world_tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::{allocate, AllocationLimits, AllocationPlan};
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::parent::{compose, ComposedCircuit};
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::fragment_synth::schedule::synthesise_children;
    use crate::compile::routing::DurablePhysicalRouter;
    use crate::compile::Gate;
    use crate::redstone::world::block::BlockState;

    fn built(netlist: &Netlist) -> (AllocationPlan, ComposedCircuit, CertificationConfig) {
        let chunks = partition(netlist, &root_chunk_id(netlist).unwrap(), 1).unwrap();
        let plan = allocate(
            netlist,
            &chunks,
            AllocationLimits {
                delay_budget_ticks: 4,
                corridor_capacity: 8,
            },
        )
        .unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 2).unwrap();
        let search = SearchConfig::checked_defaults();
        let circuit = compose(
            &plan,
            &artifacts,
            &DurablePhysicalRouter,
            search.router_limits,
        )
        .unwrap();
        let config = CertificationConfig::from_search(&search);
        (plan, circuit, config)
    }

    #[test]
    fn root_world_certifies_distinct_vectors_and_refuses_corruption() {
        let netlist = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["nx".into(), "ny".into()],
            gates: vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        };
        let (plan, mut circuit, config) = built(&netlist);
        let certificate = certify_root_world(
            &circuit.world,
            &netlist,
            &plan.root_ports,
            &config,
            CertificationWorkers::serial(),
        )
        .unwrap();
        assert!(!certificate.measurements.is_empty());
        assert!(certificate.physical.non_air_blocks > 0);

        let output = plan
            .root_ports
            .iter()
            .find(|port| port.role == PortRole::Output)
            .unwrap();
        let at = output.pin.handover(PortRole::Output);
        circuit.world.set(at.x, at.y, at.z, BlockState::air());
        assert!(matches!(
            certify_root_world(
                &circuit.world,
                &netlist,
                &plan.root_ports,
                &config,
                CertificationWorkers::serial()
            ),
            Err(CandidateCertificationError::FunctionalMismatch { .. })
        ));
    }

    /// The path the warm base replaced: probes injected and the initial dust
    /// settled inside every job, as certification used to do it.
    ///
    /// Test-only, and deliberately written from the same pieces production
    /// uses, so what it proves is that building the base once is the only
    /// difference between them.
    /// Exactly the setup certification did per job before the warm base: one
    /// clone of the composed world, probes injected into it, one load.
    fn cold_root_simulator(world: &World, plan: &AllocationPlan) -> Simulator {
        Simulator::new(probed(world, plan))
    }

    /// The composed world with output probes in it and nothing else done.
    fn probed(world: &World, plan: &AllocationPlan) -> World {
        let mut world = world.clone();
        for port in plan
            .root_ports
            .iter()
            .filter(|port| port.role == PortRole::Output)
        {
            compile::probe_caller_cell(&mut world, (port.pin.at.x, port.pin.at.y, port.pin.at.z));
        }
        world
    }

    fn certify_root_world_cold(
        world: &World,
        lowered: &Netlist,
        plan: &AllocationPlan,
        config: &CertificationConfig,
        workers: CertificationWorkers,
    ) -> Result<RootCertificate, CandidateCertificationError> {
        let manifest =
            TransitionManifest::for_kind(lowered.inputs.clone(), config.transition_manifest_kind);
        if lowered.inputs.len() <= usize::from(config.exhaustive_input_threshold) {
            let states = 1usize << lowered.inputs.len();
            run_indexed(workers, states, |mask| {
                let vector = bits_of(mask, lowered.inputs.len());
                // One base per job: the cold path.
                let mut simulator = cold_root_simulator(world, plan);
                drive_root_vector(&mut simulator, &plan.root_ports, &vector)?;
                settle(
                    &mut simulator,
                    mask,
                    TransitionPhase::ExhaustiveVector,
                    0,
                    config,
                )?;
                enforce_event_cap(&simulator, 0, mask, config)?;
                check_root_outputs(simulator.world(), lowered, &plan.root_ports, &vector, mask)
            })?;
        }
        let transitions = manifest.transitions();
        let measurements = run_indexed(workers, transitions.len(), |index| {
            // One probe injection and one load per job: the cold path.
            measure_root_transition(
                cold_root_simulator(world, plan),
                lowered,
                &plan.root_ports,
                &transitions[index],
                index,
                config,
            )
        })?;
        let worst_settle_game_ticks = measurements
            .iter()
            .map(|measurement| measurement.settle_game_ticks)
            .max()
            .unwrap_or(0);
        Ok(RootCertificate {
            world_fingerprint: canonical_world_fingerprint(world),
            manifest_fingerprint: manifest.fingerprint(),
            measurements,
            worst_settle_game_ticks,
            physical: physical_metrics(world, lowered.gates.len() as u64),
        })
    }

    #[test]
    fn dust_over_air_is_unsupported_and_dust_on_stone_is_not() {
        let mut world = World::new(3, 3, 1);
        world.set(1, 1, 0, compile::dust());
        assert_eq!(
            unsupported_component(&world),
            Some((Position::new(1, 1, 0), BlockKind::RedstoneWire))
        );
        world.set(1, 0, 0, compile::stone());
        assert_eq!(unsupported_component(&world), None);
    }

    #[test]
    fn a_warm_base_certifies_exactly_what_a_cold_one_did() {
        // Two inputs through two chunks each, so the sweep has four vectors,
        // the manifest has real transitions, and the root world carries four
        // trunks across the corridor rather than one.
        let netlist = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["p".into(), "q".into()],
            gates: vec![
                Gate::nor("a", &["x"]),
                Gate::nor("p", &["a"]),
                Gate::nor("b", &["y"]),
                Gate::nor("q", &["b"]),
            ],
        };
        let (plan, circuit, config) = built(&netlist);
        let cold = certify_root_world_cold(
            &circuit.world,
            &netlist,
            &plan,
            &config,
            CertificationWorkers::serial(),
        )
        .unwrap();
        let warm = certify_root_world(
            &circuit.world,
            &netlist,
            &plan.root_ports,
            &config,
            CertificationWorkers::serial(),
        )
        .unwrap();
        // Every measurement, tick, event count and fingerprint, not a summary.
        assert_eq!(cold, warm);
        assert!(cold.measurements.len() > 1, "the fixture must have jobs");
        assert!(
            cold.measurements
                .iter()
                .any(|measurement| measurement.simulator_events > 0),
            "the fixture must do simulator work worth comparing"
        );

        // And the warm path is still worker-invariant on the same fixture.
        let parallel = certify_root_world(
            &circuit.world,
            &netlist,
            &plan.root_ports,
            &config,
            CertificationWorkers::bounded(4),
        )
        .unwrap();
        assert_eq!(warm, parallel);
    }

    #[test]
    fn an_oversized_exhaustive_sweep_is_refused_before_a_slot_is_allocated() {
        let config = CertificationConfig::from_search(&SearchConfig::checked_defaults());
        // The checked defaults sweep, and the count is what they say it is.
        assert_eq!(
            exhaustive_vectors(usize::from(config.exhaustive_input_threshold), &config).unwrap(),
            256
        );
        assert_eq!(exhaustive_vectors(0, &config).unwrap(), 1);
        // A threshold the cap does not admit is a refusal, not an allocation:
        // 2^30 slots are never reserved to discover that.
        assert!(matches!(
            exhaustive_vectors(30, &config),
            Err(CandidateCertificationError::ExhaustiveVectorCapExceeded {
                inputs: 30,
                count: 1_073_741_824,
                limit: 65_536,
            })
        ));
        // And a shift wider than the counter is an overflow, not a wrap.
        assert!(matches!(
            exhaustive_vectors(64, &config),
            Err(CandidateCertificationError::CounterOverflow)
        ));
    }

    #[test]
    fn a_root_certificate_is_the_same_at_one_worker_and_many() {
        let netlist = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["nx".into(), "ny".into()],
            gates: vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        };
        let (plan, circuit, config) = built(&netlist);
        let serial = certify_root_world(
            &circuit.world,
            &netlist,
            &plan.root_ports,
            &config,
            CertificationWorkers::serial(),
        )
        .unwrap();
        let parallel = certify_root_world(
            &circuit.world,
            &netlist,
            &plan.root_ports,
            &config,
            CertificationWorkers::bounded(4),
        )
        .unwrap();
        // Whole certificate, not a summary of it: the measurements are in
        // manifest order and the worst settle is reduced from all of them.
        assert_eq!(serial, parallel);
        assert!(serial.measurements.len() > 1, "the fixture must have jobs");
    }

    #[test]
    fn malformed_root_port_contract_is_typed() {
        let netlist = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["x"])],
        };
        let (mut plan, circuit, config) = built(&netlist);
        plan.root_ports[0].signal = "wrong".into();
        assert!(matches!(
            certify_root_world(
                &circuit.world,
                &netlist,
                &plan.root_ports,
                &config,
                CertificationWorkers::serial()
            ),
            Err(CandidateCertificationError::RootPortContract {
                role: PortRole::Input,
                ..
            })
        ));
    }
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
    #[error(
        "exhaustive certification of {inputs} inputs needs {count} vectors, exceeding cap {limit}"
    )]
    ExhaustiveVectorCapExceeded {
        inputs: usize,
        count: u64,
        limit: u64,
    },
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
    #[error("{kind:?} at {at:?} has nothing to stand or hang on")]
    Unsupported { at: Position, kind: BlockKind },
    #[error("root {role:?} ports are {actual:?}, expected {expected:?}")]
    RootPortContract {
        role: PortRole,
        expected: Vec<String>,
        actual: Vec<String>,
    },
}

/// The first component, in `(y, z, x)` order, that Minecraft would drop for
/// want of a block to stand or hang on.
///
/// The simulator reads a world as given and never asks whether its dust or
/// diodes could stay placed, so a world that only works in the simulator
/// would otherwise certify. The bottom layer is exempt: it stands on whatever
/// the circuit is pasted onto.
pub(crate) fn unsupported_component(world: &World) -> Option<(Position, BlockKind)> {
    let (size_x, size_y, size_z) = world.size();
    for y in 1..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                let state = world.get(x, y, z);
                let at = Position::new(x, y, z);
                let below = world.flags_at(x, y - 1, z);
                let held = match state.kind {
                    BlockKind::RedstoneWire => below.can_carry_dust(),
                    BlockKind::Repeater | BlockKind::Comparator => below.can_carry_repeater(),
                    BlockKind::Torch => below.can_carry_torch(),
                    BlockKind::WallTorch => torch_support_position(state, at).is_some_and(
                        |wall| world.flags_at(wall.x, wall.y, wall.z).can_attach_wall_torch(),
                    ),
                    _ => true,
                };
                if !held {
                    return Some((at, state.kind));
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootCertificate {
    pub world_fingerprint: Fingerprint,
    pub manifest_fingerprint: Fingerprint,
    pub measurements: Vec<TransitionMeasurement>,
    pub worst_settle_game_ticks: u64,
    pub physical: PhysicalMetrics,
}

/// Authoritative whole-circuit functional certification after child
/// composition.  It shares the expanded-candidate certifier's logical
/// evaluator, transition policy, and bounded simulator authority.
/// How many threads one root certification may use.
///
/// Certification is a wall of independent simulations, so it parallelises, but
/// the result must not depend on how many threads ran it.  This is the only
/// knob: everything about ordering is fixed by [`run_indexed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CertificationWorkers(usize);

impl CertificationWorkers {
    /// One thread, and no thread spawned at all.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn serial() -> Self {
        Self(1)
    }

    /// `requested` threads, never fewer than one and never more than the
    /// machine has.
    pub(crate) fn bounded(requested: usize) -> Self {
        let available = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self(requested.max(1).min(available))
    }

    pub(crate) fn count(self) -> usize {
        self.0
    }
}

/// One job's fate: what it returned, or what it panicked with.
type JobOutcome<T, E> = Result<Result<T, E>, Box<dyn Any + Send + 'static>>;

/// Run `jobs` independent jobs and return their results in index order.
///
/// Workers claim indices from one counter, so a slow job never leaves a thread
/// idle, but every result is filed in the slot its index names and nothing is
/// read until every worker has joined.  The reduction then walks the slots in
/// ascending order, which is what makes the answer identical at any worker
/// count: the lowest-index error is the one returned and the lowest-index panic
/// is the one that propagates, exactly as a serial run would have reached them.
///
/// A job that fails or panics publishes its index as the lowest **terminal**
/// index known so far, and no worker claims an index above it after that: a
/// serial run would never have reached those, so running them is pure waste.
/// Indices at or below the terminal are all filed regardless -- the claim
/// counter only ever moves forward, so by the time index `n` is claimed every
/// index below it already has been, and a lower job that is still running when
/// a higher one fails is never abandoned.  That is what lets a slow low failure
/// still beat a fast high one.
pub(crate) fn run_indexed<T, E, F>(
    workers: CertificationWorkers,
    jobs: usize,
    run: F,
) -> Result<Vec<T>, E>
where
    T: Send,
    E: Send,
    F: Fn(usize) -> Result<T, E> + Sync,
{
    if jobs == 0 {
        return Ok(Vec::new());
    }
    let workers = workers.count().max(1).min(jobs);
    if workers == 1 {
        // The serial path spawns nothing and stops at the first failure.
        return (0..jobs).map(run).collect();
    }

    let slots: Vec<Mutex<Option<JobOutcome<T, E>>>> = (0..jobs).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let terminal = AtomicUsize::new(usize::MAX);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let slots = &slots;
            let next = &next;
            let terminal = &terminal;
            let run = &run;
            scope.spawn(move || loop {
                let index = next.fetch_add(1, AtomicOrdering::Relaxed);
                if index >= jobs || index > terminal.load(AtomicOrdering::Acquire) {
                    break;
                }
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(index)));
                if !matches!(outcome, Ok(Ok(_))) {
                    terminal.fetch_min(index, AtomicOrdering::AcqRel);
                }
                *slots[index].lock().expect("a job never panics holding it") = Some(outcome);
            });
        }
    });

    let mut done = Vec::with_capacity(jobs);
    for slot in slots {
        match slot.into_inner().expect("a job never panics holding it") {
            // Only indices above the terminal one are ever left unclaimed, and
            // the terminal index itself returns below before the walk gets
            // there.
            None => unreachable!("every index at or below the first failure files its outcome"),
            Some(Err(panic)) => std::panic::resume_unwind(panic),
            Some(Ok(Err(error))) => return Err(error),
            Some(Ok(Ok(value))) => done.push(value),
        }
    }
    Ok(done)
}

pub(crate) fn certify_root_world(
    world: &World,
    lowered: &Netlist,
    root_ports: &[RootPort],
    config: &CertificationConfig,
    workers: CertificationWorkers,
) -> Result<RootCertificate, CandidateCertificationError> {
    validate_root_ports(root_ports, PortRole::Input, &lowered.inputs)?;
    validate_root_ports(root_ports, PortRole::Output, &lowered.outputs)?;
    if let Some((at, kind)) = unsupported_component(world) {
        if std::env::var_os("REDA_TRACE_SUPPORT").is_some() {
            let (sx, sy, sz) = world.size();
            eprintln!("UNSUPPORTED {at:?} {kind:?} below={:?} size=({sx},{sy},{sz})", world.get(at.x, at.y - 1, at.z));
            for y in (0..sy).rev() {
                eprintln!("y={y}");
                for z in (at.z - 6).max(0)..(at.z + 7).min(sz) {
                    let row: String = ((at.x - 12).max(0)..(at.x + 13).min(sx)).map(|x| match world.get(x, y, z).kind {
                        BlockKind::Air => '.', BlockKind::Solid => '#', BlockKind::RedstoneWire => 'd',
                        BlockKind::Repeater => 'r', BlockKind::Torch => 't', BlockKind::WallTorch => 'w',
                        BlockKind::RedstoneBlock => 'B', BlockKind::Lamp => 'L', BlockKind::Lever => 'l', _ => '?' }).collect();
                    eprintln!("{row}");
                }
            }
        }
        return Err(CandidateCertificationError::Unsupported { at, kind });
    }

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

    // One probe injection and one initial dust settle for the whole
    // certification, rather than one per job.
    let base = warm_root_base(world, root_ports);

    // Two sequential phases, each parallel within itself: the exhaustive sweep
    // must have proven the function before any timing is measured on it.
    if lowered.inputs.len() <= usize::from(config.exhaustive_input_threshold) {
        let states = exhaustive_vectors(lowered.inputs.len(), config)?;
        run_indexed(workers, states, |mask| {
            let vector = bits_of(mask, lowered.inputs.len());
            let mut simulator = job_simulator(&base);
            drive_root_vector(&mut simulator, root_ports, &vector)?;
            settle(
                &mut simulator,
                mask,
                TransitionPhase::ExhaustiveVector,
                0,
                config,
            )?;
            enforce_event_cap(&simulator, 0, mask, config)?;
            check_root_outputs(simulator.world(), lowered, root_ports, &vector, mask)
        })?;
    }

    let transitions = manifest.transitions();
    let measurements = run_indexed(workers, transitions.len(), |index| {
        measure_root_transition(
            job_simulator(&base),
            lowered,
            root_ports,
            &transitions[index],
            index,
            config,
        )
    })?;
    let worst_settle_game_ticks = measurements
        .iter()
        .map(|measurement| measurement.settle_game_ticks)
        .max()
        .unwrap_or(0);
    Ok(RootCertificate {
        world_fingerprint: canonical_world_fingerprint(world),
        manifest_fingerprint: manifest.fingerprint(),
        measurements,
        worst_settle_game_ticks,
        physical: physical_metrics(world, lowered.gates.len() as u64),
    })
}

fn validate_root_ports(
    root_ports: &[RootPort],
    role: PortRole,
    expected: &[String],
) -> Result<(), CandidateCertificationError> {
    let actual = root_ports
        .iter()
        .filter(|port| port.role == role)
        .map(|port| port.signal.clone())
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(CandidateCertificationError::RootPortContract {
            role,
            expected: expected.to_vec(),
            actual,
        });
    }
    Ok(())
}

/// How many vectors the exhaustive sweep will run, or the typed refusal that
/// it is more than this certification may hold.
///
/// The sweep allocates a slot per vector, and its own threshold is a `u16`:
/// left unchecked, a threshold of 30 asks for a billion slots before a single
/// vector runs. It is certification work like any other, so it answers to the
/// same cap the manifest does -- one the checked defaults, eight inputs and
/// 256 vectors against a cap of 65_536, sit far inside.
fn exhaustive_vectors(
    inputs: usize,
    config: &CertificationConfig,
) -> Result<usize, CandidateCertificationError> {
    let count = 1u64
        .checked_shl(u32::try_from(inputs).unwrap_or(u32::MAX))
        .ok_or(CandidateCertificationError::CounterOverflow)?;
    if count > config.max_certification_transitions {
        return Err(CandidateCertificationError::ExhaustiveVectorCapExceeded {
            inputs,
            count,
            limit: config.max_certification_transitions,
        });
    }
    usize::try_from(count).map_err(|_| CandidateCertificationError::CounterOverflow)
}

/// The world every certification job starts from: probes in, dust strengths
/// already computed, nothing pending.
///
/// Built once. The probes are the same cells for every vector, and so are the
/// dust strengths the simulator computes when it loads a world it has never
/// seen. Loading computes them here; the cells it wrote are then dropped from
/// the dirty set, because they already hold the values a recomputation would
/// give them. A job opens this world with [`Simulator::from_recomputed_world`] and
/// skips the recomputation outright, instead of paying for it per job -- either
/// on load, as it used to, or deferred into its first settle.
///
/// Nothing is simulated: no tick is advanced and no queue is drained, because
/// loading a world queues nothing -- a component's startup transitions, if it
/// has any, are still ahead of every job exactly as they were before. That is
/// what keeps a job's ticks and event counts its own. (`run_until_stable(0)`
/// would not help if that were ever untrue: a zero tick budget cannot drain a
/// queue, it can only report that a non-empty one did not settle.)
fn warm_root_base(world: &World, root_ports: &[RootPort]) -> World {
    let mut world = world.clone();
    for port in root_ports
        .iter()
        .filter(|port| port.role == PortRole::Output)
    {
        compile::probe_caller_cell(&mut world, (port.pin.at.x, port.pin.at.y, port.pin.at.z));
    }
    let mut base = Simulator::new(world).world().clone();
    // The load wrote every dust strength it computed, which marked those cells
    // dirty again. They already hold the values a recomputation would give
    // them, so the request is discarded here rather than honoured once per job.
    base.take_dirty();
    base
}

/// One job's simulator, opened on a copy of the shared base.
///
/// The base came straight out of a load, so its dust strengths are already
/// right and its dirty set is empty: the ordinary constructor would recompute
/// them to the same values every job, which is the one thing this skips. Tick,
/// queue and work counters all start from zero either way.
fn job_simulator(base: &World) -> Simulator {
    Simulator::from_recomputed_world(base.clone())
}

fn drive_root_vector(
    simulator: &mut Simulator,
    root_ports: &[RootPort],
    vector: &[bool],
) -> Result<(), CandidateCertificationError> {
    let inputs = root_ports
        .iter()
        .filter(|port| port.role == PortRole::Input)
        .collect::<Vec<_>>();
    if inputs.len() != vector.len() {
        return Err(CandidateCertificationError::InputWidthMismatch {
            expected: inputs.len(),
            actual: vector.len(),
        });
    }
    for (port, &bit) in inputs.into_iter().zip(vector) {
        compile::drive_caller_cell(
            simulator.world_mut(),
            (port.pin.at.x, port.pin.at.y, port.pin.at.z),
            bit,
        );
    }
    Ok(())
}

fn check_root_outputs(
    world: &World,
    lowered: &Netlist,
    root_ports: &[RootPort],
    vector: &[bool],
    manifest_index: usize,
) -> Result<(), CandidateCertificationError> {
    let expected = evaluate_lowered(lowered, vector)?;
    for (port, expected) in root_ports
        .iter()
        .filter(|port| port.role == PortRole::Output)
        .zip(expected)
    {
        let actual = world.get(port.pin.at.x, port.pin.at.y, port.pin.at.z).lit;
        if actual != expected {
            return Err(CandidateCertificationError::FunctionalMismatch {
                manifest_index,
                output: port.signal.clone(),
                expected,
                actual,
            });
        }
    }
    Ok(())
}

fn measure_root_transition(
    mut simulator: Simulator,
    lowered: &Netlist,
    root_ports: &[RootPort],
    transition: &Transition,
    manifest_index: usize,
    config: &CertificationConfig,
) -> Result<TransitionMeasurement, CandidateCertificationError> {
    let events_before = simulator.work_done();
    drive_root_vector(&mut simulator, root_ports, &transition.from)?;
    settle(
        &mut simulator,
        manifest_index,
        TransitionPhase::Source,
        events_before,
        config,
    )?;
    enforce_event_cap(&simulator, events_before, manifest_index, config)?;
    check_root_outputs(
        simulator.world(),
        lowered,
        root_ports,
        &transition.from,
        manifest_index,
    )?;
    let start_tick = simulator.current_tick();
    drive_root_vector(&mut simulator, root_ports, &transition.to)?;
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
    check_root_outputs(
        simulator.world(),
        lowered,
        root_ports,
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
            if std::env::var_os("REDA_TRACE_SEED_WORLD").is_some() {
                let (sx, sy, sz) = world.size();
                eprintln!(
                    "functional mismatch world dump: vector={vector:?} output={name} at=({x}, {y}, {z}) size=({sx}, {sy}, {sz})"
                );
                for wy in 0..sy {
                    for wz in 0..sz {
                        for wx in 0..sx {
                            let block = world.get(wx, wy, wz);
                            if block.kind != BlockKind::Air {
                                eprintln!(
                                    "  ({wx}, {wy}, {wz}) {:?} facing={:?} power={} lit={}",
                                    block.kind, block.facing, block.power, block.lit
                                );
                            }
                        }
                    }
                }
            }
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
        external_signal_is_high, settle, CandidateCertificationError, CompleteCandidateCertifier,
        ExpandedCandidateCertifier, TransitionPhase,
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
