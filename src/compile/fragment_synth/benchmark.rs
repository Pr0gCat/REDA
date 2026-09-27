use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::circuits::{and4, full_adder, seven_segment, verilog};
use crate::compile::fragment_synth::certification::QualityKey;
use crate::compile::fragment_synth::manifest::TransitionManifest;
use crate::compile::fragment_synth::{
    compile_fragment_synth, ProposalTrace, SynthesisBudget, SynthesisInput, SynthesisResult,
};
use crate::compile::geometry::Anchor;
use crate::compile::metrics::{
    canonical_fingerprint, physical_metrics, Fingerprint, PhysicalMetrics,
};
use crate::compile::planner::PortPlacements;
use crate::compile::revisions::{
    cell_library_revision, physical_verifier_revision, simulator_revision,
};
use crate::compile::topology::Library;
use crate::compile::{self, CompiledCircuit, Netlist};
use crate::redstone::simulator::Simulator;
use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing};
use crate::redstone::world::storage::World;

pub const MAX_TRANSITION_GAME_TICKS: u64 = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkCase {
    pub name: String,
    pub transition_count: usize,
    pub lowered_netlist_hash: Fingerprint,
    pub pin_manifest_hash: Fingerprint,
    pub transition_manifest_hash: Fingerprint,
    pub generated_world_fingerprint: Option<Fingerprint>,
    pub certified: bool,
    pub physical: Option<PhysicalMetrics>,
    pub max_observed_settle_game_ticks_on_manifest: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkBaseline {
    pub baseline_commit: String,
    pub build_profile: String,
    pub cargo_features: Vec<String>,
    pub cell_library_revision: Fingerprint,
    pub simulator_revision: Fingerprint,
    pub verifier_revision: Fingerprint,
    pub cases: Vec<BenchmarkCase>,
}

type ExpectedOutputs = fn(&[bool]) -> Vec<bool>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchmarkOutput {
    pub label: String,
    pub signal: String,
}

impl BenchmarkOutput {
    pub fn new(label: impl Into<String>, signal: impl Into<String>) -> Self {
        BenchmarkOutput {
            label: label.into(),
            signal: signal.into(),
        }
    }
}

pub struct BenchmarkFixture {
    name: String,
    lowered_netlist: Netlist,
    input_ports: Vec<String>,
    outputs: Vec<BenchmarkOutput>,
    expected: ExpectedOutputs,
    placements: PortPlacements,
    explicit_legacy_new_coverage: bool,
}

impl BenchmarkFixture {
    pub fn new(
        name: impl Into<String>,
        lowered_netlist: Netlist,
        input_ports: Vec<String>,
        outputs: Vec<BenchmarkOutput>,
        expected: ExpectedOutputs,
        placements: PortPlacements,
    ) -> Self {
        BenchmarkFixture {
            name: name.into(),
            lowered_netlist,
            input_ports,
            outputs,
            expected,
            placements,
            explicit_legacy_new_coverage: false,
        }
    }

    fn with_explicit_legacy_new_coverage(mut self) -> Self {
        self.explicit_legacy_new_coverage = true;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn lowered_netlist(&self) -> &Netlist {
        &self.lowered_netlist
    }

    pub fn placements(&self) -> &PortPlacements {
        &self.placements
    }

    pub fn transition_manifest(&self) -> TransitionManifest {
        TransitionManifest::new(self.input_ports.clone())
    }

    fn blank_case(&self) -> BenchmarkCase {
        let manifest = self.transition_manifest();
        BenchmarkCase {
            name: self.name.clone(),
            transition_count: manifest.transitions().len(),
            lowered_netlist_hash: canonical_netlist_fingerprint(&self.lowered_netlist),
            pin_manifest_hash: canonical_pin_manifest_fingerprint(&self.placements),
            transition_manifest_hash: manifest.fingerprint(),
            generated_world_fingerprint: None,
            certified: false,
            physical: None,
            max_observed_settle_game_ticks_on_manifest: None,
        }
    }
}

pub struct AcceptanceEvaluator {
    fixtures: Vec<BenchmarkFixture>,
}

impl AcceptanceEvaluator {
    pub fn new(fixtures: Vec<BenchmarkFixture>) -> Self {
        AcceptanceEvaluator { fixtures }
    }

    pub fn fixtures(&self) -> &[BenchmarkFixture] {
        &self.fixtures
    }

    pub fn fixture(&self, name: &str) -> Option<&BenchmarkFixture> {
        self.fixtures.iter().find(|fixture| fixture.name == name)
    }

    pub fn evaluate_world(
        &self,
        name: &str,
        compiled: &CompiledCircuit,
    ) -> Result<BenchmarkCase, String> {
        let fixture = self
            .fixture(name)
            .ok_or_else(|| format!("unknown benchmark fixture `{name}`"))?;
        let manifest = fixture.transition_manifest();
        if manifest.transitions().is_empty() {
            return Err(format!("`{name}` has an empty transition manifest"));
        }
        let sinks = validate_output_identities(compiled, fixture)?;

        // Drivers, probes and the circuit's own initial settle are the same
        // work for every transition -- nothing about them depends on which
        // vector comes next -- so they happen once and each transition opens on
        // a copy of the result.
        //
        // What a transition does *not* inherit is the simulator state the
        // initial settle built up: its tick counter and its per-torch change
        // history both start from zero again. That is sound for what this
        // measures. The settle ends at a fixpoint, so the world a transition
        // opens on is the same one it opened on before; the ticks it reports
        // are counted per `run_until_stable` call, not from the epoch; and
        // torch burnout is a rolling 60-tick window, so history from a settle
        // that has already converged can only make a torch burn out sooner
        // than its own transition would. Carrying it forward would let a
        // circuit's startup charge a transition for work that was not its. The settle's own writes left the world's dirty
        // set full; the strengths they asked for are already in it, so the set
        // is dropped and each transition loads the copy with
        // `from_recomputed_world` rather than recomputing what is already
        // there.
        let (drivers, warm) = {
            let mut world = compiled.world.clone();
            let drivers = install_drivers(&mut world, compiled, fixture)?;
            install_probes(&mut world, fixture)?;
            let mut settling = Simulator::new(world);
            settling
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| format!("{} did not initially settle: {error:?}", fixture.name))?;
            let mut warm = settling.world().clone();
            warm.take_dirty();
            (drivers, warm)
        };

        let mut worst = 0;
        for transition in manifest.transitions() {
            let mut simulator = Simulator::from_recomputed_world(warm.clone());

            drive(&mut simulator, &drivers, &transition.from);
            simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| {
                    format!(
                        "{} did not settle at from={:?}: {error:?}",
                        fixture.name, transition.from
                    )
                })?;
            check_outputs(fixture, simulator.world(), &sinks, &transition.from)?;

            drive(&mut simulator, &drivers, &transition.to);
            let ticks = simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| {
                    format!(
                        "{} did not settle at to={:?}: {error:?}",
                        fixture.name, transition.to
                    )
                })?;
            check_outputs(fixture, simulator.world(), &sinks, &transition.to)?;
            worst = worst.max(ticks);
        }

        let mut case = fixture.blank_case();
        case.generated_world_fingerprint = Some(canonical_world_fingerprint(&compiled.world));
        case.certified = true;
        case.physical = Some(physical_metrics(
            &compiled.world,
            fixture.lowered_netlist.gates.len() as u64,
        ));
        case.max_observed_settle_game_ticks_on_manifest = Some(worst);
        Ok(case)
    }

    /// The worst transition of `name`'s manifest on `compiled`, with every
    /// net's arrival: the same drivers, probes, warm start and manifest
    /// [`Self::evaluate_world`] measures, so its settle is the acceptance
    /// ticks by construction. The first transition reaching the worst settle
    /// is the one returned.
    pub fn worst_transition_timing(
        &self,
        name: &str,
        compiled: &CompiledCircuit,
    ) -> Result<crate::timing::TransitionResult, String> {
        let fixture = self
            .fixture(name)
            .ok_or_else(|| format!("unknown benchmark fixture `{name}`"))?;
        let manifest = fixture.transition_manifest();
        let (drivers, warm) = {
            let mut world = compiled.world.clone();
            let drivers = install_drivers(&mut world, compiled, fixture)?;
            install_probes(&mut world, fixture)?;
            let mut settling = Simulator::new(world);
            settling
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| format!("{} did not initially settle: {error:?}", fixture.name))?;
            let mut warm = settling.world().clone();
            warm.take_dirty();
            (drivers, warm)
        };
        let mut worst: Option<crate::timing::TransitionResult> = None;
        for transition in manifest.transitions() {
            let mut simulator = Simulator::from_recomputed_world(warm.clone());
            drive(&mut simulator, &drivers, &transition.from);
            simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| format!("{} did not settle: {error:?}", fixture.name))?;
            simulator.attach_observer(crate::timing::watch_all_nets(compiled));
            let result =
                crate::timing::measure_transition(&mut simulator, MAX_TRANSITION_GAME_TICKS, |sim| {
                    drive(sim, &drivers, &transition.to)
                })
                .map_err(|error| format!("{} did not settle: {error:?}", fixture.name))?;
            if worst
                .as_ref()
                .is_none_or(|worst| result.settle_game_ticks > worst.settle_game_ticks)
            {
                worst = Some(result);
            }
        }
        worst.ok_or_else(|| format!("`{name}` has an empty transition manifest"))
    }

    pub fn capture_legacy(&self, baseline_commit: String) -> Result<BenchmarkBaseline, String> {
        let mut cases = Vec::with_capacity(self.fixtures.len());
        for fixture in &self.fixtures {
            if fixture.explicit_legacy_new_coverage {
                cases.push(fixture.blank_case());
                continue;
            }
            let compiled = compile::compile_legacy(&fixture.lowered_netlist)
                .map_err(|error| format!("{} legacy compile failed: {error}", fixture.name))?;
            cases.push(self.evaluate_world(&fixture.name, &compiled)?);
        }
        Ok(self.baseline_with_cases(baseline_commit, cases))
    }

    pub fn baseline(&self, baseline_commit: String) -> BenchmarkBaseline {
        self.baseline_with_cases(baseline_commit, Vec::new())
    }

    fn baseline_with_cases(
        &self,
        baseline_commit: String,
        cases: Vec<BenchmarkCase>,
    ) -> BenchmarkBaseline {
        let library = Library::default_library();
        BenchmarkBaseline {
            baseline_commit,
            build_profile: if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            },
            cargo_features: Vec::new(),
            cell_library_revision: cell_library_revision(&library),
            simulator_revision: simulator_revision(),
            verifier_revision: physical_verifier_revision(),
            cases,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FragmentAcceptanceCase {
    pub name: String,
    pub budget: u64,
    pub compiled_and_certified: bool,
    pub error: Option<String>,
    pub baseline: BenchmarkCase,
    pub measured: Option<BenchmarkCase>,
    pub quality: Option<QualityKey>,
    pub evaluations_used: Option<u64>,
    pub case_fingerprint: Option<String>,
    pub candidate_fingerprint: Option<Fingerprint>,
    pub trace: Vec<ProposalTrace>,
    pub new_coverage: bool,
    pub no_tick_regression: bool,
    pub no_block_regression: bool,
    pub pinned_ten_percent_tick_improvement: Option<bool>,
    pub strict_block_improvement: Option<bool>,
}

impl FragmentAcceptanceCase {
    pub fn passed(&self) -> bool {
        self.compiled_and_certified
            && self.no_tick_regression
            && self.no_block_regression
            && self.pinned_ten_percent_tick_improvement.unwrap_or(true)
            && self.strict_block_improvement.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FragmentBudgetRun {
    pub budget: u64,
    pub cases: Vec<FragmentAcceptanceCase>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FragmentAcceptanceReport {
    pub schema_version: u32,
    pub baseline_commit: String,
    pub base_shuffle_seed: u64,
    pub derived_shuffle_seeds: Vec<u64>,
    pub planned_budget_orders: Vec<Vec<u64>>,
    pub repeatability_executed: bool,
    pub runs: Vec<FragmentBudgetRun>,
    pub quality_passing_evaluations: Option<u64>,
    pub replacement_gate_passed: bool,
    pub shipping_evaluations: Option<u64>,
    pub failures: Vec<String>,
}

pub fn deterministic_budget_orders(
    budgets: &[u64],
    repetitions: usize,
    base_seed: u64,
) -> Vec<Vec<u64>> {
    (0..repetitions)
        .map(|repetition| {
            let mut state = base_seed.wrapping_add(repetition as u64);
            let mut order = budgets.to_vec();
            for index in (1..order.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let selected = usize::try_from(state % (index as u64 + 1)).unwrap_or(0);
                order.swap(index, selected);
            }
            order
        })
        .collect()
}

/// Every fixture the evaluator carries, each through [`evaluate_fragment_case`],
/// and whether all of them passed.
pub fn evaluate_fragment_budget(
    evaluator: &AcceptanceEvaluator,
    baseline: &BenchmarkBaseline,
    budget: u64,
) -> FragmentBudgetRun {
    let cases = evaluator
        .fixtures
        .iter()
        .map(|fixture| evaluate_fragment_case(evaluator, baseline, fixture, budget))
        .collect::<Vec<_>>();
    let passed = cases.iter().all(FragmentAcceptanceCase::passed);
    FragmentBudgetRun {
        budget,
        cases,
        passed,
    }
}

/// One fixture compiled at `budget` through the public entry, measured on
/// its own manifest, and judged against its baseline case.
///
/// The one place a case is scored: the corpus run above maps every fixture
/// through this, and a per-case gate runs exactly this for one name, so both
/// judge by the same [`FragmentAcceptanceCase::passed`].
pub fn evaluate_fragment_case(
    evaluator: &AcceptanceEvaluator,
    baseline: &BenchmarkBaseline,
    fixture: &BenchmarkFixture,
    budget: u64,
) -> FragmentAcceptanceCase {
    evaluate_fragment_case_with_world(evaluator, baseline, fixture, budget).0
}

/// [`evaluate_fragment_case`], handing back the circuit it compiled as well.
///
/// `Some` whenever the compile succeeded, whether or not the measurement then
/// did, so a gate that wants to look at the world -- where a pinned port was
/// reported, what stands in the caller's cell -- reads the very circuit that
/// was scored rather than compiling a second one. The circuit is moved out of
/// the synthesis result, not cloned; the case keeps everything else.
pub fn evaluate_fragment_case_with_world(
    evaluator: &AcceptanceEvaluator,
    baseline: &BenchmarkBaseline,
    fixture: &BenchmarkFixture,
    budget: u64,
) -> (FragmentAcceptanceCase, Option<CompiledCircuit>) {
    let baseline_case = baseline
        .cases
        .iter()
        .find(|case| case.name == fixture.name)
        .cloned()
        .unwrap_or_else(|| fixture.blank_case());
    let result = compile_fragment_synth(
        SynthesisInput {
            lowered: &fixture.lowered_netlist,
            source_provenance: None,
            pins: Some(&fixture.placements),
        },
        SynthesisBudget::Evaluations(budget),
    );
    let SynthesisResult {
        compiled,
        metrics,
        trace,
        evaluations_used,
        case_fingerprint,
        candidate_fingerprint,
        ..
    } = match result {
        Ok(result) => result,
        Err(error) => {
            return (
                FragmentAcceptanceCase {
                    name: fixture.name.clone(),
                    budget,
                    compiled_and_certified: false,
                    error: Some(error.to_string()),
                    baseline: baseline_case,
                    measured: None,
                    quality: None,
                    evaluations_used: None,
                    case_fingerprint: None,
                    candidate_fingerprint: None,
                    trace: Vec::new(),
                    new_coverage: false,
                    no_tick_regression: false,
                    no_block_regression: false,
                    pinned_ten_percent_tick_improvement: None,
                    strict_block_improvement: None,
                },
                None,
            )
        }
    };
    let case = match evaluator.evaluate_world(&fixture.name, &compiled) {
        Err(error) => FragmentAcceptanceCase {
            name: fixture.name.clone(),
            budget,
            compiled_and_certified: false,
            error: Some(error),
            baseline: baseline_case,
            measured: None,
            quality: Some(metrics.quality),
            evaluations_used: Some(evaluations_used),
            case_fingerprint: Some(case_fingerprint.as_str().to_string()),
            candidate_fingerprint: Some(candidate_fingerprint),
            trace,
            new_coverage: false,
            no_tick_regression: false,
            no_block_regression: false,
            pinned_ten_percent_tick_improvement: None,
            strict_block_improvement: None,
        },
        Ok(measured) => {
            let new_coverage = !baseline_case.certified;
            let baseline_ticks = baseline_case.max_observed_settle_game_ticks_on_manifest;
            let measured_ticks = measured.max_observed_settle_game_ticks_on_manifest;
            let baseline_blocks = baseline_case
                .physical
                .as_ref()
                .map(|physical| physical.non_air_blocks);
            let measured_blocks = measured
                .physical
                .as_ref()
                .map(|physical| physical.non_air_blocks);
            let no_tick_regression = baseline_ticks
                .zip(measured_ticks)
                .is_none_or(|(old, new)| new <= old);
            let no_block_regression = baseline_blocks
                .zip(measured_blocks)
                .is_none_or(|(old, new)| new <= old);
            let is_pinned = fixture.name == "pinned:verilog:seven_segment";
            let (pinned_ten_percent_tick_improvement, strict_block_improvement) =
                pinned_improvement_predicates(
                    is_pinned,
                    baseline_case.certified,
                    baseline_ticks,
                    measured_ticks,
                    baseline_blocks,
                    measured_blocks,
                );
            FragmentAcceptanceCase {
                name: fixture.name.clone(),
                budget,
                compiled_and_certified: true,
                error: None,
                baseline: baseline_case,
                measured: Some(measured),
                quality: Some(metrics.quality),
                evaluations_used: Some(evaluations_used),
                case_fingerprint: Some(case_fingerprint.as_str().to_string()),
                candidate_fingerprint: Some(candidate_fingerprint),
                trace,
                new_coverage,
                no_tick_regression,
                no_block_regression,
                pinned_ten_percent_tick_improvement,
                strict_block_improvement,
            }
        }
    };
    (case, Some(compiled))
}

fn pinned_improvement_predicates(
    is_pinned: bool,
    baseline_certified: bool,
    baseline_ticks: Option<u64>,
    measured_ticks: Option<u64>,
    baseline_blocks: Option<u64>,
    measured_blocks: Option<u64>,
) -> (Option<bool>, Option<bool>) {
    let ticks = if is_pinned && baseline_certified {
        Some(
            baseline_ticks
                .zip(measured_ticks)
                .is_some_and(|(old, new)| 10_u128 * u128::from(new) <= 9_u128 * u128::from(old)),
        )
    } else {
        None
    };
    let blocks = if is_pinned && baseline_certified {
        Some(
            baseline_blocks
                .zip(measured_blocks)
                .is_some_and(|(old, new)| new < old),
        )
    } else {
        None
    };
    (ticks, blocks)
}

pub fn build_acceptance_report(
    evaluator: &AcceptanceEvaluator,
    baseline: &BenchmarkBaseline,
    budgets: &[u64],
    base_shuffle_seed: u64,
) -> FragmentAcceptanceReport {
    let orders = deterministic_budget_orders(budgets, 3, base_shuffle_seed);
    let mut runs = Vec::new();
    let mut quality_passing_evaluations = None;
    for &budget in budgets {
        let run = evaluate_fragment_budget(evaluator, baseline, budget);
        let seed_failed = run.cases.iter().any(|case| !case.compiled_and_certified);
        if run.passed && quality_passing_evaluations.is_none() {
            quality_passing_evaluations = Some(budget);
        }
        runs.push(run);
        if seed_failed {
            break;
        }
    }
    let failures = runs
        .iter()
        .flat_map(|run| {
            run.cases
                .iter()
                .filter(|case| !case.passed())
                .map(move |case| {
                    format!(
                        "budget {} case {} failed: {}",
                        run.budget,
                        case.name,
                        case.error.as_deref().unwrap_or("quality gate")
                    )
                })
        })
        .collect();
    FragmentAcceptanceReport {
        schema_version: 1,
        baseline_commit: baseline.baseline_commit.clone(),
        base_shuffle_seed,
        derived_shuffle_seeds: (0..3)
            .map(|index| base_shuffle_seed.wrapping_add(index))
            .collect(),
        planned_budget_orders: orders,
        repeatability_executed: false,
        runs,
        quality_passing_evaluations,
        replacement_gate_passed: false,
        shipping_evaluations: None,
        failures,
    }
}

pub fn write_acceptance_json(path: &Path, report: &FragmentAcceptanceReport) -> Result<(), String> {
    refuse_existing_output(path, false)?;
    let mut bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("could not serialize acceptance report: {error}"))?;
    bytes.push(b'\n');
    stage_json_bytes(path, &bytes)?.persist(false)
}

pub fn write_generated_text(path: &Path, text: &str) -> Result<(), String> {
    refuse_existing_output(path, false)?;
    stage_json_bytes(path, text.as_bytes())?.persist(false)
}

pub fn shipping_config_source(report: &FragmentAcceptanceReport) -> Result<String, String> {
    let evaluations = report
        .shipping_evaluations
        .filter(|_| report.replacement_gate_passed)
        .ok_or_else(|| "replacement gate did not pass".to_string())?;
    let cases = report
        .runs
        .iter()
        .find(|run| run.budget == evaluations)
        .ok_or_else(|| "passing budget run is missing".to_string())?
        .cases
        .iter()
        .map(|case| {
            format!(
                "    ({:?}, {:?}),",
                case.name,
                case.case_fingerprint.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "// @generated by fragment_acceptance; do not edit.\n\
         use crate::compile::fragment_synth::config::{{CertificationConfig, SearchConfig}};\n\
         use crate::compile::fragment_synth::manifest::TransitionManifestKind;\n\
         use crate::compile::routing::RouterLimits;\n\n\
         pub const SHIPPING_EVALUATIONS: u64 = {evaluations};\n\
         pub const SHIPPING_BASE_SHUFFLE_SEED: u64 = 0x{:016x};\n\
         pub const SHIPPING_CASE_FINGERPRINTS: &[(&str, &str)] = &[\n{}\n];\n\n\
         pub fn shipping_search_config() -> SearchConfig {{\n\
             SearchConfig {{\n\
                 router_limits: RouterLimits {{ max_node_expansions: 262_144, max_queue_entries: 262_144 }},\n\
                 max_seed_shell_radius: 64, max_fragment_shell_radius: 32,\n\
                 max_seed_backtracks: 1_000_000, max_fragment_backtracks_per_proposal: 100_000,\n\
                 max_equivalence_proof_steps: 1_000_000, max_certification_transitions: 65_536,\n\
                 max_simulator_events_per_transition: 1_000_000, max_game_ticks_per_transition: 2_048,\n\
                 fragment_instance_schedule: vec![1, 2, 4, 8], max_boundary_nets: 32,\n\
                 max_fragment_manhattan_radius: 48,\n\
             }}\n\
         }}\n\n\
         pub fn shipping_certification_config() -> CertificationConfig {{\n\
             CertificationConfig {{ exhaustive_input_threshold: 8, transition_manifest_kind: TransitionManifestKind::FixedV1,\n\
                 max_equivalence_proof_steps: 1_000_000, max_certification_transitions: 65_536,\n\
                 max_simulator_events_per_transition: 1_000_000, max_game_ticks_per_transition: 2_048 }}\n\
         }}\n",
        report.base_shuffle_seed, cases
    ))
}

#[derive(Clone, Copy)]
enum Driver {
    Lever((i32, i32, i32)),
    CallerCell((i32, i32, i32)),
}

fn install_drivers(
    world: &mut World,
    compiled: &CompiledCircuit,
    fixture: &BenchmarkFixture,
) -> Result<Vec<Driver>, String> {
    fixture
        .input_ports
        .iter()
        .map(|name| {
            if let Some(pin) = fixture.placements.get(name) {
                require_empty_caller_cell(world, name, pin.at)?;
                Ok(Driver::CallerCell((pin.at.x, pin.at.y, pin.at.z)))
            } else {
                compiled
                    .input_positions
                    .get(name)
                    .copied()
                    .map(Driver::Lever)
                    .ok_or_else(|| format!("compiled circuit has no input `{name}`"))
            }
        })
        .collect()
}

fn install_probes(world: &mut World, fixture: &BenchmarkFixture) -> Result<(), String> {
    for output in &fixture.outputs {
        if let Some(pin) = fixture.placements.get(&output.signal) {
            require_empty_caller_cell(world, &output.signal, pin.at)?;
            compile::probe_caller_cell(world, (pin.at.x, pin.at.y, pin.at.z));
        }
    }
    Ok(())
}

fn require_empty_caller_cell(world: &World, name: &str, at: Anchor) -> Result<(), String> {
    let kind = world.get(at.x, at.y, at.z).kind;
    if kind == BlockKind::Air {
        Ok(())
    } else {
        Err(format!(
            "`{name}` caller cell ({}, {}, {}) holds {kind:?}",
            at.x, at.y, at.z
        ))
    }
}

fn validate_output_identities(
    compiled: &CompiledCircuit,
    fixture: &BenchmarkFixture,
) -> Result<Vec<(i32, i32, i32)>, String> {
    let mut labels = BTreeSet::new();
    let mut signals = BTreeSet::new();
    for output in &fixture.outputs {
        if output.label.is_empty() {
            return Err(format!("{} has an unresolved output label", fixture.name));
        }
        if output.signal.is_empty() {
            return Err(format!("{} has an unresolved output signal", fixture.name));
        }
        if !labels.insert(output.label.as_str()) {
            return Err(format!(
                "{} has duplicate output label `{}`",
                fixture.name, output.label
            ));
        }
        if !signals.insert(output.signal.as_str()) {
            return Err(format!(
                "{} has duplicate output signal `{}`",
                fixture.name, output.signal
            ));
        }
    }

    let fixture_signals: Vec<_> = fixture
        .outputs
        .iter()
        .map(|output| output.signal.as_str())
        .collect();
    let netlist_signals: Vec<_> = fixture
        .lowered_netlist
        .outputs
        .iter()
        .map(String::as_str)
        .collect();
    if fixture_signals != netlist_signals {
        return Err(format!(
            "{} fixture output order {:?} does not match netlist outputs {:?}",
            fixture.name, fixture_signals, netlist_signals
        ));
    }

    let compiled_signals: BTreeSet<_> = compiled
        .output_positions
        .keys()
        .map(String::as_str)
        .collect();
    let netlist_signal_set: BTreeSet<_> = netlist_signals.iter().copied().collect();
    if let Some(missing) = netlist_signal_set.difference(&compiled_signals).next() {
        return Err(format!(
            "{} is missing compiled output `{missing}`",
            fixture.name
        ));
    }
    if let Some(extra) = compiled_signals.difference(&netlist_signal_set).next() {
        return Err(format!(
            "{} has extra compiled output `{extra}`",
            fixture.name
        ));
    }

    let expected_count = (fixture.expected)(&vec![false; fixture.input_ports.len()]).len();
    if expected_count != fixture.outputs.len() {
        return Err(format!(
            "{} expected-output count {} does not match {} declared outputs",
            fixture.name,
            expected_count,
            fixture.outputs.len()
        ));
    }

    fixture
        .outputs
        .iter()
        .map(|output| {
            compiled
                .output_positions
                .get(&output.signal)
                .copied()
                .ok_or_else(|| format!("compiled circuit has no output `{}`", output.signal))
        })
        .collect()
}

fn drive(simulator: &mut Simulator, drivers: &[Driver], bits: &[bool]) {
    for (driver, &bit) in drivers.iter().zip(bits) {
        match driver {
            Driver::Lever((x, y, z)) => {
                let mut state = simulator.world().get(*x, *y, *z).clone();
                state.lit = bit;
                simulator.world_mut().set(*x, *y, *z, state);
            }
            Driver::CallerCell(at) => compile::drive_caller_cell(simulator.world_mut(), *at, bit),
        }
    }
}

fn check_outputs(
    fixture: &BenchmarkFixture,
    world: &World,
    sinks: &[(i32, i32, i32)],
    bits: &[bool],
) -> Result<(), String> {
    let expected = (fixture.expected)(bits);
    if expected.len() != sinks.len() {
        return Err(format!(
            "{} expected-output count {} does not match {} declared outputs",
            fixture.name,
            expected.len(),
            sinks.len()
        ));
    }
    for ((output, &(x, y, z)), &want) in fixture.outputs.iter().zip(sinks).zip(&expected) {
        let got = world.get(x, y, z).lit;
        if got != want {
            return Err(format!(
                "{} {:?} -> `{}` ({}) expected {want}, got {got}",
                fixture.name, bits, output.label, output.signal
            ));
        }
    }
    Ok(())
}

fn and4_expected(bits: &[bool]) -> Vec<bool> {
    vec![bits.iter().all(|&bit| bit)]
}

fn full_adder_expected(bits: &[bool]) -> Vec<bool> {
    let ones = bits.iter().filter(|&&bit| bit).count();
    vec![ones % 2 == 1, ones >= 2]
}

fn seven_segment_expected(bits: &[bool]) -> Vec<bool> {
    let digit = bits
        .iter()
        .fold(0usize, |value, &bit| value * 2 + usize::from(bit));
    (0..seven_segment::SEGMENT_NAMES.len())
        .map(|segment| {
            digit < seven_segment::TRUTH_TABLE.len()
                && seven_segment::TRUTH_TABLE[digit][segment] == 1
        })
        .collect()
}

fn segment_a_expected(bits: &[bool]) -> Vec<bool> {
    vec![seven_segment_expected(bits)[0]]
}

fn labels_in_order(
    labels: &[(String, String)],
    names: &[&str],
) -> Result<Vec<BenchmarkOutput>, String> {
    names
        .iter()
        .map(|name| {
            labels
                .iter()
                .find(|(label, _)| label == name)
                .map(|(_, signal)| BenchmarkOutput::new(*name, signal))
                .ok_or_else(|| format!("Verilog output `{name}` is missing"))
        })
        .collect()
}

pub fn legacy_benchmark_evaluator() -> Result<AcceptanceEvaluator, String> {
    let (and4_netlist, and4_output) = and4::build_and4_netlist();
    let (adder_netlist, adder_outputs) = full_adder::build_full_adder_netlist();
    let (segment_netlist, segment_output) = seven_segment::build_single_segment_netlist(0);
    let (decoder_netlist, decoder_outputs) = seven_segment::build_seven_segment_netlist();

    let verilog_and4 = verilog::find("verilog:and4").expect("the catalog ships verilog:and4");
    let (verilog_and4_gate_level, verilog_and4_labels) = verilog_and4.baked_netlist();
    let verilog_and4_lowered =
        compile::lowering::lower(&verilog_and4_gate_level).map_err(|error| error.to_string())?;
    let verilog_and4_outputs = labels_in_order(&verilog_and4_labels, &[and4::OUTPUT_NAME])?;

    let verilog_decoder =
        verilog::find("verilog:seven_segment").expect("the catalog ships verilog:seven_segment");
    let (verilog_decoder_gate_level, verilog_decoder_labels) = verilog_decoder.baked_netlist();
    let verilog_decoder_lowered = compile::lowering::lower_optimised(&verilog_decoder_gate_level)
        .map_err(|error| error.to_string())?;
    let verilog_decoder_outputs =
        labels_in_order(&verilog_decoder_labels, &seven_segment::SEGMENT_NAMES)?;

    let mut pinned_decoder_ports = PortPlacements::default();
    for ((_, at, toward), output) in pinned_glyph().iter().zip(&verilog_decoder_outputs) {
        pinned_decoder_ports.pin(output.signal.clone(), *at, *toward);
    }
    for (index, name) in seven_segment::INPUT_NAMES.iter().enumerate() {
        pinned_decoder_ports.pin(
            *name,
            Anchor {
                x: 76 + 12 * index as i32,
                y: 1,
                z: 144,
            },
            Facing::North,
        );
    }

    let inputs = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect();
    let decoder_output_signals = seven_segment::SEGMENT_NAMES
        .iter()
        .map(|name| BenchmarkOutput::new(*name, &decoder_outputs[*name]))
        .collect();
    let adder_output_signals = full_adder::OUTPUT_NAMES
        .iter()
        .map(|name| BenchmarkOutput::new(*name, &adder_outputs[name]))
        .collect();

    Ok(AcceptanceEvaluator::new(vec![
        BenchmarkFixture::new(
            "and4",
            and4_netlist,
            inputs(&and4::INPUT_NAMES),
            vec![BenchmarkOutput::new(and4::OUTPUT_NAME, and4_output)],
            and4_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "verilog:and4",
            verilog_and4_lowered,
            inputs(&and4::INPUT_NAMES),
            verilog_and4_outputs,
            and4_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "full_adder",
            adder_netlist,
            inputs(&full_adder::INPUT_NAMES),
            adder_output_signals,
            full_adder_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "segment_a",
            segment_netlist,
            inputs(&seven_segment::INPUT_NAMES),
            vec![BenchmarkOutput::new("a", segment_output)],
            segment_a_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "seven_segment",
            decoder_netlist,
            inputs(&seven_segment::INPUT_NAMES),
            decoder_output_signals,
            seven_segment_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "pinned:verilog:seven_segment",
            verilog_decoder_lowered,
            inputs(&seven_segment::INPUT_NAMES),
            verilog_decoder_outputs,
            seven_segment_expected,
            pinned_decoder_ports,
        )
        .with_explicit_legacy_new_coverage(),
    ]))
}

/// Acceptance corpus that never invokes Yosys.
///
/// The two Verilog entries intentionally use their checked-in baked gate-level
/// netlists and the same lowering choice as the production acceptance corpus:
/// plain `lower` for `verilog:and4`, and `lower_optimised` for the decoder.
pub fn baked_benchmark_evaluator() -> Result<AcceptanceEvaluator, String> {
    let (and4_netlist, and4_output) = and4::build_and4_netlist();
    let (adder_netlist, adder_outputs) = full_adder::build_full_adder_netlist();
    let (segment_netlist, segment_output) = seven_segment::build_single_segment_netlist(0);
    let (decoder_netlist, decoder_outputs) = seven_segment::build_seven_segment_netlist();

    let verilog_and4 = verilog::find("verilog:and4").expect("the catalog ships verilog:and4");
    let (verilog_and4_gate_level, verilog_and4_labels) = verilog_and4.baked_netlist();
    let verilog_and4_lowered =
        compile::lowering::lower(&verilog_and4_gate_level).map_err(|error| error.to_string())?;
    let verilog_and4_outputs = labels_in_order(&verilog_and4_labels, &[and4::OUTPUT_NAME])?;

    let verilog_decoder =
        verilog::find("verilog:seven_segment").expect("the catalog ships verilog:seven_segment");
    let (verilog_decoder_gate_level, verilog_decoder_labels) = verilog_decoder.baked_netlist();
    let verilog_decoder_lowered = compile::lowering::lower_optimised(&verilog_decoder_gate_level)
        .map_err(|error| error.to_string())?;
    let verilog_decoder_outputs =
        labels_in_order(&verilog_decoder_labels, &seven_segment::SEGMENT_NAMES)?;

    let mut pinned_decoder_ports = PortPlacements::default();
    for ((_, at, toward), output) in pinned_glyph().iter().zip(&verilog_decoder_outputs) {
        pinned_decoder_ports.pin(output.signal.clone(), *at, *toward);
    }
    for (index, name) in seven_segment::INPUT_NAMES.iter().enumerate() {
        pinned_decoder_ports.pin(
            *name,
            Anchor {
                x: 76 + 12 * index as i32,
                y: 1,
                z: 144,
            },
            Facing::North,
        );
    }

    let inputs = |names: &[&str]| names.iter().map(|name| (*name).to_string()).collect();
    let decoder_output_signals = seven_segment::SEGMENT_NAMES
        .iter()
        .map(|name| BenchmarkOutput::new(*name, &decoder_outputs[*name]))
        .collect();
    let adder_output_signals = full_adder::OUTPUT_NAMES
        .iter()
        .map(|name| BenchmarkOutput::new(*name, &adder_outputs[name]))
        .collect();

    Ok(AcceptanceEvaluator::new(vec![
        BenchmarkFixture::new(
            "and4",
            and4_netlist,
            inputs(&and4::INPUT_NAMES),
            vec![BenchmarkOutput::new(and4::OUTPUT_NAME, and4_output)],
            and4_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "verilog:and4",
            verilog_and4_lowered,
            inputs(&and4::INPUT_NAMES),
            verilog_and4_outputs,
            and4_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "full_adder",
            adder_netlist,
            inputs(&full_adder::INPUT_NAMES),
            adder_output_signals,
            full_adder_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "segment_a",
            segment_netlist,
            inputs(&seven_segment::INPUT_NAMES),
            vec![BenchmarkOutput::new("a", segment_output)],
            segment_a_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "seven_segment",
            decoder_netlist,
            inputs(&seven_segment::INPUT_NAMES),
            decoder_output_signals,
            seven_segment_expected,
            PortPlacements::default(),
        ),
        BenchmarkFixture::new(
            "pinned:verilog:seven_segment",
            verilog_decoder_lowered,
            inputs(&seven_segment::INPUT_NAMES),
            verilog_decoder_outputs,
            seven_segment_expected,
            pinned_decoder_ports,
        )
        .with_explicit_legacy_new_coverage(),
    ]))
}

/// The decoder's display: one wall at `z = 24` standing up in the x-y plane,
/// every segment facing north toward whoever reads it and fed from behind.
/// Seen from the north, `b` and `c` are on the reader's right (west).
fn pinned_glyph() -> [(&'static str, Anchor, Facing); 7] {
    [
        ("a", Anchor { x: 76, y: 13, z: 24 }, Facing::North),
        ("b", Anchor { x: 68, y: 10, z: 24 }, Facing::North),
        ("c", Anchor { x: 68, y: 4, z: 24 }, Facing::North),
        ("d", Anchor { x: 76, y: 1, z: 24 }, Facing::North),
        ("e", Anchor { x: 84, y: 4, z: 24 }, Facing::North),
        ("f", Anchor { x: 84, y: 10, z: 24 }, Facing::North),
        ("g", Anchor { x: 76, y: 7, z: 24 }, Facing::North),
    ]
}

#[derive(Serialize)]
struct CanonicalNetlist<'a> {
    inputs: &'a [String],
    outputs: &'a [String],
    gates: Vec<CanonicalGate<'a>>,
}

#[derive(Serialize)]
struct CanonicalGate<'a> {
    name: &'a str,
    inputs: &'a [String],
    output: &'a str,
    kind: &'static str,
    arity: usize,
}

fn canonical_netlist_fingerprint(netlist: &Netlist) -> Fingerprint {
    canonical_fingerprint(&canonical_netlist_bytes(netlist))
}

fn canonical_netlist_bytes(netlist: &Netlist) -> Vec<u8> {
    let canonical = CanonicalNetlist {
        inputs: &netlist.inputs,
        outputs: &netlist.outputs,
        gates: netlist
            .gates
            .iter()
            .map(|gate| CanonicalGate {
                name: &gate.name,
                inputs: &gate.inputs,
                output: &gate.output,
                kind: gate.kind.wire_name(),
                arity: gate.kind.arity(),
            })
            .collect(),
    };
    serde_json::to_vec(&canonical).expect("a netlist must serialize")
}

#[derive(Serialize)]
struct CanonicalPin<'a> {
    port: &'a str,
    x: i32,
    y: i32,
    z: i32,
    toward: &'static str,
}

fn canonical_pin_manifest_fingerprint(placements: &PortPlacements) -> Fingerprint {
    canonical_fingerprint(&canonical_pin_manifest_bytes(placements))
}

fn canonical_pin_manifest_bytes(placements: &PortPlacements) -> Vec<u8> {
    let pins: Vec<_> = placements
        .iter()
        .map(|(port, pin)| CanonicalPin {
            port,
            x: pin.at.x,
            y: pin.at.y,
            z: pin.at.z,
            toward: facing_name(pin.toward),
        })
        .collect();
    serde_json::to_vec(&pins).expect("a pin manifest must serialize")
}

#[derive(Serialize)]
struct CanonicalWorld {
    size: (i32, i32, i32),
    cells: Vec<CanonicalCell>,
}

#[derive(Serialize)]
struct CanonicalCell {
    x: i32,
    y: i32,
    z: i32,
    kind: &'static str,
    facing: Option<&'static str>,
    power: u8,
    lit: bool,
    delay: u8,
    face: Option<&'static str>,
}

pub fn canonical_world_fingerprint(world: &World) -> Fingerprint {
    canonical_fingerprint(&canonical_world_bytes(world))
}

fn canonical_world_bytes(world: &World) -> Vec<u8> {
    let cells = world
        .cells()
        .iter()
        .enumerate()
        .filter_map(|(flat, &palette_index)| {
            let state = world
                .palette()
                .get(palette_index)
                .expect("a world cell must reference its palette");
            (state.kind != BlockKind::Air).then(|| canonical_cell(world, flat, state))
        })
        .collect();
    let canonical = CanonicalWorld {
        size: world.size(),
        cells,
    };
    serde_json::to_vec(&canonical).expect("a world must serialize")
}

fn canonical_cell(world: &World, flat: usize, state: &BlockState) -> CanonicalCell {
    let (x, y, z) = world.decode(flat);
    CanonicalCell {
        x,
        y,
        z,
        kind: block_kind_name(state.kind),
        facing: state.facing.map(facing_name),
        power: state.power,
        lit: state.lit,
        delay: state.delay,
        face: state.face.map(face_name),
    }
}

fn facing_name(facing: Facing) -> &'static str {
    match facing {
        Facing::North => "north",
        Facing::South => "south",
        Facing::East => "east",
        Facing::West => "west",
        Facing::Up => "up",
        Facing::Down => "down",
    }
}

fn face_name(face: Face) -> &'static str {
    match face {
        Face::Floor => "floor",
        Face::Wall => "wall",
        Face::Ceiling => "ceiling",
    }
}

