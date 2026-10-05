//! Certified physical rewrites retain both netlists instead of fabricating
//! observation positions for source gates removed by the rewrite.

use super::candidate::{CandidateError, CompatibilityViews, ExpandedPhysicalCandidate};
use super::identity::{ObservationId, PortId};
use super::realise::{CertificationError, CertifiedWorld};
use crate::compile::equivalence::{
    prove_combinational_equivalence, prove_netlist_rewrite, EquivalenceCertificate,
    EquivalenceError, NetlistRewriteCertificate, NetlistRewriteError,
};
use crate::compile::topology::Library;
use crate::compile::Netlist;

/// These boundaries need retained source-port occurrences, not name-keyed maps.
pub(crate) fn needs_occurrence_preserving_boundary(source: &Netlist) -> bool {
    source.outputs.iter().any(|output| source.inputs.contains(output))
        || source.outputs.iter().collect::<std::collections::BTreeSet<_>>().len()
            != source.outputs.len()
}

/// Give aliases real implementation buffers when a specialized boundary does
/// not apply. The source graph and its declared output identities stay intact.
pub(crate) fn compile_occurrence_boundary(
    source: &Netlist,
) -> Result<CertifiedPhysicalRewrite, crate::compile::CompileError> {
    use crate::compile::{CompileError, Gate};
    crate::compile::checked_rewrite_topological_order(source)?;
    let mut prefix = "__reda_public_output_".to_string();
    while source.inputs.iter().chain(&source.outputs)
        .chain(source.gates.iter().flat_map(|g| [&g.name, &g.output]))
        .any(|name| name.starts_with(&prefix))
    {
        prefix.push('_');
    }
    let mut implemented = source.clone();
    let mut seen = std::collections::HashSet::new();
    for (ordinal, signal) in source.outputs.iter().enumerate() {
        if !seen.insert(signal) || source.inputs.contains(signal) {
            let inverse = format!("{prefix}{ordinal}_inverse");
            let terminal = format!("{prefix}{ordinal}_buffer");
            implemented.gates.push(Gate::nor(&inverse, &[signal.as_str()]));
            implemented.gates.push(Gate::nor(&terminal, &[inverse.as_str()]));
            implemented.outputs[ordinal] = terminal;
        }
    }
    let physical = crate::compile::compile_without_rewrite(&implemented)?;
    let candidate = physical.physical_candidate().ok_or_else(||
        CompileError::CandidateMetadataViolation {
            item: "public output boundary".into(),
            reason: "physical implementation candidate is missing".into(),
        })?;
    realise_physical_rewrite_with_size(
        source, &implemented, candidate,
        &Library::default_library(), &implemented.inputs, &implemented.outputs,
        1_000_000, Some(physical.world.size()),
    ).map_err(|error| CompileError::CandidateMetadataViolation {
        item: "public output boundary".into(),
        reason: format!("{error:?}"),
    })
}

#[derive(Debug)]
pub enum PhysicalRewriteError {
    Rewrite(NetlistRewriteError),
    Equivalence(EquivalenceError),
    Certification(CertificationError),
    Boundary(CandidateError),
    MissingOutputDriver { signal: String },
}

/// A source boundary and its actual rewritten physical implementation.
/// Read-only access keeps the netlists, proofs and world tied together.
#[derive(Debug, Clone)]
pub struct CertifiedPhysicalRewrite {
    library: std::sync::Arc<Library>,
    source: Netlist,
    implemented: Netlist,
    rewrite: NetlistRewriteCertificate,
    equivalence: EquivalenceCertificate,
    physical: CertifiedWorld,
    implemented_candidate: ExpandedPhysicalCandidate,
    observations: CompatibilityViews,
    source_inputs: Vec<(i32, i32, i32)>,
    source_outputs: Vec<(i32, i32, i32)>,
    source_output_drivers: Vec<(i32, i32, i32)>,
}

impl CertifiedPhysicalRewrite {
    /// Compose a Boolean source rewrite with this already-certified hardware.
    /// Port mappings and physical observations remain tied to the implementation.
    pub(crate) fn rebind_source(
        &mut self,
        source: &Netlist,
        proof_work_limit: u64,
    ) -> Result<(), PhysicalRewriteError> {
        let rewrite = prove_netlist_rewrite(
            source,
            &self.implemented,
            self.rewrite.rewritten_inputs_in_source_order(),
            self.rewrite.rewritten_outputs_in_source_order(),
            proof_work_limit,
        ).map_err(PhysicalRewriteError::Rewrite)?;
        self.source = source.clone();
        self.rewrite = rewrite;
        Ok(())
    }

