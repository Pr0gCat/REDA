use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::certification::{CandidateMetrics, CompleteCandidateCertifier};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::fragment::FragmentProposalStream;
use crate::compile::fragment_synth::manifest::TransitionManifest;
use crate::compile::fragment_synth::search::{
    run_budgeted_proposals, ProposalTrace, StopReason, SynthesisBudget, SystemMonotonicClock,
};
use crate::compile::fragment_synth::seed::{
    compile_sparse_seed_with_services, SeedInput, SeedServices,
};
use crate::compile::fragment_synth::services::{
    DurableSeedEmitter, DurableSeedVerifier, TopologyAwareSeedPlacer,
};
use crate::compile::geometry::Anchor;
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::planner::{PortPin, PortPlacements};
use crate::compile::revisions::{
    cell_library_revision, expanded_physical_verifier_revision, simulator_revision,
};
use crate::compile::routing::DurablePhysicalRouter;
use crate::compile::topology::{GateKind, Library};
use crate::compile::{CircuitObservations, CompiledCircuit, Netlist, PlannerKind};
use crate::redstone::world::block::Facing;

#[derive(Clone, Copy)]
pub struct SynthesisInput<'a> {
    pub lowered: &'a Netlist,
    pub source_provenance: Option<&'a [usize]>,
    pub pins: Option<&'a PortPlacements>,
}

impl<'a> From<&SynthesisInput<'a>> for SeedInput<'a> {
    fn from(input: &SynthesisInput<'a>) -> Self {
        Self {
            lowered: input.lowered,
            source_provenance: input.source_provenance,
            pins: input.pins,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SynthesisCaseFingerprint(Fingerprint);

impl SynthesisCaseFingerprint {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Error)]
pub enum SynthesisError {
    #[error("independent seed construction failed: {0}")]
    Seed(String),
    #[error("certified candidate cannot expose compatibility metadata: {0}")]
    Compatibility(String),
}

pub struct SynthesisResult {
    pub compiled: CompiledCircuit,
    pub metrics: CandidateMetrics,
    pub trace: Vec<ProposalTrace>,
    pub evaluations_used: u64,
    pub case_fingerprint: SynthesisCaseFingerprint,
    pub candidate_fingerprint: Fingerprint,
    pub stop_reason: StopReason,
}

pub fn compile_fragment_synth(
    input: SynthesisInput<'_>,
    budget: SynthesisBudget,
) -> Result<SynthesisResult, SynthesisError> {
    let search_config = SearchConfig::checked_defaults();
    compile_fragment_synth_with_config(input, budget, &search_config)
}

fn compile_fragment_synth_with_config(
    input: SynthesisInput<'_>,
    budget: SynthesisBudget,
    search_config: &SearchConfig,
) -> Result<SynthesisResult, SynthesisError> {
    let library = Library::default_library();
    let certification_config = CertificationConfig::from_search(search_config);
    let case_fingerprint =
        synthesis_case_fingerprint(&input, search_config, &certification_config, &library);

    let seed_input = SeedInput::from(&input);
    let seed_services = SeedServices {
        library: &library,
        placer: &TopologyAwareSeedPlacer,
        router: &DurablePhysicalRouter,
        emitter: &DurableSeedEmitter,
        verifier: &DurableSeedVerifier,
        certifier: &CompleteCandidateCertifier,
        search_config,
    };
    let certified = compile_sparse_seed_with_services(seed_input, seed_services)
        .map_err(|error| SynthesisError::Seed(error.to_string()))?;

    let clock = SystemMonotonicClock::start();
    let mut proposals = FragmentProposalStream::new(seed_input, seed_services);
    let summary = run_budgeted_proposals(certified, budget, &clock, &mut proposals);

    let compiled = compiled_from_certified(&summary.best, input.lowered)?;
    let metrics = summary.best.metrics().clone();
    let candidate_fingerprint = metrics.candidate_fingerprint.clone();
    Ok(SynthesisResult {
        compiled,
        metrics,
        trace: summary.trace,
        evaluations_used: summary.evaluations_used,
        case_fingerprint,
        candidate_fingerprint,
        stop_reason: summary.stop_reason,
    })
}

fn compiled_from_certified(
    certified: &crate::compile::fragment_synth::certification::CertifiedCandidate,
    lowered: &Netlist,
) -> Result<CompiledCircuit, SynthesisError> {
    let views = certified
        .candidate()
        .compatibility_views(lowered)
        .map_err(|error| SynthesisError::Compatibility(error.to_string()))?;
    Ok(CompiledCircuit {
        world: certified.world().clone(),
        input_positions: views.input_positions,
        output_positions: views.output_positions,
        gate_output_positions: views.gate_output_positions,
        gate_facings: views.gate_facings,
        observations: CircuitObservations::from_expanded(certified.candidate()),
        legacy_emission: None,
        planner_kind: PlannerKind::FragmentSynth,
    })
}

#[derive(Serialize)]
struct CaseDescriptor<'a> {
    schema_version: u64,
    netlist: NetlistDescriptor<'a>,
    source_provenance: Option<&'a [usize]>,
    pins: Vec<PinDescriptor<'a>>,
    library_revision: Fingerprint,
    placement_revision: Fingerprint,
    search_config: &'a SearchConfig,
    certification_config: &'a CertificationConfig,
    simulator_revision: Fingerprint,
    verifier_revision: Fingerprint,
    manifest_fingerprint: Fingerprint,
}