fn block_kind_name(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Air => "air",
        BlockKind::Solid => "solid",
        BlockKind::Glass => "glass",
        BlockKind::Slab => "slab",
        BlockKind::RedstoneWire => "redstone-wire",
        BlockKind::Repeater => "repeater",
        BlockKind::Comparator => "comparator",
        BlockKind::Torch => "torch",
        BlockKind::WallTorch => "wall-torch",
        BlockKind::Lever => "lever",
        BlockKind::RedstoneBlock => "redstone-block",
        BlockKind::Lamp => "lamp",
        BlockKind::Piston => "piston",
        BlockKind::Button => "button",
        BlockKind::PressurePlate => "pressure-plate",
        BlockKind::WeightedPressurePlate => "weighted-pressure-plate",
        BlockKind::Observer => "observer",
        BlockKind::Target => "target",
        BlockKind::DaylightDetector => "daylight-detector",
        BlockKind::Other => "other",
    }
}

fn git_output(repository: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(git_compatible_path(repository))
        .args(arguments)
        .output()
        .map_err(|error| format!("could not invoke git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git -C {} {} failed: {}",
            repository.display(),
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|stdout| stdout.trim_end_matches(['\r', '\n']).to_string())
        .map_err(|error| format!("git returned non-UTF-8 output: {error}"))
}

#[cfg(windows)]
fn git_compatible_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(local) = text.strip_prefix(r"\\?\") {
        PathBuf::from(local)
    } else {
        path.to_path_buf()
    }
}

