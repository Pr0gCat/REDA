//! Independent structural certification for expanded physical candidates.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::candidate::{
    endpoint_for_driver, CandidateError, ExpandedPhysicalCandidate, PrimitivePlacement,
    RouteTarget, VerifiedObservation,
};
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, ObservationId, PhysicalEndpointId, PortId, PrimitiveId, RouteId,
};
use crate::compile::fragment_synth::instance_graph::{PhysicalSink, SynthesisError};
use crate::compile::fragment_synth::topology::{
    instantiate, ConnectionSource, ConnectionSpec, ContributorSpec, ExpandedInstance, OutputSpec,
    PrimitiveSpec,
};
use crate::compile::metrics::Fingerprint;
use crate::compile::physical::{self, LocalBlock, PhysicalVariant};
use crate::compile::topology::{Library, Primitive};
use crate::compile::Netlist;
use crate::redstone::world::block::BlockKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StableStructuralId {
    Instance(InstanceId),
    Primitive(PrimitiveId),
    Connection(ConnectionId),
    Route(RouteId),
    Endpoint(PhysicalEndpointId),
    DeclaredOutput(PortId),
    Observation(ObservationId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralMismatchKind {
    InstanceGraph,
    ImplementationTopology,
    PrimitiveSet,
    PrimitiveKind,
    PrimitiveFacing,
    PrimitiveVariant,
    PrimitiveFootprint,
    ConnectionTopology,
    ExternalSink,
    JunctionContributor,
    DeclaredOutput,
    Observation,
    PinContract,
    PhysicalOwnership,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CertificationError {
    #[error("structural mismatch {kind:?} at {affected:?}")]
    StructuralMismatch {
        kind: StructuralMismatchKind,
        affected: StableStructuralId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralCertificate {
    pub candidate_fingerprint: Fingerprint,
    pub library_revision: Fingerprint,
    pub instance_count: usize,
}

type CertificationResult<T> = Result<T, CertificationError>;

#[derive(Default)]
struct Authority {
    instances: BTreeMap<InstanceId, ExpandedInstance>,
    primitives: BTreeMap<PrimitiveId, PrimitiveSpec>,
    connections: BTreeMap<ConnectionId, (InstanceId, ConnectionSpec)>,
    outputs: BTreeMap<InstanceId, OutputSpec>,
}

/// Re-instantiate all selected implementations from `library`, then certify
/// candidate structure without invoking emission or the physical verifier.
pub fn certify_expanded_structure(
    candidate: &ExpandedPhysicalCandidate,
    netlist: &Netlist,
    library: &Library,
) -> CertificationResult<StructuralCertificate> {
    let authority = reinstantiate_authority(candidate, netlist, library)?;
    validate_instance_graph(candidate, netlist)?;
    validate_primitive_placements(candidate, &authority)?;
    validate_connections(candidate, &authority)?;
    validate_junctions(candidate, &authority)?;
    validate_declared_outputs(candidate)?;
    validate_observations(candidate, &authority)?;
    candidate
        .validate_pin_contracts_against(netlist)
        .map_err(|error| pin_contract_error(candidate, error))?;
    candidate
        .validate_shape()
        .map_err(|error| shape_error(candidate, error))?;
    candidate
        .validate_physical_ownership()
        .map_err(|error| physical_ownership_error(candidate, error))?;
    Ok(StructuralCertificate {
        candidate_fingerprint: candidate.fingerprint(),
        library_revision: library.revision_fingerprint(),
        instance_count: authority.instances.len(),
    })
}

fn mismatch<T>(
    kind: StructuralMismatchKind,
    affected: StableStructuralId,
) -> CertificationResult<T> {
    Err(CertificationError::StructuralMismatch { kind, affected })
}

fn reinstantiate_authority(
    candidate: &ExpandedPhysicalCandidate,
    netlist: &Netlist,
    library: &Library,
) -> CertificationResult<Authority> {
    let mut authority = Authority::default();
    let mut seen = BTreeSet::new();
    for instance in &candidate.instances.instances {
        if !seen.insert(instance.id) {
            return mismatch(
                StructuralMismatchKind::InstanceGraph,
                StableStructuralId::Instance(instance.id),
            );
        }
        let Some(gate) = usize::try_from(instance.logical_gate.0)
            .ok()
            .and_then(|index| netlist.gates.get(index))
        else {
            return mismatch(
                StructuralMismatchKind::InstanceGraph,
                StableStructuralId::Instance(instance.id),
            );
        };
        if instance.expanded.instance != instance.id
            || instance.expanded.implementation != instance.implementation
        {
            return mismatch(
                StructuralMismatchKind::ImplementationTopology,
                StableStructuralId::Instance(instance.id),
            );
        }
        let expected =
            instantiate(library, gate, instance.id, &instance.implementation).map_err(|_| {
                CertificationError::StructuralMismatch {
                    kind: StructuralMismatchKind::ImplementationTopology,
                    affected: StableStructuralId::Instance(instance.id),
                }
            })?;
        compare_expanded(instance.id, &expected, &instance.expanded)?;
        for primitive in &expected.topology.primitives {
            if authority
                .primitives
                .insert(primitive.id, *primitive)
                .is_some()
            {
                return mismatch(
                    StructuralMismatchKind::PrimitiveSet,
                    StableStructuralId::Primitive(primitive.id),
                );
            }
        }
        for connection in &expected.topology.connections {
            if authority
                .connections
                .insert(connection.id, (instance.id, *connection))
                .is_some()
            {
                return mismatch(
                    StructuralMismatchKind::ConnectionTopology,
                    StableStructuralId::Connection(connection.id),
                );
            }
        }
        authority
            .outputs
            .insert(instance.id, expected.topology.output.clone());
        authority.instances.insert(instance.id, expected);
    }
    Ok(authority)
}

fn compare_expanded(
    instance: InstanceId,
    expected: &ExpandedInstance,
    actual: &ExpandedInstance,
) -> CertificationResult<()> {
    let mut actual_primitives = BTreeMap::new();
    for primitive in &actual.topology.primitives {
        if actual_primitives.insert(primitive.id, primitive).is_some() {
            return mismatch(
                StructuralMismatchKind::PrimitiveSet,
                StableStructuralId::Primitive(primitive.id),
            );
        }
    }
    let expected_ids = expected
        .topology
        .primitives
        .iter()
        .map(|primitive| primitive.id)
        .collect::<BTreeSet<_>>();
    let actual_ids = actual_primitives.keys().copied().collect::<BTreeSet<_>>();
    if let Some(id) = expected_ids
        .symmetric_difference(&actual_ids)
        .next()
        .copied()
    {
        return mismatch(
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(id),
        );
    }
    for primitive in &expected.topology.primitives {
        let actual = actual_primitives[&primitive.id];
        if actual.primitive != primitive.primitive {
            return mismatch(
                StructuralMismatchKind::PrimitiveKind,
                StableStructuralId::Primitive(primitive.id),
            );
        }
        if actual.role != primitive.role {
            return mismatch(
                StructuralMismatchKind::ImplementationTopology,
                StableStructuralId::Primitive(primitive.id),
            );
        }
    }

    let mut actual_connections = BTreeMap::new();
    for connection in &actual.topology.connections {
        if actual_connections
            .insert(connection.id, connection)
            .is_some()
        {
            return mismatch(
                StructuralMismatchKind::ConnectionTopology,
                StableStructuralId::Connection(connection.id),
            );
        }
    }
    let expected_ids = expected
        .topology
        .connections
        .iter()
        .map(|connection| connection.id)
        .collect::<BTreeSet<_>>();
    let actual_ids = actual_connections.keys().copied().collect::<BTreeSet<_>>();
    if let Some(id) = expected_ids
        .symmetric_difference(&actual_ids)
        .next()
        .copied()
    {
        return mismatch(
            StructuralMismatchKind::ConnectionTopology,
            StableStructuralId::Connection(id),
        );
    }
    for connection in &expected.topology.connections {
        if *actual_connections[&connection.id] != *connection {
            return mismatch(
                StructuralMismatchKind::ConnectionTopology,
                StableStructuralId::Connection(connection.id),
            );
        }
    }
    if actual.topology.output != expected.topology.output
        || actual.topology.embedding_hints != expected.topology.embedding_hints
        || actual.topology.fingerprint != expected.topology.fingerprint
    {
        return mismatch(
            StructuralMismatchKind::ImplementationTopology,
            StableStructuralId::Instance(instance),
        );
    }
    Ok(())
}

fn validate_instance_graph(
    candidate: &ExpandedPhysicalCandidate,
    netlist: &Netlist,
) -> CertificationResult<()> {
    candidate.instances.validate(netlist).map_err(|error| {
        let affected = match error {
            SynthesisError::MissingAssignment { sink }
            | SynthesisError::DuplicateAssignment { sink }
            | SynthesisError::UnexpectedAssignment { sink }
            | SynthesisError::WrongLogicalSignal { sink, .. }
            | SynthesisError::DuplicateSinkSignalMismatch { sink, .. }
            | SynthesisError::DriverSignalMismatch { sink, .. }
            | SynthesisError::WrongPhysicalDriver { sink } => stable_sink(sink),
            SynthesisError::DuplicateInputMismatch { instance, .. }
            | SynthesisError::DuplicateInstanceId { instance }
            | SynthesisError::ExpandedInstanceMismatch { instance }
            | SynthesisError::UnknownImplementationOverride { instance }
            | SynthesisError::UnknownLogicalGate { instance, .. }
            | SynthesisError::UnknownDriverInstance { instance } => {
                StableStructuralId::Instance(instance)
            }
            SynthesisError::MissingDuplicateCanonical { canonical }
            | SynthesisError::UnsupportedDuplicateTopology { canonical } => {
                StableStructuralId::Instance(canonical)
            }
            SynthesisError::InvalidBlockOutputGate { block, .. } => {
                StableStructuralId::Instance(block)
            }
            SynthesisError::UnsupportedStatefulTopology { gate }
            | SynthesisError::NoLibraryEntry { gate }
            | SynthesisError::MissingCanonicalInstance { gate }
            | SynthesisError::DuplicateCanonicalInstance { gate }
            | SynthesisError::Topology { gate, .. }
            | SynthesisError::DuplicateInstanceRole { gate, .. }
            | SynthesisError::RepeatedDuplicateRequest { gate, .. } => {
                StableStructuralId::Instance(InstanceId(gate.0))
            }
            SynthesisError::IdentityOverflow
            | SynthesisError::UndrivenSignal { .. }
            | SynthesisError::DuplicateSignalDriver { .. }
            | SynthesisError::NonCanonicalInstanceOrder
            | SynthesisError::NonCanonicalAssignmentOrder => candidate
                .instances
                .instances
                .first()
                .map(|instance| StableStructuralId::Instance(instance.id))
                .unwrap_or(StableStructuralId::DeclaredOutput(PortId(0))),
        };
        CertificationError::StructuralMismatch {
            kind: StructuralMismatchKind::InstanceGraph,
            affected,
        }
    })
}

fn stable_sink(sink: PhysicalSink) -> StableStructuralId {
    match sink {
        PhysicalSink::InstanceInput { instance, .. } => StableStructuralId::Instance(instance),
        PhysicalSink::DeclaredOutput(port) => StableStructuralId::DeclaredOutput(port),
    }
}

fn shape_error(candidate: &ExpandedPhysicalCandidate, error: CandidateError) -> CertificationError {
    let (kind, affected) = match error {
        CandidateError::DuplicateRouteCell { route, .. }
        | CandidateError::DuplicateRouteFloor { route, .. }
        | CandidateError::TerminalStateMismatch { route, .. }
        | CandidateError::RouteSinkMismatch { route, .. }
        | CandidateError::DuplicateRoutedSink { route, .. }
        | CandidateError::DuplicateRouteTarget { route, .. }
        | CandidateError::EmptyRoutePath { route, .. }
        | CandidateError::RoutePathEndpointMismatch { route, .. }
        | CandidateError::DiscontinuousRoutePath { route, .. }
        | CandidateError::MissingRoutePathCell { route, .. }
        | CandidateError::RouteSourceMismatch { route, .. }
        | CandidateError::RouteTimingMismatch { route, .. }
        | CandidateError::RouteKeyMismatch { key: route, .. } => (
            StructuralMismatchKind::ConnectionTopology,
            StableStructuralId::Route(route),
        ),
        CandidateError::PinContractMismatch { endpoint } => (
            StructuralMismatchKind::PinContract,
            StableStructuralId::Endpoint(endpoint),
        ),
        CandidateError::PrimitiveKeyMismatch { key, .. } => (
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(key),
        ),
        _ => {
            let affected = candidate
                .routes
                .keys()
                .next()
                .copied()
                .map(StableStructuralId::Route)
                .or_else(|| {
                    candidate
                        .placements
                        .keys()
                        .next()
                        .copied()
                        .map(StableStructuralId::Primitive)
                })
                .unwrap_or(StableStructuralId::DeclaredOutput(PortId(0)));
            (StructuralMismatchKind::PhysicalOwnership, affected)
        }
    };
    CertificationError::StructuralMismatch { kind, affected }
}

fn pin_contract_error(
    candidate: &ExpandedPhysicalCandidate,
    error: CandidateError,
) -> CertificationError {
    let affected = match error {
        CandidateError::PinContractMismatch { endpoint } => StableStructuralId::Endpoint(endpoint),
        _ => candidate
            .pin_contracts
            .keys()
            .next()
            .copied()
            .map(StableStructuralId::Endpoint)
            .unwrap_or(StableStructuralId::DeclaredOutput(PortId(0))),
    };
    CertificationError::StructuralMismatch {
        kind: StructuralMismatchKind::PinContract,
        affected,
    }
}

fn validate_primitive_placements(
    candidate: &ExpandedPhysicalCandidate,
    authority: &Authority,
) -> CertificationResult<()> {
    let expected = authority
        .primitives
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let actual = candidate
        .placements
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    if let Some(id) = expected.symmetric_difference(&actual).next().copied() {
        return mismatch(
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(id),
        );
    }
    for (&id, placement) in &candidate.placements {
        if placement.id != id {
            return mismatch(
                StructuralMismatchKind::PrimitiveSet,
                StableStructuralId::Primitive(id),
            );
        }
        validate_primitive_placement(authority.primitives[&id].primitive, placement).map_err(
            |kind| CertificationError::StructuralMismatch {
                kind,
                affected: StableStructuralId::Primitive(id),
            },
        )?;
    }
    Ok(())
}

fn validate_primitive_placement(
    primitive: Primitive,
    placement: &PrimitivePlacement,
) -> Result<(), StructuralMismatchKind> {
    let variants = physical::variants(primitive);
    let selected_index = usize::from(placement.variant);
    let facing_index = usize::from(placement.facing.index());
    let Some(selected) = variants.get(selected_index) else {
        return Err(StructuralMismatchKind::PrimitiveVariant);
    };
    let Some(facing_variant) = variants.get(facing_index) else {
        return Err(StructuralMismatchKind::PrimitiveFacing);
    };

    if selected_index != facing_index {
        if primitive_footprint_matches(placement, facing_variant) {
            return Err(StructuralMismatchKind::PrimitiveVariant);
        }
        if primitive_footprint_matches(placement, selected) {
            return Err(StructuralMismatchKind::PrimitiveFacing);
        }
        return Err(StructuralMismatchKind::PrimitiveFootprint);
    }

    if placement.blocks.len() != selected.blocks.len() {
        return Err(StructuralMismatchKind::PrimitiveFootprint);
    }
    let mut actual = BTreeMap::new();
    for block in &placement.blocks {
        if actual.insert(block.at, &block.state).is_some() {
            return Err(StructuralMismatchKind::PrimitiveFootprint);
        }
    }
    for expected in selected.blocks {
        let Some(at) = absolute_block_position(placement, expected) else {
            return Err(StructuralMismatchKind::PrimitiveFootprint);
        };
        let Some(state) = actual.get(&at) else {
            return Err(StructuralMismatchKind::PrimitiveFootprint);
        };
        if state.kind != expected.kind {
            return Err(StructuralMismatchKind::PrimitiveKind);
        }
        if state.facing != expected.facing {
            return Err(StructuralMismatchKind::PrimitiveFacing);
        }
        if state.face != expected.face {
            return Err(StructuralMismatchKind::PrimitiveFootprint);
        }
    }
    Ok(())
}

fn primitive_footprint_matches(placement: &PrimitivePlacement, variant: &PhysicalVariant) -> bool {
    if placement.blocks.len() != variant.blocks.len() {
        return false;
    }
    let actual = placement
        .blocks
        .iter()
        .map(|block| (block.at, &block.state))
        .collect::<BTreeMap<_, _>>();
    actual.len() == placement.blocks.len()
        && variant.blocks.iter().all(|expected| {
            absolute_block_position(placement, expected).is_some_and(|at| {
                actual.get(&at).is_some_and(|state| {
                    state.kind == expected.kind
                        && state.facing == expected.facing
                        && state.face == expected.face
                })
            })
        })
}

fn absolute_block_position(
    placement: &PrimitivePlacement,
    block: &LocalBlock,
) -> Option<crate::compile::geometry::Anchor> {
    Some(crate::compile::geometry::Anchor {
        x: placement.anchor.x.checked_add(block.position.x)?,
        y: placement.anchor.y.checked_add(block.position.y)?,
        z: placement.anchor.z.checked_add(block.position.z)?,
    })
}

fn validate_connections(
    candidate: &ExpandedPhysicalCandidate,
    authority: &Authority,
) -> CertificationResult<()> {
    let expected = authority
        .connections
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    let actual = candidate
        .connections
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    if let Some(id) = expected.symmetric_difference(&actual).next().copied() {
        return connection_mismatch(id);
    }
    for (&id, &(instance, specification)) in &authority.connections {
        let binding = &candidate.connections[&id];
        let expected_source = expected_connection_source(candidate, instance, specification)?;
        if binding.id != id
            || binding.source != expected_source
            || binding.landing != PhysicalEndpointId::Landing(id)
            || binding.sink.route != binding.route
        {
            return connection_mismatch(id);
        }
        let Some(route) = candidate.routes.get(&binding.route) else {
            return connection_mismatch(id);
        };
        if route.source != expected_source
            || route
                .branches
                .iter()
                .filter(|branch| {
                    branch.sink == binding.sink && branch.target == RouteTarget::Connection(id)
                })
                .count()
                != 1
        {
            return connection_mismatch(id);
        }
    }
    let realised = candidate
        .routes
        .values()
        .flat_map(|route| route.branches.iter())
        .filter_map(|branch| match branch.target {
            RouteTarget::Connection(id) => Some(id),
            RouteTarget::DeclaredOutput(_) => None,
        })
        .collect::<Vec<_>>();
    if realised.len() != expected.len() {
        let affected = expected
            .iter()
            .find(|id| realised.iter().filter(|actual| actual == id).count() != 1)
            .copied()
            .or_else(|| realised.iter().find(|id| !expected.contains(id)).copied())
            .unwrap_or_else(|| *expected.iter().next().expect("a count mismatch has an ID"));
        return connection_mismatch(affected);
    }
    Ok(())
}

fn connection_mismatch<T>(id: ConnectionId) -> CertificationResult<T> {
    mismatch(
        match id {
            ConnectionId::External { .. } => StructuralMismatchKind::ExternalSink,
            ConnectionId::Internal { .. } => StructuralMismatchKind::ConnectionTopology,
        },
        StableStructuralId::Connection(id),
    )
}

fn expected_connection_source(
    candidate: &ExpandedPhysicalCandidate,
    instance: InstanceId,
    specification: ConnectionSpec,
) -> CertificationResult<PhysicalEndpointId> {
    match specification.source {
        ConnectionSource::Primitive(primitive) => {
            Ok(PhysicalEndpointId::PrimitiveOutput(primitive))
        }
        ConnectionSource::ExternalInput { input_index } => candidate
            .instances
            .assignments
            .iter()
            .find(|assignment| {
                assignment.sink
                    == PhysicalSink::InstanceInput {
                        instance,
                        input_index,
                    }
            })
            .and_then(|assignment| endpoint_for_driver(&assignment.driver))
            .ok_or(CertificationError::StructuralMismatch {
                kind: StructuralMismatchKind::ExternalSink,
                affected: StableStructuralId::Connection(specification.id),
            }),
    }
}

fn validate_junctions(
    candidate: &ExpandedPhysicalCandidate,
    authority: &Authority,
) -> CertificationResult<()> {
    let expected_ids = authority
        .outputs
        .iter()
        .filter_map(|(&instance, output)| {
            matches!(output, OutputSpec::Junction { .. }).then_some(instance)
        })
        .collect::<BTreeSet<_>>();
    let actual_ids = candidate.junctions.keys().copied().collect::<BTreeSet<_>>();
    if let Some(instance) = expected_ids
        .symmetric_difference(&actual_ids)
        .next()
        .copied()
    {
        return mismatch(
            StructuralMismatchKind::JunctionContributor,
            StableStructuralId::Instance(instance),
        );
    }
    for &instance in &expected_ids {
        let OutputSpec::Junction {
            logical_owner,
            contributors,
        } = &authority.outputs[&instance]
        else {
            unreachable!()
        };
        if *logical_owner != instance {
            return mismatch(
                StructuralMismatchKind::JunctionContributor,
                StableStructuralId::Instance(instance),
            );
        }
        let junction = &candidate.junctions[&instance];
        let expected = contributors
            .iter()
            .copied()
            .map(contributor_endpoint)
            .collect::<Vec<_>>();
        if junction.id != instance || junction.contributors != expected {
            let affected = expected
                .iter()
                .find(|endpoint| {
                    junction
                        .contributors
                        .iter()
                        .filter(|actual| actual == endpoint)
                        .count()
                        != 1
                })
                .copied()
                .or_else(|| {
                    junction
                        .contributors
                        .iter()
                        .find(|endpoint| !expected.contains(endpoint))
                        .copied()
                })
                .unwrap_or(PhysicalEndpointId::Junction(instance));
            return mismatch(
                StructuralMismatchKind::JunctionContributor,
                StableStructuralId::Endpoint(affected),
            );
        }
        if !junction.cells.iter().any(|block| block.at == junction.at) {
            return mismatch(
                StructuralMismatchKind::JunctionContributor,
                StableStructuralId::Instance(instance),
            );
        }
        for &endpoint in &expected {
            if contributor_realisation_count(candidate, endpoint) != 1 {
                return mismatch(
                    StructuralMismatchKind::JunctionContributor,
                    StableStructuralId::Endpoint(endpoint),
                );
            }
        }
    }
    Ok(())
}

fn contributor_endpoint(contributor: ContributorSpec) -> PhysicalEndpointId {
    match contributor {
        ContributorSpec::Landing(connection) => PhysicalEndpointId::Landing(connection),
        ContributorSpec::Primitive(primitive) => PhysicalEndpointId::PrimitiveOutput(primitive),
    }
}

fn contributor_realisation_count(
    candidate: &ExpandedPhysicalCandidate,
    endpoint: PhysicalEndpointId,
) -> usize {
    match endpoint {
        PhysicalEndpointId::Landing(connection) => candidate
            .connections
            .get(&connection)
            .into_iter()
            .flat_map(|binding| {
                candidate
                    .routes
                    .get(&binding.route)
                    .into_iter()
                    .flat_map(move |route| {
                        route.branches.iter().filter(move |branch| {
                            branch.sink == binding.sink
                                && branch.target == RouteTarget::Connection(connection)
                        })
                    })
            })
            .count(),
        PhysicalEndpointId::PrimitiveOutput(primitive) => usize::from(
            candidate.placements.contains_key(&primitive)
                && candidate
                    .observations
                    .contains_key(&ObservationId::PrimitiveOutput(primitive)),
        ),
        _ => 0,
    }
}

fn validate_declared_outputs(candidate: &ExpandedPhysicalCandidate) -> CertificationResult<()> {
    for &port in &candidate.instances.declared_outputs {
        let endpoint = PhysicalEndpointId::DeclaredOutput(port);
        let Some(assignment) = candidate
            .instances
            .assignments
            .iter()
            .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
        else {
            return mismatch(
                StructuralMismatchKind::DeclaredOutput,
                StableStructuralId::DeclaredOutput(port),
            );
        };
        let Some(source) = endpoint_for_driver(&assignment.driver) else {
            return mismatch(
                StructuralMismatchKind::DeclaredOutput,
                StableStructuralId::DeclaredOutput(port),
            );
        };
        let routes = candidate
            .routes
            .values()
            .flat_map(|route| route.branches.iter().map(move |branch| (route, branch)))
            .filter(|(route, branch)| {
                route.source == source && branch.target == RouteTarget::DeclaredOutput(port)
            })
            .count();
        if !candidate.boundaries.contains_key(&endpoint)
            || !candidate
                .observations
                .contains_key(&ObservationId::DeclaredOutput(port))
            || routes != 1
        {
            return mismatch(
                StructuralMismatchKind::DeclaredOutput,
                StableStructuralId::DeclaredOutput(port),
            );
        }
    }
    let expected = candidate
        .instances
        .declared_outputs
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if let Some(port) = candidate
        .routes
        .values()
        .flat_map(|route| route.branches.iter())
        .filter_map(|branch| match branch.target {
            RouteTarget::DeclaredOutput(port) => Some(port),
            RouteTarget::Connection(_) => None,
        })
        .find(|port| !expected.contains(port))
    {
        return mismatch(
            StructuralMismatchKind::DeclaredOutput,
            StableStructuralId::DeclaredOutput(port),
        );
    }
    Ok(())
}

fn validate_observations(
    candidate: &ExpandedPhysicalCandidate,
    authority: &Authority,
) -> CertificationResult<()> {
    let mut expected = candidate
        .instances
        .primary_inputs
        .iter()
        .copied()
        .map(ObservationId::PrimaryInput)
        .chain(
            candidate
                .instances
                .declared_outputs
                .iter()
                .copied()
                .map(ObservationId::DeclaredOutput),
        )
        .collect::<BTreeSet<_>>();
    for &instance in authority.instances.keys() {
        expected.insert(ObservationId::InstanceOutput(instance));
    }
    for &primitive in authority.primitives.keys() {
        expected.insert(ObservationId::PrimitiveOutput(primitive));
    }
    for (&instance, output) in &authority.outputs {
        if matches!(output, OutputSpec::Junction { .. }) {
            expected.insert(ObservationId::JunctionOutput(instance));
        }
    }
    let actual = candidate
        .observations
        .keys()
        .copied()
        .collect::<BTreeSet<_>>();
    if let Some(id) = expected.symmetric_difference(&actual).next().copied() {
        return mismatch(
            StructuralMismatchKind::Observation,
            StableStructuralId::Observation(id),
        );
    }
    for &id in &expected {
        let observation = &candidate.observations[&id];
        if observation.site.id != id || !observation_matches(candidate, authority, observation) {
            return mismatch(
                StructuralMismatchKind::Observation,
                StableStructuralId::Observation(id),
            );
        }
    }
    Ok(())
}

fn observation_matches(
    candidate: &ExpandedPhysicalCandidate,
    authority: &Authority,
    observation: &VerifiedObservation,
) -> bool {
    match observation.site.id {
        ObservationId::PrimaryInput(port) => {
            observation.site.logical_owner.is_none()
                && observation_matches_boundary(
                    candidate,
                    PhysicalEndpointId::PrimaryInput(port),
                    observation,
                )
        }
        ObservationId::PrimitiveOutput(primitive) => {
            observation.site.logical_owner == Some(primitive.instance)
                && candidate
                    .placements
                    .get(&primitive)
                    .is_some_and(|placement| {
                        observation_matches_blocks(observation, &placement.blocks)
                    })
        }
        ObservationId::InstanceOutput(instance) => {
            if observation.site.logical_owner != Some(instance) {
                return false;
            }
            match &authority.outputs[&instance] {
                OutputSpec::Primitive(primitive) => candidate
                    .observations
                    .get(&ObservationId::PrimitiveOutput(*primitive))
                    .is_some_and(|expected| same_observation_point(observation, expected)),
                OutputSpec::Junction { .. } => candidate
                    .observations
                    .get(&ObservationId::JunctionOutput(instance))
                    .is_some_and(|expected| same_observation_point(observation, expected)),
            }
        }
        ObservationId::JunctionOutput(instance) => {
            observation.site.logical_owner == Some(instance)
                && candidate.junctions.get(&instance).is_some_and(|junction| {
                    observation.site.at == junction.at
                        && observation_matches_blocks(observation, &junction.cells)
                })
        }
        ObservationId::DeclaredOutput(port) => {
            observation.site.logical_owner == declared_output_owner(candidate, port)
                && observation_matches_boundary(
                    candidate,
                    PhysicalEndpointId::DeclaredOutput(port),
                    observation,
                )
        }
    }
}

fn declared_output_owner(
    candidate: &ExpandedPhysicalCandidate,
    port: PortId,
) -> Option<InstanceId> {
    use crate::compile::fragment_synth::instance_graph::{InstanceDriver, PhysicalDriver};

    let assignment = candidate
        .instances
        .assignments
        .iter()
        .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))?;
    match &assignment.driver {
        PhysicalDriver::PrimaryInput(_) => None,
        PhysicalDriver::Instance(InstanceDriver::Primitive { logical_owner, .. })
        | PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
            Some(*logical_owner)
        }
    }
}

fn observation_matches_boundary(
    candidate: &ExpandedPhysicalCandidate,
    endpoint: PhysicalEndpointId,
    observation: &VerifiedObservation,
) -> bool {
    if candidate
        .boundaries
        .get(&endpoint)
        .is_some_and(|boundary| observation_matches_blocks(observation, &boundary.blocks))
    {
        return true;
    }
    candidate.pin_contracts.get(&endpoint).is_some_and(|pin| {
        observation.site.at == pin.at && observation.state.kind == BlockKind::Air
    })
}

fn observation_matches_blocks(
    observation: &VerifiedObservation,
    blocks: &[crate::compile::fragment_synth::candidate::PlacedBlock],
) -> bool {
    blocks
        .iter()
        .any(|block| block.at == observation.site.at && block.state == observation.state)
}

fn same_observation_point(left: &VerifiedObservation, right: &VerifiedObservation) -> bool {
    left.site.at == right.site.at && left.state == right.state
}

fn physical_ownership_error(
    candidate: &ExpandedPhysicalCandidate,
    _error: CandidateError,
) -> CertificationError {
    let affected = candidate
        .placements
        .keys()
        .next()
        .copied()
        .map(StableStructuralId::Primitive)
        .or_else(|| {
            candidate
                .instances
                .instances
                .first()
                .map(|instance| StableStructuralId::Instance(instance.id))
        })
        .unwrap_or(StableStructuralId::DeclaredOutput(PortId(0)));
    CertificationError::StructuralMismatch {
        kind: StructuralMismatchKind::PhysicalOwnership,
        affected,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::compile::fragment_synth::candidate::{
        BoundaryPlacement, ConnectionBinding, DelayedComponent, DelayedOwner,
        ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedJunction,
        RealisedRouteBranch, RealisedRouteTree, RouteTarget, TerminalRecord, VerifiedObservation,
    };
    use crate::compile::fragment_synth::identity::{
        ConnectionId, ImplementationKey, InputMask, InstanceId, ObservationId, ObservationSite,
        PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::instance_graph::{
        InstanceDriver, InstanceGraph, PhysicalDriver, PhysicalSink,
    };
    use crate::compile::fragment_synth::topology::{ConnectionSource, ContributorSpec, OutputSpec};
    use crate::compile::geometry::{Anchor, CellFacing};
    use crate::compile::physical;
    use crate::compile::planner::{PortPlacements, PortRole, RouteTerminalKind};
    use crate::compile::topology::{GateKind, Library, Primitive};
    use crate::compile::{Gate, Netlist};
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

    use super::{
        certify_expanded_structure, CertificationError, StableStructuralId, StructuralMismatchKind,
    };

    struct Fixture {
        netlist: Netlist,
        library: Library,
        candidate: ExpandedPhysicalCandidate,
        internal: ConnectionId,
        external: ConnectionId,
        buf_first: PrimitiveId,
        merge_repeater: PrimitiveId,
        merge: InstanceId,
    }

    fn state(kind: BlockKind, facing: Option<Facing>) -> BlockState {
        let mut state = BlockState::air();
        state.kind = kind;
        state.name = match kind {
            BlockKind::Torch => "minecraft:redstone_torch",
            BlockKind::WallTorch => "minecraft:redstone_wall_torch",
            BlockKind::Repeater => "minecraft:repeater",
            BlockKind::Comparator => "minecraft:comparator",
            BlockKind::Lever => "minecraft:lever",
            BlockKind::Lamp => "minecraft:redstone_lamp",
            BlockKind::RedstoneWire => "minecraft:redstone_wire",
            _ => "minecraft:stone",
        }
        .to_string();
        state.facing = facing;
        state
    }

    fn fixture_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: vec!["y".into(), "spy".into()],
            gates: vec![
                Gate {
                    name: "buf".into(),
                    inputs: vec!["a".into()],
                    output: "mid".into(),
                    kind: GateKind::Buf,
                },
                Gate::nor("shared", &["b"]),
                Gate::merge("y", &["mid", "shared"]),
                Gate::nor("spy", &["shared"]),
            ],
        }
    }

    fn endpoint_for_driver(driver: &PhysicalDriver) -> PhysicalEndpointId {
        match driver {
            PhysicalDriver::PrimaryInput(port) => PhysicalEndpointId::PrimaryInput(*port),
            PhysicalDriver::Instance(InstanceDriver::Primitive { terminals, .. }) => {
                PhysicalEndpointId::PrimitiveOutput(terminals[0])
            }
            PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
                PhysicalEndpointId::Junction(*logical_owner)
            }
        }
    }

    fn observation_id(endpoint: PhysicalEndpointId) -> ObservationId {
        match endpoint {
            PhysicalEndpointId::PrimaryInput(port) => ObservationId::PrimaryInput(port),
            PhysicalEndpointId::PrimitiveOutput(primitive) => {
                ObservationId::PrimitiveOutput(primitive)
            }
            PhysicalEndpointId::Junction(instance) => ObservationId::JunctionOutput(instance),
            other => panic!("{other:?} is not a route source"),
        }
    }

    fn contributor_endpoint(contributor: ContributorSpec) -> PhysicalEndpointId {
        match contributor {
            ContributorSpec::Landing(connection) => PhysicalEndpointId::Landing(connection),
            ContributorSpec::Primitive(primitive) => PhysicalEndpointId::PrimitiveOutput(primitive),
        }
    }

    fn authoritative_placement(
        id: PrimitiveId,
        primitive: Primitive,
        anchor: Anchor,
    ) -> (PrimitivePlacement, PlacedBlock) {
        let facing = CellFacing::NORTH;
        let variant = &physical::variants(primitive)[usize::from(facing.index())];
        let blocks = variant
            .blocks
            .iter()
            .map(|local| {
                let mut block_state = state(local.kind, local.facing);
                block_state.face = local.face;
                PlacedBlock {
                    at: Anchor {
                        x: anchor.x + local.position.x,
                        y: anchor.y + local.position.y,
                        z: anchor.z + local.position.z,
                    },
                    state: block_state,
                }
            })
            .collect::<Vec<_>>();
        let output = blocks
            .iter()
            .find(|block| match primitive {
                Primitive::Torch => block.state.kind == BlockKind::WallTorch,
                Primitive::Repeater => block.state.kind == BlockKind::Repeater,
                Primitive::Comparator => block.state.kind == BlockKind::Comparator,
                Primitive::Lever => block.state.kind == BlockKind::Lever,
                Primitive::Lamp => block.state.kind == BlockKind::Lamp,
            })
            .cloned()
            .expect("a physical primitive variant has its functional block");
        (
            PrimitivePlacement {
                id,
                variant: u16::from(facing.index()),
                facing,
                anchor,
                delayed: (primitive == Primitive::Repeater).then_some(DelayedComponent {
                    at: output.at,
                    owner: DelayedOwner::Primitive(id),
                }),
                blocks,
            },
            output,
        )
    }

    fn make_fixture() -> Fixture {
        let netlist = fixture_netlist();
        let library = Library::default_library();
        let graph = InstanceGraph::one_to_one(&netlist, &library).unwrap();
        let internal = graph.instances[0]
            .expanded
            .topology
            .connections
            .iter()
            .find(|connection| matches!(connection.id, ConnectionId::Internal { .. }))
            .unwrap()
            .id;
        let external = graph.instances[0].expanded.topology.connections[0].id;
        let buf_first = graph.instances[0].expanded.topology.primitives[0].id;
        let merge_instance = graph
            .instances
            .iter()
            .find(|instance| matches!(instance.implementation, ImplementationKey::Merge { .. }))
            .unwrap();
        assert_eq!(
            merge_instance.implementation,
            ImplementationKey::Merge {
                isolation_mask: InputMask::new(0b10)
            },
            "the fixture must contain one bare and one isolated merge input"
        );
        let merge = merge_instance.id;
        let merge_repeater = merge_instance.expanded.topology.primitives[0].id;

        let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
        let mut next_x = 20;
        for instance in &candidate.instances.instances {
            for primitive in &instance.expanded.topology.primitives {
                let at = Anchor {
                    x: next_x,
                    y: 4,
                    z: 20,
                };
                next_x += 12;
                let (placement, block) =
                    authoritative_placement(primitive.id, primitive.primitive, at);
                candidate.placements.insert(primitive.id, placement);
                candidate.observations.insert(
                    ObservationId::PrimitiveOutput(primitive.id),
                    VerifiedObservation {
                        site: ObservationSite {
                            id: ObservationId::PrimitiveOutput(primitive.id),
                            at: block.at,
                            logical_owner: Some(instance.id),
                            display_label: None,
                        },
                        state: block.state,
                    },
                );
            }
        }

        for &port in &candidate.instances.primary_inputs {
            let endpoint = PhysicalEndpointId::PrimaryInput(port);
            let at = Anchor {
                x: 4 + i32::try_from(port.0).unwrap() * 8,
                y: 4,
                z: 4,
            };
            let block = PlacedBlock {
                at,
                state: state(BlockKind::Lever, None),
            };
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed: None,
                    blocks: vec![block.clone()],
                },
            );
            candidate.observations.insert(
                ObservationId::PrimaryInput(port),
                VerifiedObservation {
                    site: ObservationSite {
                        id: ObservationId::PrimaryInput(port),
                        at,
                        logical_owner: None,
                        display_label: None,
                    },
                    state: block.state,
                },
            );
        }

        for instance in &candidate.instances.instances {
            match &instance.expanded.topology.output {
                OutputSpec::Primitive(primitive) => {
                    let primitive_observation =
                        candidate.observations[&ObservationId::PrimitiveOutput(*primitive)].clone();
                    candidate.observations.insert(
                        ObservationId::InstanceOutput(instance.id),
                        VerifiedObservation {
                            site: ObservationSite {
                                id: ObservationId::InstanceOutput(instance.id),
                                ..primitive_observation.site
                            },
                            state: primitive_observation.state,
                        },
                    );
                }
                OutputSpec::Junction { contributors, .. } => {
                    let at = Anchor {
                        x: 20 + i32::try_from(instance.id.0).unwrap() * 12,
                        y: 4,
                        z: 60,
                    };
                    let block = PlacedBlock {
                        at,
                        state: state(BlockKind::RedstoneWire, None),
                    };
                    candidate.junctions.insert(
                        instance.id,
                        RealisedJunction {
                            id: instance.id,
                            at,
                            facing: CellFacing::NORTH,
                            contributors: contributors
                                .iter()
                                .copied()
                                .map(contributor_endpoint)
                                .collect(),
                            cells: vec![block.clone()],
                        },
                    );
                    for id in [
                        ObservationId::JunctionOutput(instance.id),
                        ObservationId::InstanceOutput(instance.id),
                    ] {
                        candidate.observations.insert(
                            id,
                            VerifiedObservation {
                                site: ObservationSite {
                                    id,
                                    at,
                                    logical_owner: Some(instance.id),
                                    display_label: None,
                                },
                                state: block.state.clone(),
                            },
                        );
                    }
                }
            }
        }

        let mut route_number = 0u32;
        let instances = candidate.instances.instances.clone();
        for instance in &instances {
            for specification in &instance.expanded.topology.connections {
                let source = match specification.source {
                    ConnectionSource::Primitive(primitive) => {
                        PhysicalEndpointId::PrimitiveOutput(primitive)
                    }
                    ConnectionSource::ExternalInput { input_index } => {
                        let assignment = candidate
                            .instances
                            .assignments
                            .iter()
                            .find(|assignment| {
                                assignment.sink
                                    == PhysicalSink::InstanceInput {
                                        instance: instance.id,
                                        input_index,
                                    }
                            })
                            .unwrap();
                        endpoint_for_driver(&assignment.driver)
                    }
                };
                add_connection_route(
                    &mut candidate,
                    specification.id,
                    source,
                    RouteId(route_number),
                );
                route_number += 1;
            }
        }

        for &port in &candidate.instances.declared_outputs.clone() {
            let assignment = candidate
                .instances
                .assignments
                .iter()
                .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                .unwrap();
            let source = endpoint_for_driver(&assignment.driver);
            let source_at = candidate.observations[&observation_id(source)].site.at;
            let terminal_at = Anchor {
                x: source_at.x,
                y: source_at.y,
                z: source_at.z - 1,
            };
            let lamp_at = Anchor {
                z: source_at.z - 2,
                ..source_at
            };
            let endpoint = PhysicalEndpointId::DeclaredOutput(port);
            let block = PlacedBlock {
                at: lamp_at,
                state: state(BlockKind::Lamp, None),
            };
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed: None,
                    blocks: vec![block.clone()],
                },
            );
            candidate.observations.insert(
                ObservationId::DeclaredOutput(port),
                VerifiedObservation {
                    site: ObservationSite {
                        id: ObservationId::DeclaredOutput(port),
                        at: lamp_at,
                        logical_owner: assignment.driver.clone().into_instance_owner(),
                        display_label: None,
                    },
                    state: block.state.clone(),
                },
            );
            let route = RouteId(route_number);
            route_number += 1;
            let sink = RoutedSinkId { route, ordinal: 0 };
            candidate.routes.insert(
                route,
                RealisedRouteTree {
                    id: route,
                    source,
                    cells: vec![PlacedBlock {
                        at: terminal_at,
                        state: state(BlockKind::RedstoneWire, None),
                    }],
                    floors: Vec::new(),
                    branches: vec![RealisedRouteBranch {
                        sink,
                        target: RouteTarget::DeclaredOutput(port),
                        root: source_at,
                        path: vec![source_at, terminal_at],
                        terminal: TerminalRecord {
                            sink,
                            at: terminal_at,
                            state: state(BlockKind::RedstoneWire, None),
                            kind: RouteTerminalKind::DirectedDustIntoSupport,
                            repeaters: 0,
                            delayed_owner: None,
                        },
                    }],
                },
            );
        }

        Fixture {
            netlist,
            library,
            candidate,
            internal,
            external,
            buf_first,
            merge_repeater,
            merge,
        }
    }

    fn pinned_input_only_fixture() -> (Netlist, Library, ExpandedPhysicalCandidate) {
        let netlist = Netlist {
            inputs: vec!["a".into(), "b".into()],
            outputs: Vec::new(),
            gates: Vec::new(),
        };
        let library = Library::default_library();
        let graph = InstanceGraph::one_to_one(&netlist, &library).unwrap();
        let mut candidate = ExpandedPhysicalCandidate::empty(graph, PortPlacements::default());
        for (index, name) in netlist.inputs.iter().enumerate() {
            let port = PortId(u32::try_from(index).unwrap());
            let endpoint = PhysicalEndpointId::PrimaryInput(port);
            let caller = Anchor {
                x: 10 + i32::try_from(index).unwrap() * 10,
                y: 4,
                z: 10,
            };
            candidate.pins.pin(name, caller, Facing::North);
            let pin = candidate.pins.get(name).unwrap();
            let handover = pin.handover(PortRole::Input);
            let mut repeater = state(BlockKind::Repeater, Some(pin.toward.opposite()));
            repeater.delay = 1;
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed: Some(DelayedComponent {
                        at: handover,
                        owner: DelayedOwner::InputBinding(port),
                    }),
                    blocks: vec![PlacedBlock {
                        at: handover,
                        state: repeater,
                    }],
                },
            );
            candidate.observations.insert(
                ObservationId::PrimaryInput(port),
                VerifiedObservation {
                    site: ObservationSite {
                        id: ObservationId::PrimaryInput(port),
                        at: caller,
                        logical_owner: None,
                        display_label: Some(name.clone()),
                    },
                    state: BlockState::air(),
                },
            );
        }
        candidate.bind_pin_contracts(&netlist).unwrap();
        (netlist, library, candidate)
    }

    fn swap_primary_input_endpoint_metadata(candidate: &mut ExpandedPhysicalCandidate) {
        let first = PhysicalEndpointId::PrimaryInput(PortId(0));
        let second = PhysicalEndpointId::PrimaryInput(PortId(1));
        let first_pin = candidate.pin_contracts.remove(&first).unwrap();
        let second_pin = candidate.pin_contracts.remove(&second).unwrap();
        candidate.pin_contracts.insert(first, second_pin);
        candidate.pin_contracts.insert(second, first_pin);
        candidate.pin_name_bindings.insert("a".into(), second);
        candidate.pin_name_bindings.insert("b".into(), first);

        let mut first_boundary = candidate.boundaries.remove(&first).unwrap();
        let mut second_boundary = candidate.boundaries.remove(&second).unwrap();
        first_boundary.endpoint = second;
        first_boundary.delayed.as_mut().unwrap().owner = DelayedOwner::InputBinding(PortId(1));
        second_boundary.endpoint = first;
        second_boundary.delayed.as_mut().unwrap().owner = DelayedOwner::InputBinding(PortId(0));
        candidate.boundaries.insert(first, second_boundary);
        candidate.boundaries.insert(second, first_boundary);

        let first_id = ObservationId::PrimaryInput(PortId(0));
        let second_id = ObservationId::PrimaryInput(PortId(1));
        let mut first_observation = candidate.observations.remove(&first_id).unwrap();
        let mut second_observation = candidate.observations.remove(&second_id).unwrap();
        first_observation.site.id = second_id;
        second_observation.site.id = first_id;
        candidate.observations.insert(first_id, second_observation);
        candidate.observations.insert(second_id, first_observation);
    }

    trait DriverOwner {
        fn into_instance_owner(self) -> Option<InstanceId>;
    }

    impl DriverOwner for PhysicalDriver {
        fn into_instance_owner(self) -> Option<InstanceId> {
            match self {
                PhysicalDriver::PrimaryInput(_) => None,
                PhysicalDriver::Instance(InstanceDriver::Primitive { logical_owner, .. })
                | PhysicalDriver::Instance(InstanceDriver::Junction { logical_owner, .. }) => {
                    Some(logical_owner)
                }
            }
        }
    }

    fn add_connection_route(
        candidate: &mut ExpandedPhysicalCandidate,
        connection: ConnectionId,
        source: PhysicalEndpointId,
        route: RouteId,
    ) {
        let source_at = candidate.observations[&observation_id(source)].site.at;
        let ordinal = match connection {
            ConnectionId::External { input_index, .. } => input_index,
            ConnectionId::Internal { edge_index, .. } => edge_index + 3,
        };
        let at = Anchor {
            x: source_at.x + 1,
            y: source_at.y + i32::from(ordinal),
            z: source_at.z,
        };
        let root = Anchor {
            x: source_at.x,
            y: at.y,
            z: source_at.z,
        };
        let mut path = vec![source_at];
        for y in (source_at.y + 1)..=root.y {
            path.push(Anchor {
                x: source_at.x,
                y,
                z: source_at.z,
            });
        }
        path.push(at);
        let cells = path
            .iter()
            .skip(1)
            .copied()
            .map(|at| PlacedBlock {
                at,
                state: state(BlockKind::RedstoneWire, None),
            })
            .collect::<Vec<_>>();
        let sink = RoutedSinkId { route, ordinal: 0 };
        candidate.connections.insert(
            connection,
            ConnectionBinding {
                id: connection,
                source,
                landing: PhysicalEndpointId::Landing(connection),
                route,
                sink,
            },
        );
        candidate.routes.insert(
            route,
            RealisedRouteTree {
                id: route,
                source,
                cells,
                floors: Vec::new(),
                branches: vec![RealisedRouteBranch {
                    sink,
                    target: RouteTarget::Connection(connection),
                    root: source_at,
                    path,
                    terminal: TerminalRecord {
                        sink,
                        at,
                        state: state(BlockKind::RedstoneWire, None),
                        kind: RouteTerminalKind::DirectedDustIntoSupport,
                        repeaters: 0,
                        delayed_owner: None,
                    },
                }],
            },
        );
    }

    fn assert_mismatch(
        candidate: &ExpandedPhysicalCandidate,
        fixture: &Fixture,
        kind: StructuralMismatchKind,
        affected: StableStructuralId,
    ) {
        assert_eq!(
            certify_expanded_structure(candidate, &fixture.netlist, &fixture.library),
            Err(CertificationError::StructuralMismatch { kind, affected })
        );
    }

    #[test]
    fn independently_reinstantiated_baseline_certifies() {
        let fixture = make_fixture();
        assert_eq!(fixture.candidate.validate_physical_ownership(), Ok(()));
        assert_eq!(fixture.candidate.validate_shape(), Ok(()));
        let certificate =
            certify_expanded_structure(&fixture.candidate, &fixture.netlist, &fixture.library)
                .unwrap();

        assert_eq!(
            certificate.candidate_fingerprint,
            fixture.candidate.fingerprint()
        );
        assert_eq!(
            certificate.library_revision,
            fixture.library.revision_fingerprint()
        );
        assert_eq!(certificate.instance_count, 4);
    }

    #[test]
    fn topology_corruption_matrix_returns_named_stable_ids() {
        let fixture = make_fixture();

        let mut missing_internal = fixture.candidate.clone();
        missing_internal.instances.instances[0]
            .expanded
            .topology
            .connections
            .retain(|connection| connection.id != fixture.internal);
        assert_mismatch(
            &missing_internal,
            &fixture,
            StructuralMismatchKind::ConnectionTopology,
            StableStructuralId::Connection(fixture.internal),
        );

        let mut redirected_internal = fixture.candidate.clone();
        let topology = &mut redirected_internal.instances.instances[0].expanded.topology;
        let wrong_target = topology.primitives[0].id;
        topology
            .connections
            .iter_mut()
            .find(|connection| connection.id == fixture.internal)
            .unwrap()
            .target =
            crate::compile::fragment_synth::topology::ConnectionTarget::Primitive(wrong_target);
        assert_mismatch(
            &redirected_internal,
            &fixture,
            StructuralMismatchKind::ConnectionTopology,
            StableStructuralId::Connection(fixture.internal),
        );

        let mut missing_primitive = fixture.candidate.clone();
        missing_primitive.instances.instances[0]
            .expanded
            .topology
            .primitives
            .retain(|primitive| primitive.id != fixture.buf_first);
        assert_mismatch(
            &missing_primitive,
            &fixture,
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(fixture.buf_first),
        );

        let mut duplicate_primitive = fixture.candidate.clone();
        let duplicate = duplicate_primitive.instances.instances[0]
            .expanded
            .topology
            .primitives[0];
        duplicate_primitive.instances.instances[0]
            .expanded
            .topology
            .primitives
            .push(duplicate);
        assert_mismatch(
            &duplicate_primitive,
            &fixture,
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(duplicate.id),
        );

        let mut changed_kind = fixture.candidate.clone();
        changed_kind.instances.instances[0]
            .expanded
            .topology
            .primitives[0]
            .primitive = Primitive::Repeater;
        assert_mismatch(
            &changed_kind,
            &fixture,
            StructuralMismatchKind::PrimitiveKind,
            StableStructuralId::Primitive(fixture.buf_first),
        );

        let mut wrong_implementation = fixture.candidate.clone();
        wrong_implementation.instances.instances[0].implementation = ImplementationKey::Merge {
            isolation_mask: InputMask::new(0),
        };
        assert_mismatch(
            &wrong_implementation,
            &fixture,
            StructuralMismatchKind::ImplementationTopology,
            StableStructuralId::Instance(InstanceId(0)),
        );

        let mut wrong_topology = fixture.candidate.clone();
        wrong_topology.instances.instances[0]
            .expanded
            .topology
            .output = OutputSpec::Primitive(fixture.buf_first);
        assert_mismatch(
            &wrong_topology,
            &fixture,
            StructuralMismatchKind::ImplementationTopology,
            StableStructuralId::Instance(InstanceId(0)),
        );
    }

    #[test]
    fn candidate_corruption_matrix_returns_named_stable_ids() {
        let fixture = make_fixture();

        let mut missing_primitive = fixture.candidate.clone();
        missing_primitive.placements.remove(&fixture.buf_first);
        assert_mismatch(
            &missing_primitive,
            &fixture,
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(fixture.buf_first),
        );

        let mut duplicate_primitive = fixture.candidate.clone();
        let extra = PrimitiveId {
            instance: InstanceId(99),
            node: TopologyNodeId(0),
        };
        let mut duplicate = duplicate_primitive.placements[&fixture.buf_first].clone();
        duplicate_primitive
            .placements
            .insert(extra, duplicate.clone());
        assert_mismatch(
            &duplicate_primitive,
            &fixture,
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(extra),
        );

        let mut changed_kind = fixture.candidate.clone();
        changed_kind
            .placements
            .get_mut(&fixture.buf_first)
            .unwrap()
            .blocks[0]
            .state = state(BlockKind::Repeater, Some(Facing::North));
        assert_mismatch(
            &changed_kind,
            &fixture,
            StructuralMismatchKind::PrimitiveKind,
            StableStructuralId::Primitive(fixture.buf_first),
        );

        let mut refaced = fixture.candidate.clone();
        refaced
            .placements
            .get_mut(&fixture.merge_repeater)
            .unwrap()
            .facing = CellFacing::SOUTH;
        assert_mismatch(
            &refaced,
            &fixture,
            StructuralMismatchKind::PrimitiveFacing,
            StableStructuralId::Primitive(fixture.merge_repeater),
        );

        let mut reassigned_sink = fixture.candidate.clone();
        reassigned_sink
            .connections
            .get_mut(&fixture.external)
            .unwrap()
            .source = PhysicalEndpointId::PrimaryInput(PortId(1));
        assert_mismatch(
            &reassigned_sink,
            &fixture,
            StructuralMismatchKind::ExternalSink,
            StableStructuralId::Connection(fixture.external),
        );

        let expected_contributors = fixture.candidate.junctions[&fixture.merge]
            .contributors
            .clone();
        let mut missing_contributor = fixture.candidate.clone();
        let omitted = missing_contributor
            .junctions
            .get_mut(&fixture.merge)
            .unwrap()
            .contributors
            .pop()
            .unwrap();
        assert_mismatch(
            &missing_contributor,
            &fixture,
            StructuralMismatchKind::JunctionContributor,
            StableStructuralId::Endpoint(omitted),
        );

        let mut swapped_contributor = fixture.candidate.clone();
        swapped_contributor
            .junctions
            .get_mut(&fixture.merge)
            .unwrap()
            .contributors[0] = PhysicalEndpointId::PrimitiveOutput(fixture.buf_first);
        assert_mismatch(
            &swapped_contributor,
            &fixture,
            StructuralMismatchKind::JunctionContributor,
            StableStructuralId::Endpoint(expected_contributors[0]),
        );

        let output_route = fixture
            .candidate
            .routes
            .iter()
            .find(|(_, route)| {
                route
                    .branches
                    .iter()
                    .any(|branch| branch.target == RouteTarget::DeclaredOutput(PortId(0)))
            })
            .map(|(&id, _)| id)
            .unwrap();
        let mut missing_output = fixture.candidate.clone();
        missing_output.routes.remove(&output_route);
        assert_mismatch(
            &missing_output,
            &fixture,
            StructuralMismatchKind::DeclaredOutput,
            StableStructuralId::DeclaredOutput(PortId(0)),
        );

        let observation = ObservationId::PrimitiveOutput(fixture.buf_first);
        let mut forged_observation = fixture.candidate.clone();
        forged_observation
            .observations
            .get_mut(&observation)
            .unwrap()
            .site
            .at
            .x += 1_000;
        assert_mismatch(
            &forged_observation,
            &fixture,
            StructuralMismatchKind::Observation,
            StableStructuralId::Observation(observation),
        );

        duplicate.id = extra;
        let mut added_primitive = fixture.candidate.clone();
        added_primitive.placements.insert(extra, duplicate);
        assert_mismatch(
            &added_primitive,
            &fixture,
            StructuralMismatchKind::PrimitiveSet,
            StableStructuralId::Primitive(extra),
        );

        assert_eq!(
            fixture
                .candidate
                .placements
                .keys()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            fixture.candidate.placements.len()
        );
    }

    #[test]
    fn primitive_variant_drift_is_rejected() {
        let fixture = make_fixture();
        let mut corrupted = fixture.candidate.clone();
        corrupted
            .placements
            .get_mut(&fixture.buf_first)
            .unwrap()
            .variant = 1;

        assert_mismatch(
            &corrupted,
            &fixture,
            StructuralMismatchKind::PrimitiveVariant,
            StableStructuralId::Primitive(fixture.buf_first),
        );
    }

    #[test]
    fn primitive_anchor_drift_is_rejected() {
        let fixture = make_fixture();
        let mut corrupted = fixture.candidate.clone();
        corrupted
            .placements
            .get_mut(&fixture.buf_first)
            .unwrap()
            .anchor
            .x += 1;

        assert_mismatch(
            &corrupted,
            &fixture,
            StructuralMismatchKind::PrimitiveFootprint,
            StableStructuralId::Primitive(fixture.buf_first),
        );
    }

    #[test]
    fn missing_primitive_implementation_block_is_rejected() {
        let fixture = make_fixture();
        let mut corrupted = fixture.candidate.clone();
        corrupted
            .placements
            .get_mut(&fixture.buf_first)
            .unwrap()
            .blocks
            .retain(|block| block.state.kind != BlockKind::Solid);

        assert_mismatch(
            &corrupted,
            &fixture,
            StructuralMismatchKind::PrimitiveFootprint,
            StableStructuralId::Primitive(fixture.buf_first),
        );
    }

    #[test]
    fn extra_primitive_implementation_block_is_rejected() {
        let fixture = make_fixture();
        let mut corrupted = fixture.candidate.clone();
        corrupted
            .placements
            .get_mut(&fixture.buf_first)
            .unwrap()
            .blocks
            .push(PlacedBlock {
                at: Anchor {
                    x: 900,
                    y: 4,
                    z: 900,
                },
                state: state(BlockKind::Solid, None),
            });

        assert_mismatch(
            &corrupted,
            &fixture,
            StructuralMismatchKind::PrimitiveFootprint,
            StableStructuralId::Primitive(fixture.buf_first),
        );
    }

    #[test]
    fn distant_same_kind_decoy_cannot_impersonate_primitive_output() {
        let fixture = make_fixture();
        let mut corrupted = fixture.candidate.clone();
        let placement = corrupted.placements.get_mut(&fixture.buf_first).unwrap();
        let output = placement
            .blocks
            .iter_mut()
            .find(|block| block.state.kind == BlockKind::WallTorch)
            .unwrap();
        let output_at = output.at;
        let decoy_state = output.state.clone();
        output.state = state(BlockKind::Solid, None);
        placement.blocks.push(PlacedBlock {
            at: Anchor {
                x: 950,
                y: 4,
                z: 950,
            },
            state: decoy_state,
        });
        corrupted
            .observations
            .get_mut(&ObservationId::PrimitiveOutput(fixture.buf_first))
            .unwrap()
            .state = state(BlockKind::Solid, None);
        assert_eq!(
            corrupted.observations[&ObservationId::PrimitiveOutput(fixture.buf_first)]
                .site
                .at,
            output_at
        );

        assert_mismatch(
            &corrupted,
            &fixture,
            StructuralMismatchKind::PrimitiveFootprint,
            StableStructuralId::Primitive(fixture.buf_first),
        );
    }

    #[test]
    fn disconnected_route_metadata_cannot_bypass_shape_and_timing_validation() {
        let fixture = make_fixture();
        let mut corrupted = fixture.candidate.clone();
        let route_id = corrupted.connections[&fixture.internal].route;
        let branch = &mut corrupted.routes.get_mut(&route_id).unwrap().branches[0];
        branch.root = branch.terminal.at;
        branch.path = vec![branch.terminal.at];

        assert_mismatch(
            &corrupted,
            &fixture,
            StructuralMismatchKind::ConnectionTopology,
            StableStructuralId::Route(route_id),
        );
    }

    #[test]
    fn candidate_consistent_pin_name_swap_is_rejected_against_netlist_authority() {
        let (netlist, library, mut candidate) = pinned_input_only_fixture();
        certify_expanded_structure(&candidate, &netlist, &library).unwrap();
        swap_primary_input_endpoint_metadata(&mut candidate);
        candidate.validate_pin_contracts().unwrap();

        assert_eq!(
            certify_expanded_structure(&candidate, &netlist, &library),
            Err(CertificationError::StructuralMismatch {
                kind: StructuralMismatchKind::PinContract,
                affected: StableStructuralId::Endpoint(PhysicalEndpointId::PrimaryInput(PortId(0))),
            })
        );
    }
}
