use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::attribution::RecursiveDiagnostics;
use crate::compile::fragment_synth::certification::CandidateMetrics;
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::manifest::TransitionManifest;
use crate::compile::fragment_synth::partition::canonical_order;
use crate::compile::fragment_synth::recursive;
use crate::compile::fragment_synth::search::{ProposalTrace, StopReason, SynthesisBudget};
use crate::compile::geometry::Anchor;
use crate::compile::metrics::{canonical_fingerprint, Fingerprint};
use crate::compile::planner::{PortPin, PortPlacements};
use crate::compile::revisions::{
    cell_library_revision, expanded_physical_verifier_revision, simulator_revision,
};
use crate::compile::topology::{GateKind, Library};
use crate::compile::{CircuitObservations, CompiledCircuit, Netlist, PlannerKind};
use crate::redstone::world::block::Facing;

#[derive(Clone, Copy)]
pub struct SynthesisInput<'a> {
    pub lowered: &'a Netlist,
    pub source_provenance: Option<&'a [usize]>,
    pub pins: Option<&'a PortPlacements>,
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
    #[error("invalid synthesis netlist: {0}")]
    InvalidNetlist(String),
    #[error("recursive contract synthesis failed: {0}")]
    RecursiveContract(String),
}

/// Which synthesis path a case is compiled by.
///
/// Part of the case identity, not an implementation detail: the recursive
/// contract and the legacy whole-circuit seed lay out different circuits from
/// the same netlist, so the case fingerprint names its path. Production has
/// exactly one path and can name no other. The `Seed` variant exists only
/// under `cfg(test)`, for the unit tests that still exercise the legacy seed's
/// proposal loop through [`tests::compile_legacy_whole_circuit_seed`] -- so
/// there is, by construction, no production value that could route a case to
/// the seed or ship a seed circuit under a fingerprint of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum SynthesisPath {
    RecursiveContract,
    #[cfg(test)]
    Seed,
}

pub struct SynthesisResult {
    pub compiled: CompiledCircuit,
    pub metrics: CandidateMetrics,
    /// One entry per proposal the search evaluated. The production path is
    /// the recursive contract, which runs no proposal loop, so this is always
    /// empty from [`compile_fragment_synth`]; the type is kept because the
    /// field is public and the budgeted search that fills it still exists.
    pub trace: Vec<ProposalTrace>,
    /// Proposals the search spent. Always zero from [`compile_fragment_synth`],
    /// for the same reason `trace` is empty.
    pub evaluations_used: u64,
    pub case_fingerprint: SynthesisCaseFingerprint,
    pub candidate_fingerprint: Fingerprint,
    /// Why the search stopped. From [`compile_fragment_synth`] this only ever
    /// names the kind of budget the caller passed, since the one circuit is
    /// built and returned whatever the budget was.
    pub stop_reason: StopReason,
    /// What the recursive producer built, when its shape records it: the
    /// leaves it certified and the root trunks it laid. `None` for a direct
    /// leaf or an allocating root. Read-only; a measurement checks itself
    /// against it, the producer never reads it.
    pub recursive_diagnostics: Option<RecursiveDiagnostics>,
}

/// The public entry: one producer, the recursive contract, at every budget.
///
/// There is no selector in front of this and no fallback behind it. The case
/// fingerprint is computed for the recursive contract path and the recursive
/// producer is the only thing then asked to compile; a refusal is returned as
/// [`SynthesisError::RecursiveContract`], never swallowed into another run
/// that would ship a different circuit under this fingerprint.
///
/// **The budget buys nothing on this path.** The recursive contract has no
/// proposal loop: it builds one certified circuit and returns it, so every
/// budget yields the same world, `evaluations_used` is zero and `trace` is
/// empty. A budget is still not ignored -- it is what the reported
/// [`StopReason`] names -- but a caller that raises it gets the same answer
/// sooner rather than a better one.
///
/// The recursive path decides its own shape from the request: a root at or
/// below its direct-leaf grain is planned in one piece, an unpinned root above
/// it is packed, and a pinned root above it is allocated and composed. None of
/// that is visible here.
pub fn compile_fragment_synth(
    input: SynthesisInput<'_>,
    budget: SynthesisBudget,
) -> Result<SynthesisResult, SynthesisError> {
    let search_config = SearchConfig::checked_defaults();
    let library = Library::default_library();
    let certification_config = CertificationConfig::from_search(&search_config);
    let case_fingerprint =
        synthesis_case_fingerprint(&input, &search_config, &certification_config, &library)?;
    compile_recursive_contract(input, budget, &search_config, case_fingerprint)
}