#[cfg(not(windows))]
fn git_compatible_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

fn canonical_existing_directory(path: &Path, description: &str) -> Result<PathBuf, String> {
    path.canonicalize().map_err(|error| {
        format!(
            "could not resolve {description} {}: {error}",
            path.display()
        )
    })
}

fn canonical_absent_path(path: &Path) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("capture output {} has no file name", path.display()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(canonical_existing_directory(parent, "capture output parent")?.join(file_name))
}

fn allowed_absent_output_status(repository: &Path, output: &Path) -> Option<String> {
    let relative = output.strip_prefix(repository).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    Some(format!(" D {relative}"))
}

pub fn verified_capture_commit(
    manifest_root: &Path,
    current_directory: &Path,
    requested_output: &Path,
) -> Result<String, String> {
    let repository = canonical_existing_directory(manifest_root, "CARGO_MANIFEST_DIR")?;
    let current_directory = canonical_existing_directory(current_directory, "current directory")?;
    if current_directory != repository {
        return Err(format!(
            "capture must run from the exact repository root {}; current directory is {}",
            repository.display(),
            current_directory.display()
        ));
    }
    if !git_output(&repository, &["rev-parse", "--show-prefix"])?.is_empty() {
        return Err(format!(
            "CARGO_MANIFEST_DIR {} is not the Git repository root",
            repository.display()
        ));
    }
    if requested_output.exists() {
        return Err(format!(
            "capture output {} must be absent before provenance verification",
            requested_output.display()
        ));
    }
    let requested_output = canonical_absent_path(requested_output)?;
    let allowed = allowed_absent_output_status(&repository, &requested_output);
    let verify_clean = || -> Result<(), String> {
        let status = git_output(
            &repository,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
        if status.is_empty() || allowed.as_deref() == Some(status.as_str()) {
            Ok(())
        } else {
            Err(format!(
                "capture requires a clean tracked/untracked repository; status was:\n{status}"
            ))
        }
    };
    verify_clean()?;
    let head = git_output(&repository, &["rev-parse", "HEAD"])?;
    if head.is_empty() {
        return Err("git returned an empty HEAD".to_string());
    }
    verify_clean()?;
    Ok(head)
}

pub fn current_git_commit(requested_output: &Path) -> Result<String, String> {
    let current_directory = std::env::current_dir()
        .map_err(|error| format!("could not read current directory: {error}"))?;
    verified_capture_commit(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &current_directory,
        requested_output,
    )
}

pub fn refuse_existing_output(path: &Path, replace: bool) -> Result<(), String> {
    if path.exists() && !replace {
        Err(format!(
            "{} already exists; pass --replace to overwrite it",
            path.display()
        ))
    } else {
        Ok(())
    }
}

pub fn write_baseline_json(
    path: &Path,
    baseline: &BenchmarkBaseline,
    replace: bool,
) -> Result<(), String> {
    refuse_existing_output(path, replace)?;
    stage_baseline_json(path, baseline)?.persist(replace)
}

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct StagedBaselineJson {
    temporary: PathBuf,
    destination: PathBuf,
    persisted: bool,
}

impl StagedBaselineJson {
    fn persist(mut self, replace: bool) -> Result<(), String> {
        atomic_publish(&self.temporary, &self.destination, replace).map_err(|error| {
            if !replace && self.destination.exists() {
                format!(
                    "{} already exists; refusing to overwrite raced destination: {error}",
                    self.destination.display()
                )
            } else {
                format!(
                    "could not atomically publish {}: {error}",
                    self.destination.display()
                )
            }
        })?;
        self.persisted = true;
        Ok(())
    }
}

impl Drop for StagedBaselineJson {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

fn stage_baseline_json(
    path: &Path,
    baseline: &BenchmarkBaseline,
) -> Result<StagedBaselineJson, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let mut bytes = serde_json::to_vec_pretty(baseline)
        .map_err(|error| format!("could not serialize baseline: {error}"))?;
    bytes.push(b'\n');
    stage_json_bytes(path, &bytes)
}

fn stage_json_bytes(path: &Path, bytes: &[u8]) -> Result<StagedBaselineJson, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("output {} has no UTF-8 file name", path.display()))?;
    for _ in 0..100 {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(mut file) => {
                if let Err(error) = write_and_sync(&mut file, bytes) {
                    drop(file);
                    let _ = std::fs::remove_file(&temporary);
                    return Err(format!("could not stage {}: {error}", path.display()));
                }
                drop(file);
                return Ok(StagedBaselineJson {
                    temporary,
                    destination: path.to_path_buf(),
                    persisted: false,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "could not stage {} in {}: {error}",
                    path.display(),
                    parent.display()
                ));
            }
        }
    }
    Err(format!(
        "could not allocate a same-directory temporary file for {}",
        path.display()
    ))
}