#[derive(Serialize)]
struct NetlistDescriptor<'a> {
    inputs: &'a [String],
    outputs: &'a [String],
    gates: Vec<GateDescriptor<'a>>,
}

#[derive(Serialize)]
struct GateDescriptor<'a> {
    name: &'a str,
    inputs: &'a [String],
    output: &'a str,
    kind: GateKind,
}

#[derive(Serialize)]
struct PinDescriptor<'a> {
    name: &'a str,
    at: Anchor,
    toward: u8,
}

pub(crate) fn synthesis_case_fingerprint(
    input: &SynthesisInput<'_>,
    search_config: &SearchConfig,
    certification_config: &CertificationConfig,
    library: &Library,
) -> SynthesisCaseFingerprint {
    synthesis_case_fingerprint_with_placement_revision(
        input,
        search_config,
        certification_config,
        library,
        topology_aware_seed_placement_revision(),
    )
}

fn topology_aware_seed_placement_revision() -> Fingerprint {
    canonical_fingerprint(b"topology-aware-seed-v2")
}

fn synthesis_case_fingerprint_with_placement_revision(
    input: &SynthesisInput<'_>,
    search_config: &SearchConfig,
    certification_config: &CertificationConfig,
    library: &Library,
    placement_revision: Fingerprint,
) -> SynthesisCaseFingerprint {
    let gates = input
        .lowered
        .gates
        .iter()
        .map(|gate| GateDescriptor {
            name: &gate.name,
            inputs: &gate.inputs,
            output: &gate.output,
            kind: gate.kind,
        })
        .collect();
    let pins = input
        .pins
        .into_iter()
        .flat_map(|placements| placements.iter())
        .map(|(name, pin)| pin_descriptor(name, *pin))
        .collect();
    let manifest = TransitionManifest::for_kind(
        input.lowered.inputs.clone(),
        certification_config.transition_manifest_kind,
    );
    let descriptor = CaseDescriptor {
        schema_version: 1,
        netlist: NetlistDescriptor {
            inputs: &input.lowered.inputs,
            outputs: &input.lowered.outputs,
            gates,
        },
        source_provenance: input.source_provenance,
        pins,
        library_revision: cell_library_revision(library),
        placement_revision,
        search_config,
        certification_config,
        simulator_revision: simulator_revision(),
        verifier_revision: expanded_physical_verifier_revision(),
        manifest_fingerprint: manifest.fingerprint(),
    };
    SynthesisCaseFingerprint(canonical_fingerprint(
        &serde_json::to_vec(&descriptor).expect("synthesis case descriptor must serialize"),
    ))
}