/// Compile `input` on the recursive contract under an already-computed case
/// fingerprint.
///
/// `case_fingerprint` must have been computed for
/// [`SynthesisPath::RecursiveContract`]; both production callers of this
/// function are the ones that compute it that way. This is the only producer
/// call in production.
fn compile_recursive_contract(
    input: SynthesisInput<'_>,
    budget: SynthesisBudget,
    search_config: &SearchConfig,
    case_fingerprint: SynthesisCaseFingerprint,
) -> Result<SynthesisResult, SynthesisError> {
    let product = recursive::compile(input.lowered, input.pins, search_config)
        .map_err(|error| SynthesisError::RecursiveContract(error.to_string()))?;
    // This path has no proposal loop, so it never exhausts a budget: it
    // builds one certified circuit and stops. What it can still report
    // honestly is *which* budget the caller asked under, so a reader is
    // not told a time-budgeted call ran out of evaluations.
    let stop_reason = match budget {
        SynthesisBudget::Evaluations(_) => StopReason::EvaluationBudget,
        SynthesisBudget::Time(_) => StopReason::TimeBudget,
    };
    let compiled = CompiledCircuit {
        world: product.world,
        input_positions: product.input_positions,
        output_positions: product.output_positions,
        gate_output_positions: product.gate_output_positions,
        gate_facings: product.gate_facings,
        observations: CircuitObservations::default(),
        legacy_emission: None,
        planner_kind: PlannerKind::FragmentSynth,
    };
    Ok(SynthesisResult {
        compiled,
        metrics: product.metrics,
        trace: Vec::new(),
        evaluations_used: 0,
        case_fingerprint,
        candidate_fingerprint: product.candidate_fingerprint,
        stop_reason,
        recursive_diagnostics: product.diagnostics,
    })
}

#[derive(Serialize)]
struct CaseDescriptor<'a> {
    schema_version: u64,
    synthesis_path: SynthesisPath,
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
    /// Which recursive generator built the case, and `None` when the case is
    /// not built by one.
    ///
    /// Path-sensitive on purpose: a seed case's identity must not move when
    /// the recursive producer is revised, because nothing about the circuit
    /// it ships did. Still typed `Option` although production only ever writes
    /// `Some`: the serialised shape is the case identity, and narrowing the
    /// type would not change a single shipping fingerprint.
    recursive_producer_revision: Option<Fingerprint>,
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

/// The case fingerprint every public call ships under: the recursive contract
/// path, at the shipping placement and producer revisions.
fn synthesis_case_fingerprint(
    input: &SynthesisInput<'_>,
    search_config: &SearchConfig,
    certification_config: &CertificationConfig,
    library: &Library,
) -> Result<SynthesisCaseFingerprint, SynthesisError> {
    synthesis_case_fingerprint_with_revisions(
        input,
        SynthesisPath::RecursiveContract,
        search_config,
        certification_config,
        library,
        topology_aware_seed_placement_revision(),
        recursive::producer_revision(),
    )
}

fn topology_aware_seed_placement_revision() -> Fingerprint {
    canonical_fingerprint(b"topology-aware-seed-v2")
}