fn write_and_sync(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(windows)]
fn atomic_publish(source: &Path, destination: &Path, replace: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn atomic_publish(source: &Path, destination: &Path, replace: bool) -> std::io::Result<()> {
    if replace {
        std::fs::rename(source, destination)
    } else {
        std::fs::hard_link(source, destination)?;
        std::fs::remove_file(source)
    }
}

#[cfg(not(any(unix, windows)))]
fn atomic_publish(_source: &Path, _destination: &Path, _replace: bool) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic file publication is not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{
        canonical_netlist_bytes, canonical_netlist_fingerprint, canonical_pin_manifest_bytes,
        canonical_pin_manifest_fingerprint, canonical_world_bytes, canonical_world_fingerprint,
        check_outputs, drive, install_drivers, install_probes, legacy_benchmark_evaluator,
        physical_metrics, pinned_improvement_predicates, stage_baseline_json,
        validate_output_identities, verified_capture_commit, write_baseline_json,
        AcceptanceEvaluator, BenchmarkBaseline, BenchmarkCase, BenchmarkFixture, BenchmarkOutput,
        Simulator, MAX_TRANSITION_GAME_TICKS,
    };
    use crate::circuits::and4;
    use crate::compile::fragment_synth::allocation::{root_placement, RootAccess};
    use crate::compile::fragment_synth::manifest::TransitionManifest;
    use crate::compile::geometry::Anchor;
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::planner::PortPlacements;
    use crate::compile::revisions::{
        cell_library_revision, physical_verifier_revision, simulator_revision,
    };
    use crate::compile::topology::Library;
    use crate::compile::CompiledCircuit;
    use crate::compile::{compile_legacy, Gate, Netlist};
    use crate::redstone::world::block::{BlockKind, BlockState, Face, Facing};
    use crate::redstone::world::storage::World;

    #[test]
    fn literal_baseline_schema_preserves_case_order_and_certification_evidence() {
        let baseline: BenchmarkBaseline = serde_json::from_value(serde_json::json!({
            "baseline_commit": "0123456789abcdef",
            "build_profile": "release",
            "cargo_features": [],
            "cell_library_revision": "cell-library",
            "simulator_revision": "simulator",
            "verifier_revision": "verifier",
            "cases": [
                {
                    "name": "and4",
                    "transition_count": 240,
                    "lowered_netlist_hash": "and4-netlist",
                    "pin_manifest_hash": "and4-pins",
                    "transition_manifest_hash": "and4-transitions",
                    "generated_world_fingerprint": "and4-world",
                    "certified": true,
                    "physical": {
                        "non_air_blocks": 1,
                        "occupied_min": { "x": 0, "y": 0, "z": 0 },
                        "occupied_max": { "x": 0, "y": 0, "z": 0 },
                        "occupied_volume": 1,
                        "blocks_per_lowered_gate": { "numerator": 1, "denominator": 1 }
                    },
                    "max_observed_settle_game_ticks_on_manifest": 1
                },
                {
                    "name": "verilog:and4",
                    "transition_count": 240,
                    "lowered_netlist_hash": "verilog-and4-netlist",
                    "pin_manifest_hash": "verilog-and4-pins",
                    "transition_manifest_hash": "verilog-and4-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "full_adder",
                    "transition_count": 56,
                    "lowered_netlist_hash": "full-adder-netlist",
                    "pin_manifest_hash": "full-adder-pins",
                    "transition_manifest_hash": "full-adder-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "segment_a",
                    "transition_count": 240,
                    "lowered_netlist_hash": "segment-a-netlist",
                    "pin_manifest_hash": "segment-a-pins",
                    "transition_manifest_hash": "segment-a-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "seven_segment",
                    "transition_count": 240,
                    "lowered_netlist_hash": "seven-segment-netlist",
                    "pin_manifest_hash": "seven-segment-pins",
                    "transition_manifest_hash": "seven-segment-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                },
                {
                    "name": "pinned:verilog:seven_segment",
                    "transition_count": 240,
                    "lowered_netlist_hash": "pinned-seven-segment-netlist",
                    "pin_manifest_hash": "pinned-seven-segment-pins",
                    "transition_manifest_hash": "pinned-seven-segment-transitions",
                    "generated_world_fingerprint": null,
                    "certified": false,
                    "physical": null,
                    "max_observed_settle_game_ticks_on_manifest": null
                }
            ]
        }))
        .expect("the literal baseline schema deserialises");

        assert_eq!(
            baseline
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>(),
            [
                "and4",
                "verilog:and4",
                "full_adder",
                "segment_a",
                "seven_segment",
                "pinned:verilog:seven_segment",
            ]
        );

        for case in baseline.cases.iter().filter(|case| case.certified) {
            assert!(!baseline.baseline_commit.is_empty());
            assert!(!case.lowered_netlist_hash.as_str().is_empty());
            assert!(!case.pin_manifest_hash.as_str().is_empty());
            assert!(!case.transition_manifest_hash.as_str().is_empty());
            assert!(!case
                .generated_world_fingerprint
                .as_ref()
                .expect("a certified case records its world")
                .as_str()
                .is_empty());
            assert!(
                case.physical
                    .as_ref()
                    .expect("a certified case records physical metrics")
                    .non_air_blocks
                    > 0
            );
            assert!(case.max_observed_settle_game_ticks_on_manifest.is_some());
        }
        for case in &baseline.cases {
            let input_count = if case.name == "full_adder" { 3 } else { 4 };
            assert_eq!(
                case.transition_count,
                TransitionManifest::new(
                    (0..input_count).map(|index| format!("i{index}")).collect()
                )
                .transitions()
                .len()
            );
            assert!(case.transition_count > 0);
        }
    }

    #[test]
    fn pinned_new_coverage_leaves_improvement_predicates_unset() {
        assert_eq!(
            pinned_improvement_predicates(true, false, Some(100), Some(90), Some(100), Some(99)),
            (None, None)
        );
    }

    #[test]
    fn pinned_certified_null_baseline_fails_missing_improvement_metrics() {
        assert_eq!(
            pinned_improvement_predicates(true, true, None, Some(90), None, Some(99)),
            (Some(false), Some(false))
        );
    }

    #[test]
    fn pinned_numeric_baseline_enforces_both_improvements() {
        assert_eq!(
            pinned_improvement_predicates(true, true, Some(100), Some(90), Some(100), Some(99)),
            (Some(true), Some(true))
        );
        assert_eq!(
            pinned_improvement_predicates(true, true, Some(100), Some(91), Some(100), Some(100)),
            (Some(false), Some(false))
        );
    }

    fn tiny_fixture(expected: fn(&[bool]) -> Vec<bool>) -> BenchmarkFixture {
        BenchmarkFixture::new(
            "not",
            Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![Gate::nor("y", &["a"])],
            },
            vec!["a".into()],
            vec![BenchmarkOutput::new("y", "y")],
            expected,
            Default::default(),
        )
    }

    /// `evaluate_world` as it stood before the warm-up: drivers, probes and the
    /// initial settle repeated inside every transition.
    ///
    /// Test-only, and written from the same pieces, so what it proves is that
    /// doing that work once is the only difference.
    fn evaluate_world_cold(
        evaluator: &AcceptanceEvaluator,
        name: &str,
        compiled: &CompiledCircuit,
    ) -> Result<BenchmarkCase, String> {
        let fixture = evaluator
            .fixture(name)
            .ok_or_else(|| format!("unknown benchmark fixture `{name}`"))?;
        let manifest = fixture.transition_manifest();
        if manifest.transitions().is_empty() {
            return Err(format!("`{name}` has an empty transition manifest"));
        }
        let sinks = validate_output_identities(compiled, fixture)?;

        let mut worst = 0;
        for transition in manifest.transitions() {
            let mut world = compiled.world.clone();
            let drivers = install_drivers(&mut world, compiled, fixture)?;
            install_probes(&mut world, fixture)?;
            let mut simulator = Simulator::new(world);
            simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| format!("{} did not initially settle: {error:?}", fixture.name))?;

            drive(&mut simulator, &drivers, &transition.from);
            simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| {
                    format!(
                        "{} did not settle at from={:?}: {error:?}",
                        fixture.name, transition.from
                    )
                })?;
            check_outputs(fixture, simulator.world(), &sinks, &transition.from)?;

            drive(&mut simulator, &drivers, &transition.to);
            let ticks = simulator
                .run_until_stable(MAX_TRANSITION_GAME_TICKS)
                .map_err(|error| {
                    format!(
                        "{} did not settle at to={:?}: {error:?}",
                        fixture.name, transition.to
                    )
                })?;
            check_outputs(fixture, simulator.world(), &sinks, &transition.to)?;
            worst = worst.max(ticks);
        }

        let mut case = fixture.blank_case();
        case.generated_world_fingerprint = Some(canonical_world_fingerprint(&compiled.world));
        case.certified = true;
        case.physical = Some(physical_metrics(
            &compiled.world,
            fixture.lowered_netlist.gates.len() as u64,
        ));
        case.max_observed_settle_game_ticks_on_manifest = Some(worst);
        Ok(case)
    }

    #[test]
    fn warming_the_evaluator_once_certifies_exactly_what_repeating_it_did() {
        let tiny = AcceptanceEvaluator::new(vec![tiny_fixture(|bits| vec![!bits[0]])]);
        let tiny_compiled = compile_legacy(tiny.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");
        // One inverter settles in a handful of ticks and has one torch. `and4`
        // is the smallest baked fixture with a real startup: many torches
        // resolving over many ticks before any vector is driven, and a
        // four-input manifest that drives every ordered pair. If dropping the
        // initial settle's tick counter or its torch history could move a
        // measurement, it would move one here.
        let corpus = legacy_benchmark_evaluator().expect("the baked corpus builds");
        let and4_compiled = compile_legacy(corpus.fixture("and4").unwrap().lowered_netlist())
            .expect("the and4 fixture compiles");

        for (evaluator, name, compiled) in [
            (&tiny, "not", &tiny_compiled),
            (&corpus, "and4", &and4_compiled),
        ] {
            let cold = evaluate_world_cold(evaluator, name, compiled)
                .unwrap_or_else(|error| panic!("{name} cold path: {error}"));
            let warm = evaluator
                .evaluate_world(name, compiled)
                .unwrap_or_else(|error| panic!("{name} warm path: {error}"));

            // The whole case: worst settle ticks, both fingerprints, the
            // physical metrics and the certified flag, not a summary of them.
            assert_eq!(cold, warm, "{name}");
            assert!(
                cold.max_observed_settle_game_ticks_on_manifest.unwrap() > 0,
                "{name} never ticks, so the comparison proves nothing about the settle"
            );
            assert!(cold.transition_count > 1, "{name}");
        }
    }

    #[test]
    fn evaluator_certifies_real_truth_outputs_over_a_non_empty_manifest() {
        let evaluator = AcceptanceEvaluator::new(vec![tiny_fixture(|bits| vec![!bits[0]])]);
        let compiled = compile_legacy(evaluator.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");

        let case = evaluator
            .evaluate_world("not", &compiled)
            .expect("the real simulator certifies the inverter");

        assert!(case.certified);
        assert!(case.generated_world_fingerprint.is_some());
        assert!(case.physical.as_ref().unwrap().non_air_blocks > 0);
        assert!(case.max_observed_settle_game_ticks_on_manifest.is_some());
        assert_eq!(case.transition_count, 2);
        assert_eq!(
            TransitionManifest::new(vec!["a".into()])
                .transitions()
                .len(),
            2
        );
    }

    #[test]
    fn evaluator_refuses_a_world_whose_truth_output_is_wrong() {
        let evaluator = AcceptanceEvaluator::new(vec![tiny_fixture(|bits| vec![bits[0]])]);
        let compiled = compile_legacy(evaluator.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");

        let error = evaluator.evaluate_world("not", &compiled).unwrap_err();
        assert!(error.contains("expected"), "unexpected error: {error}");
    }

    #[test]
    fn evaluator_refuses_an_expectation_that_omits_a_truth_output() {
        let evaluator = AcceptanceEvaluator::new(vec![tiny_fixture(|_| Vec::new())]);
        let compiled = compile_legacy(evaluator.fixture("not").unwrap().lowered_netlist())
            .expect("the minimal fixture compiles");

        let error = evaluator.evaluate_world("not", &compiled).unwrap_err();
        assert!(
            error.contains("expected-output count"),
            "unexpected error: {error}"
        );
    }

    fn identity_error(
        mutate_fixture: impl FnOnce(&mut BenchmarkFixture),
        mutate_compiled: impl FnOnce(&mut crate::compile::CompiledCircuit),
    ) -> String {
        let mut fixture = tiny_fixture(|bits| vec![!bits[0]]);
        mutate_fixture(&mut fixture);
        let mut compiled = compile_legacy(fixture.lowered_netlist()).unwrap();
        mutate_compiled(&mut compiled);
        AcceptanceEvaluator::new(vec![fixture])
            .evaluate_world("not", &compiled)
            .unwrap_err()
    }

    #[test]
    fn evaluator_rejects_missing_compiled_output_before_truth_sweep() {
        let error = identity_error(|_| {}, |compiled| compiled.output_positions.clear());
        assert!(error.contains("missing compiled output"), "{error}");
    }

    #[test]
    fn evaluator_rejects_extra_compiled_output_before_truth_sweep() {
        let error = identity_error(
            |_| {},
            |compiled| {
                compiled.output_positions.insert("extra".into(), (0, 0, 0));
            },
        );
        assert!(error.contains("extra compiled output"), "{error}");
    }

    #[test]
    fn evaluator_rejects_duplicate_output_labels_before_truth_sweep() {
        let error = identity_error(
            |fixture| fixture.outputs.push(BenchmarkOutput::new("y", "other")),
            |_| {},
        );
        assert!(error.contains("duplicate output label"), "{error}");
    }

    #[test]
    fn evaluator_rejects_duplicate_output_signals_before_truth_sweep() {
        let error = identity_error(
            |fixture| fixture.outputs.push(BenchmarkOutput::new("other", "y")),
            |_| {},
        );
        assert!(error.contains("duplicate output signal"), "{error}");
    }

    #[test]
    fn evaluator_rejects_unresolved_output_provenance_before_truth_sweep() {
        let error = identity_error(
            |fixture| fixture.outputs[0] = BenchmarkOutput::new("", "y"),
            |_| {},
        );
        assert!(error.contains("unresolved output label"), "{error}");
    }

    #[test]
    fn evaluator_rejects_fixture_netlist_output_order_mismatch_before_truth_sweep() {
        let error = identity_error(
            |fixture| fixture.outputs[0] = BenchmarkOutput::new("y", "other"),
            |_| {},
        );
        assert!(error.contains("fixture output order"), "{error}");
    }

    #[test]
    fn canonical_netlist_bytes_pin_every_ordered_boundary() {
        let netlist = Netlist {
            inputs: vec!["left".into(), "right".into()],
            outputs: vec!["sum".into(), "carry".into()],
            gates: vec![
                Gate::nor("sum", &["left", "right"]),
                Gate::nor("carry", &["right"]),
            ],
        };
        assert_eq!(
            canonical_netlist_bytes(&netlist),
            br#"{"inputs":["left","right"],"outputs":["sum","carry"],"gates":[{"name":"sum","inputs":["left","right"],"output":"sum","kind":"nor","arity":2},{"name":"carry","inputs":["right"],"output":"carry","kind":"nor","arity":1}]}"#
        );

        let original = canonical_netlist_bytes(&netlist);
        let mut swapped_inputs = netlist.clone();
        swapped_inputs.inputs.swap(0, 1);
        assert_ne!(canonical_netlist_bytes(&swapped_inputs), original);

        let mut swapped_outputs = netlist.clone();
        swapped_outputs.outputs.swap(0, 1);
        assert_ne!(canonical_netlist_bytes(&swapped_outputs), original);

        let mut swapped_gates = netlist.clone();
        swapped_gates.gates.swap(0, 1);
        assert_ne!(canonical_netlist_bytes(&swapped_gates), original);

        let mut swapped_gate_inputs = netlist.clone();
        swapped_gate_inputs.gates[0].inputs.swap(0, 1);
        assert_ne!(canonical_netlist_bytes(&swapped_gate_inputs), original);
    }

    fn two_pin_manifest(
        first_role: &str,
        first_label: &str,
        first_at: Anchor,
        first_toward: Facing,
    ) -> PortPlacements {
        let mut pins = PortPlacements::default();
        pins.pin("output:sum", Anchor { x: 37, y: 8, z: -5 }, Facing::North);
        pins.pin(
            format!("{first_role}:{first_label}"),
            first_at,
            first_toward,
        );
        pins
    }

    #[test]
    fn canonical_pin_bytes_pin_identity_role_geometry_and_map_order() {
        let input_at = Anchor {
            x: -11,
            y: 4,
            z: 29,
        };
        let pins = two_pin_manifest("input", "left", input_at, Facing::East);
        assert_eq!(
            canonical_pin_manifest_bytes(&pins),
            br#"[{"port":"input:left","x":-11,"y":4,"z":29,"toward":"east"},{"port":"output:sum","x":37,"y":8,"z":-5,"toward":"north"}]"#
        );

        let mut reverse = PortPlacements::default();
        reverse.pin("input:left", input_at, Facing::East);
        reverse.pin("output:sum", Anchor { x: 37, y: 8, z: -5 }, Facing::North);
        assert_eq!(
            canonical_pin_manifest_bytes(&pins),
            canonical_pin_manifest_bytes(&reverse)
        );

        let original = canonical_pin_manifest_bytes(&pins);
        let mutations = [
            two_pin_manifest("input", "left", Anchor { x: -10, ..input_at }, Facing::East),
            two_pin_manifest("input", "left", Anchor { y: 5, ..input_at }, Facing::East),
            two_pin_manifest("input", "left", Anchor { z: 30, ..input_at }, Facing::East),
            two_pin_manifest("input", "left", input_at, Facing::West),
            two_pin_manifest("output", "left", input_at, Facing::East),
            two_pin_manifest("input", "other", input_at, Facing::East),
        ];
        for mutation in mutations {
            assert_ne!(canonical_pin_manifest_bytes(&mutation), original);
        }
    }

    fn canonical_world_fixture(size: (i32, i32, i32), palette_noise: bool) -> World {
        let mut world = World::new(size.0, size.1, size.2);
        if palette_noise {
            let mut temporary = BlockState::air();
            temporary.kind = BlockKind::Solid;
            temporary.name = "minecraft:stone".into();
            world.set(3, 3, 4, temporary);
            world.set(3, 3, 4, BlockState::air());
        }

        let mut comparator = BlockState::air();
        comparator.kind = BlockKind::Comparator;
        comparator.name = "minecraft:comparator".into();
        comparator.facing = Some(Facing::North);
        comparator.power = 12;
        comparator.delay = 2;
        comparator.face = Some(Face::Ceiling);

        let mut lever = BlockState::air();
        lever.kind = BlockKind::Lever;
        lever.name = "minecraft:lever".into();
        lever.facing = Some(Facing::South);
        lever.lit = true;
        lever.face = Some(Face::Wall);

        let mut repeater = BlockState::air();
        repeater.kind = BlockKind::Repeater;
        repeater.name = "minecraft:repeater".into();
        repeater.facing = Some(Facing::East);
        repeater.power = 7;
        repeater.lit = true;
        repeater.delay = 3;
        repeater.face = Some(Face::Floor);
        if palette_noise {
            world.set(2, 0, 1, repeater);
            world.set(1, 0, 3, lever);
            world.set(0, 2, 0, comparator);
        } else {
            world.set(0, 2, 0, comparator);
            world.set(1, 0, 3, lever);
            world.set(2, 0, 1, repeater);
        }
        world
    }

    #[test]
    fn canonical_world_bytes_pin_yzx_order_size_and_every_electrical_field() {
        let world = canonical_world_fixture((4, 4, 5), false);
        assert_eq!(
            canonical_world_bytes(&world),
            br#"{"size":[4,4,5],"cells":[{"x":2,"y":0,"z":1,"kind":"repeater","facing":"east","power":7,"lit":true,"delay":3,"face":"floor"},{"x":1,"y":0,"z":3,"kind":"lever","facing":"south","power":0,"lit":true,"delay":0,"face":"wall"},{"x":0,"y":2,"z":0,"kind":"comparator","facing":"north","power":12,"lit":false,"delay":2,"face":"ceiling"}]}"#
        );

        let same_cells_different_palette_history = canonical_world_fixture((4, 4, 5), true);
        assert_eq!(
            canonical_world_bytes(&world),
            canonical_world_bytes(&same_cells_different_palette_history)
        );

        let original = canonical_world_bytes(&world);
        for size in [(5, 4, 5), (4, 5, 5), (4, 4, 6)] {
            assert_ne!(
                canonical_world_bytes(&canonical_world_fixture(size, false)),
                original
            );
        }

        let original_state = world.get(2, 0, 1).clone();
        let mutations = [
            BlockState {
                kind: BlockKind::Comparator,
                ..original_state.clone()
            },
            BlockState {
                facing: Some(Facing::West),
                ..original_state.clone()
            },
            BlockState {
                power: 8,
                ..original_state.clone()
            },
            BlockState {
                lit: false,
                ..original_state.clone()
            },
            BlockState {
                delay: 4,
                ..original_state.clone()
            },
            BlockState {
                face: Some(Face::Ceiling),
                ..original_state
            },
        ];
        for mutation in mutations {
            let mut changed = world.clone();
            changed.set(2, 0, 1, mutation);
            assert_ne!(canonical_world_bytes(&changed), original);
        }
    }

    #[test]
    fn checked_fixture_fingerprints_remain_byte_compatible() {
        let (netlist, _) = and4::build_and4_netlist();
        assert_eq!(
            canonical_netlist_fingerprint(&netlist).as_str(),
            "48690daa56e47fa93724683adaf4bc221892d0ebee4540401d7bb2073cf2c930"
        );
        assert_eq!(
            canonical_pin_manifest_fingerprint(&PortPlacements::default()).as_str(),
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
        );

        let compiled = compile_legacy(&netlist).expect("checked and4 netlist compiles");
        assert_eq!(
            canonical_world_fingerprint(&compiled.world).as_str(),
            "91a9608cab628fbf17bdc1c655a3a708793eb2bb82af51703fc7c0c03fb6a23c"
        );
    }

    #[test]
    fn verified_capture_requires_exact_clean_repository_root_and_uses_its_head() {
        let repository = temporary_directory("provenance");
        run_git(&repository, &["init"]);
        run_git(
            &repository,
            &["config", "user.email", "reda@example.invalid"],
        );
        run_git(&repository, &["config", "user.name", "REDA Test"]);
        std::fs::write(repository.join("tracked"), b"clean").unwrap();
        run_git(&repository, &["add", "tracked"]);
        run_git(&repository, &["commit", "-m", "fixture"]);
        let head = git_stdout(&repository, &["rev-parse", "HEAD"]);
        let requested = repository.join("capture.json");

        assert_eq!(
            verified_capture_commit(&repository, &repository, &requested).unwrap(),
            head
        );
        let subdirectory = repository.join("subdir");
        std::fs::create_dir(&subdirectory).unwrap();
        assert!(
            verified_capture_commit(&repository, &subdirectory, &requested)
                .unwrap_err()
                .contains("repository root")
        );

        std::fs::write(repository.join("untracked"), b"dirty").unwrap();
        assert!(
            verified_capture_commit(&repository, &repository, &requested)
                .unwrap_err()
                .contains("clean")
        );
        std::fs::remove_file(repository.join("untracked")).unwrap();

        std::fs::write(repository.join("tracked"), b"dirty tracked content").unwrap();
        assert!(
            verified_capture_commit(&repository, &repository, &requested)
                .unwrap_err()
                .contains("clean")
        );
        std::fs::write(repository.join("tracked"), b"clean").unwrap();

        std::fs::write(&requested, b"tracked old fixture").unwrap();
        run_git(&repository, &["add", "capture.json"]);
        run_git(&repository, &["commit", "-m", "old capture"]);
        std::fs::remove_file(&requested).unwrap();
        let new_head = git_stdout(&repository, &["rev-parse", "HEAD"]);
        assert_eq!(
            verified_capture_commit(&repository, &repository, &requested).unwrap(),
            new_head
        );

        std::fs::remove_dir_all(repository).unwrap();
    }

    fn literal_empty_baseline() -> BenchmarkBaseline {
        BenchmarkBaseline {
            baseline_commit: "commit".into(),
            build_profile: "test".into(),
            cargo_features: Vec::new(),
            cell_library_revision: canonical_fingerprint(b"cell"),
            simulator_revision: canonical_fingerprint(b"sim"),
            verifier_revision: canonical_fingerprint(b"verify"),
            cases: Vec::new(),
        }
    }

    fn temporary_output(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reda-fragment-baseline-{name}-{}.json",
            std::process::id()
        ))
    }

    fn temporary_directory(name: &str) -> PathBuf {
        let path = temporary_output(name).with_extension("dir");
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap();
        path
    }

    fn run_git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    #[test]
    fn baseline_writer_refuses_overwrite_until_replace_is_explicit() {
        let output = temporary_output("overwrite");
        let _ = std::fs::remove_file(&output);
        std::fs::write(&output, b"keep me").unwrap();

        let error = write_baseline_json(&output, &literal_empty_baseline(), false).unwrap_err();
        assert!(
            error.contains("already exists"),
            "unexpected error: {error}"
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"keep me");

        write_baseline_json(&output, &literal_empty_baseline(), true).unwrap();
        let written: BenchmarkBaseline =
            serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
        assert_eq!(written.baseline_commit, "commit");
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn staged_no_clobber_publish_cannot_overwrite_a_raced_destination() {
        let output = temporary_output("race");
        let _ = std::fs::remove_file(&output);
        let staged = stage_baseline_json(&output, &literal_empty_baseline()).unwrap();
        std::fs::write(&output, b"raced winner").unwrap();

        let error = staged.persist(false).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(std::fs::read(&output).unwrap(), b"raced winner");
        std::fs::remove_file(output).unwrap();
    }

    #[test]
    fn dropped_staged_write_never_leaves_a_partial_final_file() {
        let output = temporary_output("interrupted");
        let _ = std::fs::remove_file(&output);
        let staged = stage_baseline_json(&output, &literal_empty_baseline()).unwrap();
        drop(staged);
        assert!(!output.exists());
    }

    #[test]
    fn baseline_header_reads_only_task_zero_revision_authorities() {
        let baseline = AcceptanceEvaluator::new(Vec::new()).baseline("commit-from-git".into());
        let library = Library::default_library();

        assert_eq!(baseline.baseline_commit, "commit-from-git");
        assert_eq!(
            baseline.cell_library_revision,
            cell_library_revision(&library)
        );
        assert_eq!(baseline.simulator_revision, simulator_revision());
        assert_eq!(baseline.verifier_revision, physical_verifier_revision());
    }

    /// The glyph is the case the caller-row contract could never hold: seven
    /// outputs on five rows facing four ways, and inputs whose handover is in
    /// front of them rather than behind.  It lands, and every pin stays exactly
    /// where the caller put it.
    #[test]
    fn the_pinned_glyph_lands_and_keeps_every_pin() {
        let evaluator = super::legacy_benchmark_evaluator().unwrap();
        let fixture = evaluator.fixture("pinned:verilog:seven_segment").unwrap();
        let pins = fixture.placements();
        let placement = root_placement(fixture.lowered_netlist(), Some(pins)).unwrap();
        let RootAccess::Landed { region } = placement.access else {
            panic!("the glyph cannot fold onto one caller row");
        };
        // The body starts behind every cell a pin reaches, and the access
        // region joins the pins to it.
        assert!(region.max.z <= placement.caller_row_z);
        for (signal, pin) in pins.iter() {
            assert!(
                pin.at.z <= placement.caller_row_z,
                "{signal} at {:?} is not in front of the body",
                pin.at
            );
            assert!(region.contains(pin.at), "{signal} is outside the access");
        }
        for (_, at, toward) in super::pinned_glyph() {
            let held = pins
                .iter()
                .find(|(_, pin)| pin.at == at)
                .expect("every glyph cell is still pinned");
            assert_eq!(held.1.toward, toward);
        }
    }
}