    /// Measure the retained implementation with its own library and emission
    /// extent. Source gate names are not observations of a rewritten circuit.
    pub fn certify_runtime(
        &self,
        config: &super::config::CertificationConfig,
    ) -> Result<
        super::certification::CertifiedCandidate,
        super::certification::CandidateCertificationError,
    > {
        super::certification::CompleteCandidateCertifier.certify_with_size(
            self.implemented_candidate.clone(),
            &self.implemented,
            self.implementation_library(),
            config,
            Some(self.physical.world().size()),
        )
    }
    /// Library IDs are meaningful only in the exact library that expanded
    /// this implementation. Keep that context with its certified artifact.
    pub(crate) fn implementation_library(&self) -> &Library {
        &self.library
    }
    pub fn source(&self) -> &Netlist {
        &self.source
    }
    pub fn implemented(&self) -> &Netlist {
        &self.implemented
    }
    pub fn rewrite_certificate(&self) -> &NetlistRewriteCertificate {
        &self.rewrite
    }
    pub fn equivalence_certificate(&self) -> &EquivalenceCertificate {
        &self.equivalence
    }
    pub fn physical(&self) -> &CertifiedWorld {
        &self.physical
    }
    /// The exact techniques, placements, routes and observations certified
    /// for implemented(). Consumers must use this manifest rather than expand
    /// source() or substitute the default library's techniques.
    pub fn implemented_candidate(&self) -> &ExpandedPhysicalCandidate {
        &self.implemented_candidate
    }
    /// All internal positions belong to implemented(), never source().
    pub fn implemented_observations(&self) -> &CompatibilityViews {
        &self.observations
    }
    /// Positions in source declaration order; output aliases retain multiplicity.
    pub fn source_input_positions(&self) -> &[(i32, i32, i32)] {
        &self.source_inputs
    }
    pub fn source_output_positions(&self) -> &[(i32, i32, i32)] {
        &self.source_outputs
    }
    pub fn source_output_driver_positions(&self) -> &[(i32, i32, i32)] {
        &self.source_output_drivers
    }
}

/// Prove the complete source boundary, prove its rewritten instance graph,
/// then certify and emit exactly that candidate. No partially checked result
/// escapes on any failure. Bindings use implemented names in source order.
/// Each proof uses the caller's work limit; this is not a timing guarantee.
pub fn realise_physical_rewrite(
    source: &Netlist,
    implemented: &Netlist,
    candidate: &ExpandedPhysicalCandidate,
    library: &Library,
    inputs_in_source_order: &[String],
    outputs_in_source_order: &[String],
    proof_work_limit: u64,
) -> Result<CertifiedPhysicalRewrite, PhysicalRewriteError> {
    realise_physical_rewrite_with_size(
        source,
        implemented,
        candidate,
        library,
        inputs_in_source_order,
        outputs_in_source_order,
        proof_work_limit,
        None,
    )
}

pub(crate) fn realise_physical_rewrite_with_size(
    source: &Netlist,
    implemented: &Netlist,
    candidate: &ExpandedPhysicalCandidate,
    library: &Library,
    inputs_in_source_order: &[String],
    outputs_in_source_order: &[String],
    proof_work_limit: u64,
    size: Option<(i32, i32, i32)>,
) -> Result<CertifiedPhysicalRewrite, PhysicalRewriteError> {
    let rewrite = prove_netlist_rewrite(
        source,
        implemented,
        inputs_in_source_order,
        outputs_in_source_order,
        proof_work_limit,
    )
    .map_err(PhysicalRewriteError::Rewrite)?;
    let equivalence =
        prove_combinational_equivalence(implemented, candidate, library, proof_work_limit)
            .map_err(PhysicalRewriteError::Equivalence)?;
    let physical = super::realise::realise_and_verify_expanded_with_size(
        candidate,
        implemented,
        library,
        size,
    )
    .map_err(PhysicalRewriteError::Certification)?;
    let observations = candidate
        .compatibility_views(implemented)
        .map_err(PhysicalRewriteError::Boundary)?;
    let source_inputs = inputs_in_source_order
        .iter()
        .map(|name| observations.input_positions[name])
        .collect();
    // Names alone collapse repeated output aliases. Consume each declared
    // occurrence and retain its own certified boundary observation.
    let mut remaining_outputs: Vec<_> = implemented.outputs.iter().enumerate().collect();
    let source_outputs = outputs_in_source_order
        .iter()
        .map(|name| {
            let ordinal = remaining_outputs
                .iter()
                .position(|(_, signal)| *signal == name)
                .expect("the rewrite proof checked the complete output multiset");
            let (index, _) = remaining_outputs.remove(ordinal);
            let id =
                PortId(u32::try_from(index).map_err(|_| {
                    PhysicalRewriteError::Boundary(CandidateError::IdentityOverflow)
                })?);
            let observation = ObservationId::DeclaredOutput(id);
            let at = candidate
                .observations
                .get(&observation)
                .ok_or(PhysicalRewriteError::Boundary(
                    CandidateError::MissingCompatibilityObservation { observation },
                ))?
                .site
                .at;
            Ok((at.x, at.y, at.z))
        })
        .collect::<Result<Vec<_>, PhysicalRewriteError>>()?;
    let source_output_drivers = outputs_in_source_order
        .iter()
        .map(|name| {
            observations
                .gate_output_positions
                .get(name)
                .copied()
                .or_else(|| observations.input_positions.get(name).copied())
                .ok_or_else(|| PhysicalRewriteError::MissingOutputDriver {
                    signal: name.clone(),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CertifiedPhysicalRewrite {
        library: std::sync::Arc::new(library.clone()),
        source: source.clone(),
        implemented: implemented.clone(),
        rewrite,
        equivalence,
        physical,
        implemented_candidate: candidate.clone(),
        observations,
        source_inputs,
        source_outputs,
        source_output_drivers,
    })
}