fn synthesis_case_fingerprint_with_revisions(
    input: &SynthesisInput<'_>,
    path: SynthesisPath,
    search_config: &SearchConfig,
    certification_config: &CertificationConfig,
    library: &Library,
    placement_revision: Fingerprint,
    recursive_producer_revision: Fingerprint,
) -> Result<SynthesisCaseFingerprint, SynthesisError> {
    let gates = canonical_order(input.lowered)
        .map_err(|error| SynthesisError::InvalidNetlist(error.to_string()))?
        .into_iter()
        .map(|index| {
            let gate = &input.lowered.gates[index];
            GateDescriptor {
                name: &gate.name,
                inputs: &gate.inputs,
                output: &gate.output,
                kind: gate.kind,
            }
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
        // Bumped with the `recursive_producer_revision` field below: the
        // descriptor's shape changed, so every identity under the old shape
        // is a different case by construction. Deleting the seed fallback did
        // not move it: the shipping descriptor is byte-for-byte what it was.
        schema_version: 3,
        synthesis_path: path,
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
        recursive_producer_revision: match path {
            SynthesisPath::RecursiveContract => Some(recursive_producer_revision),
            #[cfg(test)]
            SynthesisPath::Seed => None,
        },
    };
    Ok(SynthesisCaseFingerprint(canonical_fingerprint(
        &serde_json::to_vec(&descriptor).expect("synthesis case descriptor must serialize"),
    )))
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
        compile_recursive_contract, synthesis_case_fingerprint,
        synthesis_case_fingerprint_with_revisions, topology_aware_seed_placement_revision,
        StopReason, SynthesisBudget, SynthesisError, SynthesisInput, SynthesisPath,
        SynthesisResult,
    };
    use std::time::Duration;

    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::fragment_synth::certification::CompleteCandidateCertifier;
    use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
    use crate::compile::fragment_synth::fragment::FragmentProposalStream;
    use crate::compile::fragment_synth::recursive;
    use crate::compile::fragment_synth::search::{run_budgeted_proposals, SystemMonotonicClock};
    use crate::compile::fragment_synth::seed::{
        compile_sparse_seed_with_services, SeedInput, SeedServices,
    };
    use crate::compile::fragment_synth::services::{
        DurableSeedEmitter, DurableSeedVerifier, TopologyAwareSeedPlacer,
    };
    use crate::compile::metrics::Fingerprint;
    use crate::compile::planner::{Anchor, PortPlacements};
    use crate::compile::routing::DurablePhysicalRouter;
    use crate::compile::topology::Library;
    use crate::compile::{CircuitObservations, CompiledCircuit, Gate, Netlist, PlannerKind};
    use crate::redstone::world::block::Facing;

    /// A two-gate chain: the smallest case that exercises a real recursive
    /// root with more than one gate.
    fn two_gate_chain() -> Netlist {
        Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("m", &["a"]), Gate::nor("y", &["m"])],
        }
    }

    fn case_fingerprint_on(
        input: &SynthesisInput<'_>,
        path: SynthesisPath,
    ) -> super::SynthesisCaseFingerprint {
        let search = SearchConfig::checked_defaults();
        synthesis_case_fingerprint_with_revisions(
            input,
            path,
            &search,
            &CertificationConfig::from_search(&search),
            &Library::default_library(),
            topology_aware_seed_placement_revision(),
            recursive::producer_revision(),
        )
        .expect("test netlist must be valid")
    }

    /// **Test-only.** The legacy whole-circuit seed and its budgeted proposal
    /// loop, compiled explicitly and under the seed's own case identity.
    ///
    /// This was the production fallback until the recursive contract shipped
    /// for every case; it is now unreachable from any production entry and
    /// exists here so the seed search's own properties (trace prefixes, real
    /// certified proposals) stay tested. It deliberately shares nothing with
    /// [`super::compile_fragment_synth`] but the fingerprint descriptor.
    fn compile_legacy_whole_circuit_seed(
        input: SynthesisInput<'_>,
        budget: SynthesisBudget,
        search_config: &SearchConfig,
    ) -> SynthesisResult {
        let library = Library::default_library();
        let certification_config = CertificationConfig::from_search(search_config);
        let case_fingerprint = synthesis_case_fingerprint_with_revisions(
            &input,
            SynthesisPath::Seed,
            search_config,
            &certification_config,
            &library,
            topology_aware_seed_placement_revision(),
            recursive::producer_revision(),
        )
        .expect("legacy seed test netlist must be valid");
        let seed_input = SeedInput {
            lowered: input.lowered,
            source_provenance: input.source_provenance,
            pins: input.pins,
        };
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
            .expect("legacy whole-circuit seed must build the test netlist");

        let clock = SystemMonotonicClock::start();
        let mut proposals = FragmentProposalStream::new(seed_input, seed_services);
        let summary = run_budgeted_proposals(certified, budget, &clock, &mut proposals);

        let best = summary.best;
        let views = best
            .candidate()
            .compatibility_views(input.lowered)
            .expect("legacy seed candidate must expose compatibility metadata");
        let compiled = CompiledCircuit {
            world: best.world().clone(),
            input_positions: views.input_positions,
            output_positions: views.output_positions,
            gate_output_positions: views.gate_output_positions,
            gate_facings: views.gate_facings,
            observations: CircuitObservations::from_expanded(best.candidate()),
            legacy_emission: None,
            planner_kind: PlannerKind::FragmentSynth,
        };
        let metrics = best.metrics().clone();
        let candidate_fingerprint = metrics.candidate_fingerprint.clone();
        SynthesisResult {
            compiled,
            metrics,
            trace: summary.trace,
            evaluations_used: summary.evaluations_used,
            case_fingerprint,
            candidate_fingerprint,
            stop_reason: summary.stop_reason,
            recursive_diagnostics: None,
        }
    }

    /// Test-only: the public producer under a different placement revision,
    /// so a test can show the revision moves the case and nothing else.
    fn compile_with_placement_revision_override(
        input: SynthesisInput<'_>,
        budget: SynthesisBudget,
        search_config: &SearchConfig,
        placement_revision: Fingerprint,
    ) -> Result<SynthesisResult, SynthesisError> {
        let library = Library::default_library();
        let certification_config = CertificationConfig::from_search(search_config);
        let case_fingerprint = synthesis_case_fingerprint_with_revisions(
            &input,
            SynthesisPath::RecursiveContract,
            search_config,
            &certification_config,
            &library,
            placement_revision,
            recursive::producer_revision(),
        )?;
        compile_recursive_contract(input, budget, search_config, case_fingerprint)
    }

    /// **Source-level regression: production has one producer and no seed.**
    ///
    /// Everything before the test module in this file is what ships. It calls
    /// the recursive producer exactly once and names nothing of the seed at
    /// all: not the legacy whole-circuit builder or its proposal loop, not the
    /// parent-connectable leaf builder (that dependency is `leaf.rs`'s and
    /// stays there), not the seed services or router, not the `Seed` path
    /// variant, not a staging selector. The `Seed` variant itself is
    /// `cfg(test)`, so a production reference would not compile; this test is
    /// what fails first, with a name, if one is reintroduced behind a `cfg`.
    #[test]
    fn the_production_source_has_one_producer_and_no_seed_seam() {
        let source = include_str!("api.rs");
        let (production, _) = source
            .split_once("#[cfg(test)]\nmod tests {")
            .expect("api.rs keeps its tests in one trailing cfg(test) module");
        // Line-oriented: comments and doc comments are free to *mention* the
        // seed; code is not.
        let code: Vec<(usize, &str)> = production
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line.trim()))
            .filter(|(_, line)| !line.is_empty() && !line.starts_with("//"))
            .collect();
        let lines_naming = |needle: &str| -> Vec<usize> {
            code.iter()
                .filter(|(_, line)| line.contains(needle))
                .map(|(number, _)| *number)
                .collect()
        };
        let preceded_by_cfg_test = |number: usize| -> bool {
            code.iter()
                .rev()
                .find(|(candidate, _)| *candidate < number)
                .is_some_and(|(_, line)| *line == "#[cfg(test)]")
        };

        assert_eq!(
            lines_naming("recursive::compile(").len(),
            1,
            "production must call the recursive producer exactly once"
        );
        for seam in [
            // The legacy whole-circuit seed and its proposal loop.
            "compile_sparse_seed_with_services",
            "FragmentProposalStream",
            "run_budgeted_proposals",
            "SystemMonotonicClock",
            // The leaf builder and everything a seed build is wired from.
            "compile_parent_connectable_seed_with_services",
            "SparseSeedBuilder",
            "seed::",
            "SeedInput",
            "SeedServices",
            "services::",
            "DurablePhysicalRouter",
            "CompleteCandidateCertifier",
            // The selector, the cutover constant, and the error it produced.
            "staged_synthesis_path",
            "compile_fragment_synth_staged_recursive",
            "PUBLIC_PATH",
            "SynthesisError::Seed",
        ] {
            assert!(
                lines_naming(seam).is_empty(),
                "production source names the seed seam `{seam}` on lines {:?}",
                lines_naming(seam)
            );
        }
        // `SynthesisPath::Seed` may appear in production source exactly twice
        // -- its declaration and the fingerprint descriptor's arm -- and each
        // must sit directly under `#[cfg(test)]`, so no production build ever
        // compiles a value that could name the seed path.
        let variant = lines_naming("Seed,");
        let arm = lines_naming("SynthesisPath::Seed =>");
        assert_eq!(
            variant.len(),
            1,
            "one `Seed` variant declaration: {variant:?}"
        );
        assert_eq!(arm.len(), 1, "one `Seed` descriptor arm: {arm:?}");
        assert_eq!(lines_naming("SynthesisPath::Seed"), arm);
        for number in variant.into_iter().chain(arm) {
            assert!(
                preceded_by_cfg_test(number),
                "line {number} names the Seed path without `#[cfg(test)]` directly above it"
            );
        }
    }

    /// Which path compiled a case is part of what the case *is*.
    ///
    /// The two paths lay out different circuits from the same netlist. A case
    /// fingerprint that did not name the path would let a seed run report
    /// itself under the number a recursive run produced, which is exactly what
    /// a baseline is supposed to make impossible.
    #[test]
    fn the_path_a_case_takes_is_part_of_its_case_fingerprint() {
        let netlist = two_gate_chain();
        let input = SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        assert_ne!(
            case_fingerprint_on(&input, SynthesisPath::RecursiveContract),
            case_fingerprint_on(&input, SynthesisPath::Seed)
        );
    }

    /// **The public entry compiles on the recursive contract, and only there.**
    ///
    /// What [`compile_fragment_synth`] ships carries the recursive contract
    /// path's case identity -- the same one the production fingerprint
    /// function computes with no path argument at all -- and it is a real
    /// circuit rather than a stub: every gate the netlist declares has a
    /// position, and the ports a caller drives and reads are there.
    #[test]
    fn the_public_entry_compiles_on_the_recursive_contract_path() {
        let netlist = two_gate_chain();
        let input = SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let result = super::compile_fragment_synth(input, SynthesisBudget::Evaluations(0)).unwrap();

        let search = SearchConfig::checked_defaults();
        let shipping = synthesis_case_fingerprint(
            &input,
            &search,
            &CertificationConfig::from_search(&search),
            &Library::default_library(),
        )
        .unwrap();
        assert_eq!(result.case_fingerprint, shipping);
        assert_eq!(
            result.case_fingerprint,
            case_fingerprint_on(&input, SynthesisPath::RecursiveContract)
        );
        assert_ne!(
            result.case_fingerprint,
            case_fingerprint_on(&input, SynthesisPath::Seed)
        );
        assert_eq!(
            result.compiled.gate_output_positions.len(),
            netlist.gates.len()
        );
        assert_eq!(result.compiled.input_positions.len(), netlist.inputs.len());
        assert_eq!(
            result.compiled.output_positions.len(),
            netlist.outputs.len()
        );
        assert_eq!(
            result.candidate_fingerprint,
            result.metrics.candidate_fingerprint
        );
    }

    /// **Recursive failure cannot fall back.**
    ///
    /// A caller row the recursive contract cannot honour used to be routed to
    /// the seed by a staging rule. Now there is nothing to route to: the
    /// public entry returns the recursive refusal itself, and no circuit. The
    /// alternative -- attempt the recursive path and fall back on failure --
    /// would produce two different circuits under one case fingerprint with
    /// no way to tell them apart afterwards.
    #[test]
    fn pins_the_recursive_contract_cannot_honour_fail_the_public_entry_without_fallback() {
        let netlist = two_gate_chain();
        let mut honoured = PortPlacements::default();
        honoured.pin("a", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        honoured.pin("y", Anchor { x: 4, y: 1, z: 4 }, Facing::North);
        let mut unsupported = PortPlacements::default();
        unsupported.pin("a", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        unsupported.pin("y", Anchor { x: 4, y: 0, z: 4 }, Facing::North);

        let input = |pins| SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: Some(pins),
        };

        let honoured_result =
            super::compile_fragment_synth(input(&honoured), SynthesisBudget::Evaluations(4))
                .expect("a caller row the contract honours compiles");
        assert_eq!(
            honoured_result.case_fingerprint,
            case_fingerprint_on(&input(&honoured), SynthesisPath::RecursiveContract)
        );

        // The seed *could* build this geometry; the public entry must not ask
        // it to. A non-zero budget makes the point sharper: nothing spends it.
        let refused =
            super::compile_fragment_synth(input(&unsupported), SynthesisBudget::Evaluations(4));
        let Err(SynthesisError::RecursiveContract(message)) = refused else {
            panic!(
                "unsupported pins must surface the recursive refusal, got {:?}",
                refused
                    .as_ref()
                    .map(|result| result.case_fingerprint.clone())
            );
        };
        // Not *a* recursive failure: the root-pin refusal itself, naming the
        // port and the cell, which is what `honours_pins` used to route on.
        assert!(
            message.starts_with("root port y is pinned at ")
                && message.contains(" facing North, which "),
            "expected the UnsupportedRootPin refusal for `y`, got: {message}"
        );
    }

    /// **F1.** The recursive path spends no budget, but it does not misreport
    /// which budget the caller asked under.
    #[test]
    fn the_public_entry_reports_the_stop_reason_matching_the_budget_kind() {
        let netlist = two_gate_chain();
        let input = || SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };

        for budget in [
            SynthesisBudget::Evaluations(0),
            SynthesisBudget::Evaluations(4),
        ] {
            let result = super::compile_fragment_synth(input(), budget).unwrap();
            assert_eq!(result.stop_reason, StopReason::EvaluationBudget);
            // The budget bought nothing, which is the honest thing to report
            // for a path with no proposal loop.
            assert_eq!(result.evaluations_used, 0);
            assert!(result.trace.is_empty());
        }

        for budget in [
            SynthesisBudget::Time(Duration::from_millis(1)),
            SynthesisBudget::Time(Duration::from_secs(30)),
        ] {
            let result = super::compile_fragment_synth(input(), budget).unwrap();
            assert_eq!(
                result.stop_reason,
                StopReason::TimeBudget,
                "a time-budgeted call must not report an evaluation budget"
            );
            assert_eq!(result.evaluations_used, 0);
            assert!(result.trace.is_empty());
        }

        // Same circuit either way: the budget names the stop reason, not the
        // producer.
        let evaluations =
            super::compile_fragment_synth(input(), SynthesisBudget::Evaluations(4)).unwrap();
        let timed =
            super::compile_fragment_synth(input(), SynthesisBudget::Time(Duration::from_secs(30)))
                .unwrap();
        assert_eq!(
            evaluations.candidate_fingerprint,
            timed.candidate_fingerprint
        );
        assert_eq!(evaluations.case_fingerprint, timed.case_fingerprint);
    }

    /// **F3.** Revising the recursive producer -- its grain, its split, its
    /// packing -- makes a new case, and leaves every seed case alone.
    #[test]
    fn the_recursive_producer_revision_is_part_of_the_recursive_case_identity() {
        let netlist = two_gate_chain();
        let input = SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let config = SearchConfig::checked_defaults();
        let certification = CertificationConfig::from_search(&config);
        let library = Library::default_library();
        let case = |path, revision| {
            synthesis_case_fingerprint_with_revisions(
                &input,
                path,
                &config,
                &certification,
                &library,
                topology_aware_seed_placement_revision(),
                revision,
            )
            .unwrap()
        };
        let shipping = recursive::producer_revision();
        let revised = crate::compile::metrics::canonical_fingerprint(b"a-different-generator");
        assert_ne!(shipping, revised);

        assert_ne!(
            case(SynthesisPath::RecursiveContract, shipping.clone()),
            case(SynthesisPath::RecursiveContract, revised.clone()),
            "revising the recursive producer must make a new recursive case"
        );
        assert_eq!(
            case(SynthesisPath::Seed, shipping.clone()),
            case(SynthesisPath::Seed, revised),
            "a seed case must not move when the recursive producer is revised"
        );
        // And the shipping public call is the one built under the shipping
        // revision, not a stale constant kept beside it.
        assert_eq!(
            super::compile_fragment_synth(input, SynthesisBudget::Evaluations(0))
                .unwrap()
                .case_fingerprint,
            case(SynthesisPath::RecursiveContract, shipping)
        );
    }

    /// **Byte compatibility of the shipping case identity, pinned.**
    ///
    /// These are the case fingerprints the public entry reports for three
    /// fixed cases under the schema-3 descriptor. The two unpinned ones were
    /// measured on the pre-deletion `api.rs` with an identical probe and still
    /// hold; the pinned one was measured after the deletion, on a descriptor
    /// whose serialised shape -- `PinDescriptor` with its `Facing` code and
    /// all -- the deletion did not touch, so it is the pre-deletion identity
    /// too. Every baseline recorded under any of them is still found.
    ///
    /// If this fails, either the descriptor, a revision it names, the pin or
    /// facing encoding, or the canonical netlist order changed -- and that is
    /// a deliberate bump to make, with the constants below updated in the same
    /// change, not a side effect to absorb.
    #[test]
    fn the_shipping_case_fingerprint_is_byte_compatible_with_the_pre_deletion_descriptor() {
        const TWO_GATE_CHAIN_CASE: &str =
            "424dcfa7f22fce9927ae60d36406b20c021efa909e73732968205150a2a5f533";
        const AND4_CASE: &str = "7cb128cb6bc2d9f623f089939587ea9befa1a934df493debee396a052b800c12";
        /// The chain on the caller row the contract honours: `a` at
        /// (1,1,4) facing south, `y` at (4,1,4) facing north.
        const PINNED_TWO_GATE_CHAIN_CASE: &str =
            "1a4dd5e1d444a01a194bc09d4549164c5ecec7e75bbc7e5f3300d3083fd65b15";

        let chain = two_gate_chain();
        let (and4, _) = build_and4_netlist();
        let mut honoured = PortPlacements::default();
        honoured.pin("a", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        honoured.pin("y", Anchor { x: 4, y: 1, z: 4 }, Facing::North);

        let cases: [(&Netlist, Option<&PortPlacements>, &str); 3] = [
            (&chain, None, TWO_GATE_CHAIN_CASE),
            (&and4, None, AND4_CASE),
            (&chain, Some(&honoured), PINNED_TWO_GATE_CHAIN_CASE),
        ];
        for (netlist, pins, expected) in cases {
            assert_eq!(expected.len(), 64);
            let input = SynthesisInput {
                lowered: netlist,
                source_provenance: None,
                pins,
            };
            let shipped = super::compile_fragment_synth(input, SynthesisBudget::Evaluations(0))
                .unwrap()
                .case_fingerprint;
            assert_eq!(shipped.as_str(), expected);
            // And the fingerprint function alone agrees, so the public entry
            // is not computing an identity of its own.
            let search = SearchConfig::checked_defaults();
            assert_eq!(
                synthesis_case_fingerprint(
                    &input,
                    &search,
                    &CertificationConfig::from_search(&search),
                    &Library::default_library(),
                )
                .unwrap()
                .as_str(),
                expected
            );
        }
        // Pins are part of the identity, and the pinned constant is not
        // accidentally the unpinned one.
        assert_ne!(PINNED_TWO_GATE_CHAIN_CASE, TWO_GATE_CHAIN_CASE);

        // The pieces the constants above are made of, named so a failure
        // points somewhere.
        assert_eq!(
            serde_json::to_string(&SynthesisPath::RecursiveContract).unwrap(),
            "\"RecursiveContract\""
        );
        assert_eq!(
            super::topology_aware_seed_placement_revision(),
            crate::compile::metrics::canonical_fingerprint(b"topology-aware-seed-v2")
        );
    }

    #[test]
    fn declaration_order_does_not_change_case_fingerprint() {
        let ordered = two_gate_chain();
        let reordered = Netlist {
            gates: vec![ordered.gates[1].clone(), ordered.gates[0].clone()],
            ..ordered.clone()
        };
        let ordered_input = SynthesisInput {
            lowered: &ordered,
            source_provenance: None,
            pins: None,
        };
        let reordered_input = SynthesisInput {
            lowered: &reordered,
            source_provenance: None,
            pins: None,
        };
        let path = SynthesisPath::RecursiveContract;

        assert_eq!(
            case_fingerprint_on(&ordered_input, path),
            case_fingerprint_on(&reordered_input, path)
        );

        let independent = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["x".into(), "y".into()],
            gates: vec![Gate::nor("x", &["a"]), Gate::nor("y", &["b"])],
        };
        let independent_reordered = Netlist {
            gates: vec![independent.gates[1].clone(), independent.gates[0].clone()],
            ..independent.clone()
        };
        assert_eq!(
            case_fingerprint_on(
                &SynthesisInput {
                    lowered: &independent,
                    source_provenance: None,
                    pins: None,
                },
                path,
            ),
            case_fingerprint_on(
                &SynthesisInput {
                    lowered: &independent_reordered,
                    source_provenance: None,
                    pins: None,
                },
                path,
            )
        );

        let mut changed_interface = ordered.clone();
        changed_interface.outputs = vec!["m".into()];
        let changed_input = SynthesisInput {
            lowered: &changed_interface,
            source_provenance: None,
            pins: None,
        };
        assert_ne!(
            case_fingerprint_on(&ordered_input, path),
            case_fingerprint_on(&changed_input, path)
        );
    }

    #[test]
    fn invalid_netlist_returns_error_before_synthesis() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["missing"])],
        };
        let result = super::compile_fragment_synth(
            SynthesisInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SynthesisBudget::Evaluations(0),
        );
        assert!(matches!(
            result,
            Err(SynthesisError::InvalidNetlist(message))
                if message.contains("neither a primary input nor a gate output")
        ));
    }

    /// The public API is one producer at every budget, and a case's identity
    /// does not move with the budget.
    ///
    /// The regression this pins: letting the budget decide the path made the
    /// same netlist two different cases depending on how many evaluations the
    /// caller asked for, so a baseline recorded at one budget could never be
    /// found again at another.
    #[test]
    fn a_public_case_keeps_one_identity_across_evaluation_budgets() {
        let netlist = two_gate_chain();
        let input = || SynthesisInput {
            lowered: &netlist,
            source_provenance: None,
            pins: None,
        };
        let seed_case = case_fingerprint_on(&input(), SynthesisPath::Seed);
        let public_case = case_fingerprint_on(&input(), SynthesisPath::RecursiveContract);

        let mut circuits = Vec::new();
        for budget in [0, 1, 2] {
            let result =
                super::compile_fragment_synth(input(), SynthesisBudget::Evaluations(budget))
                    .unwrap();
            assert_eq!(
                result.case_fingerprint, public_case,
                "budget {budget} compiled under a case other than the public one"
            );
            assert_ne!(
                result.case_fingerprint, seed_case,
                "the public case must not be the seed's"
            );
            circuits.push(result.candidate_fingerprint);
        }
        circuits.dedup();
        assert_eq!(
            circuits.len(),
            1,
            "one producer, one circuit, at every budget"
        );
    }

    /// The seed's budgeted proposal loop, named explicitly.
    ///
    /// This reached the seed through the public entry until the cutover. The
    /// property is the seed producer's -- the recursive path has no proposal
    /// loop and spends no evaluations -- so it is exercised through the
    /// test-only legacy helper rather than anything that ships.
    #[test]
    fn complete_seed_syntheses_at_larger_evaluation_budgets_extend_one_trace_prefix() {
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
        let seed = |budget| {
            compile_legacy_whole_circuit_seed(
                input(),
                SynthesisBudget::Evaluations(budget),
                &config,
            )
        };
        let complete = seed(8);
        assert_eq!(
            complete.case_fingerprint,
            case_fingerprint_on(&input(), SynthesisPath::Seed)
        );

        for budget in [0, 1, 2, 4, 8] {
            let result = seed(budget);
            assert_eq!(result.evaluations_used, budget);
            assert_eq!(result.trace, complete.trace[..budget as usize]);
            assert_eq!(result.case_fingerprint, complete.case_fingerprint);
            assert!(complete.metrics.quality <= result.metrics.quality);
        }
    }

    /// The seed's first budgeted proposal is a real certified fragment
    /// transaction. Named for the same reason as the trace-prefix test above.
    #[test]
    fn the_first_budgeted_seed_proposal_is_a_real_certified_fragment_transaction() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["y".into()],
            gates: vec![Gate::nor("y", &["a"])],
        };
        let config = SearchConfig::checked_defaults();
        let result = compile_legacy_whole_circuit_seed(
            SynthesisInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SynthesisBudget::Evaluations(1),
            &config,
        );

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
        )
        .expect("test netlist must be valid");
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
                )
                .expect("test netlist must be valid"),
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
        let config = SearchConfig::checked_defaults();
        let old_revision = crate::compile::metrics::canonical_fingerprint(b"seed-v1");
        let new_revision =
            crate::compile::metrics::canonical_fingerprint(b"topology-aware-seed-v2");

        let old = compile_with_placement_revision_override(
            input,
            SynthesisBudget::Evaluations(0),
            &config,
            old_revision,
        )
        .unwrap();
        let new = compile_with_placement_revision_override(
            input,
            SynthesisBudget::Evaluations(0),
            &config,
            new_revision,
        )
        .unwrap();
        assert_ne!(old.case_fingerprint, new.case_fingerprint);
        assert_eq!(old.candidate_fingerprint, new.candidate_fingerprint);
        // The shipping revision is the one the public entry uses.
        assert_eq!(
            new.case_fingerprint,
            super::compile_fragment_synth(input, SynthesisBudget::Evaluations(0))
                .unwrap()
                .case_fingerprint
        );
    }
}