fn pin_descriptor(name: &str, pin: PortPin) -> PinDescriptor<'_> {
    PinDescriptor {
        name,
        at: pin.at,
        toward: match pin.toward {
            Facing::North => 0,
            Facing::South => 1,
            Facing::East => 2,
            Facing::West => 3,
            Facing::Up => 4,
            Facing::Down => 5,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compile_fragment_synth_with_config, synthesis_case_fingerprint,
        synthesis_case_fingerprint_with_placement_revision, SynthesisInput,
    };
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
    use crate::compile::topology::Library;
    use crate::compile::{Gate, Netlist};

    #[test]
    fn complete_syntheses_at_larger_evaluation_budgets_extend_one_trace_prefix() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let input = || SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let config = SearchConfig::checked_defaults();
        let complete = compile_fragment_synth_with_config(
            input(),
            crate::compile::fragment_synth::search::SynthesisBudget::Evaluations(8),
            &config,
        )
        .unwrap();

        for budget in [0, 1, 2, 4, 8] {
            let result = compile_fragment_synth_with_config(
                input(),
                crate::compile::fragment_synth::search::SynthesisBudget::Evaluations(budget),
                &config,
            )
            .unwrap();
            assert_eq!(result.evaluations_used, budget);
            assert_eq!(result.trace, complete.trace[..budget as usize]);
            assert_eq!(result.case_fingerprint, complete.case_fingerprint);
            assert!(complete.metrics.quality <= result.metrics.quality);
        }
    }

    #[test]
    fn the_first_budgeted_proposal_is_a_real_certified_fragment_transaction() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let config = SearchConfig::checked_defaults();
        let result = compile_fragment_synth_with_config(
            SynthesisInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            crate::compile::fragment_synth::search::SynthesisBudget::Evaluations(1),
            &config,
        )
        .unwrap();

        assert!(matches!(
            result.trace[0].terminal,
            crate::compile::fragment_synth::search::ProposalTerminal::NoImprovement
                | crate::compile::fragment_synth::search::ProposalTerminal::Accepted
        ));
    }

    #[test]
    fn every_internal_cap_changes_the_case_before_traces_can_be_compared() {
        let (netlist, _) = build_and4_netlist();
        let input = SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let library = Library::default_library();
        let base = SearchConfig::checked_defaults();
        let base_fingerprint = synthesis_case_fingerprint(
            &input,
            &base,
            &CertificationConfig::from_search(&base),
            &library,
        );
        let mut variants = Vec::new();

        let mut changed = base.clone();
        changed.router_limits.max_node_expansions += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.router_limits.max_queue_entries += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_seed_shell_radius += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_fragment_shell_radius += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_seed_backtracks += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_fragment_backtracks_per_proposal += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_equivalence_proof_steps += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_certification_transitions += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_simulator_events_per_transition += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_game_ticks_per_transition += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.fragment_instance_schedule.push(16);
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_boundary_nets += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.max_fragment_manhattan_radius += 1;
        variants.push(changed);

        assert_eq!(variants.len(), 13);
        for changed in variants {
            assert_ne!(
                synthesis_case_fingerprint(
                    &input,
                    &changed,
                    &CertificationConfig::from_search(&changed),
                    &library,
                ),
                base_fingerprint
            );
        }
    }

    #[test]
    fn placement_revision_changes_only_the_case_fingerprint() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let input = SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&config);
        let old_revision = crate::compile::metrics::canonical_fingerprint(b"seed-v1");
        let new_revision =
            crate::compile::metrics::canonical_fingerprint(b"topology-aware-seed-v2");

        let old_case = synthesis_case_fingerprint_with_placement_revision(
            &input,
            &config,
            &certification,
            &library,
            old_revision,
        );
        let new_case = synthesis_case_fingerprint_with_placement_revision(
            &input,
            &config,
            &certification,
            &library,
            new_revision,
        );
        assert_ne!(old_case, new_case);

        let first = compile_fragment_synth_with_config(
            input,
            crate::compile::fragment_synth::search::SynthesisBudget::Evaluations(0),
            &config,
        )
        .unwrap();
        let repeated = compile_fragment_synth_with_config(
            input,
            crate::compile::fragment_synth::search::SynthesisBudget::Evaluations(0),
            &config,
        )
        .unwrap();
        assert_eq!(first.candidate_fingerprint, repeated.candidate_fingerprint);
    }
}
