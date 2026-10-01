//! Durable registration and orchestration for physical certification.
//!
//! Placement policy may choose a candidate, but it must not own the list of
//! physical rules that decide whether that candidate is safe to emit.  This
//! module is the stable authority shared by the legacy planner adapter and the
//! fragment synthesiser.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use serde::Serialize;
use thiserror::Error;

use super::emission::{EmittedWorld, PhysicalBlockRole};
use super::fragment_synth::candidate::ExpandedPhysicalCandidate;
use super::fragment_synth::identity::{
    InstanceId, ObservationId, PhysicalEndpointId, PrimitiveId, RouteId, RoutedSinkId,
};
use super::fragment_synth::legacy_adapter::LegacyCandidateAdapter;
use super::fragment_synth::realise::ExpandedCandidateAdapter;
use super::fragment_synth::topology::ConnectionTarget;
use super::fragment_synth::verify::certify_expanded_structure;
use super::planner::{self, NodeRealisation, PlanCandidate, PlannerError, RealisedCandidate};
use super::routing::route_step_is_legal;
use super::topology::{GateKind, Library, Primitive};
use super::{Net, Netlist, Reservation};
use crate::compile::geometry::Anchor;
use crate::redstone::rules::taxonomy::BlockPower;
use crate::redstone::simulator::connectivity::dust_connections;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum PhysicalVerifierRuleId {
    Collision,
    Coupling,
    Connectivity,
    ObservationEquality,
    RouteContinuity,
    TorchMergeStructure,
    SignalStrength,
    RepeaterDirection,
    TerminalStyle,
    PinHandoverHalo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct PhysicalVerifierRuleRegistration {
    pub id: PhysicalVerifierRuleId,
    pub semantic_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum CandidateVerifierCheckId {
    Collision,
    TerminalContract,
    RouteTerminals,
    RealisedWorld,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct CandidateVerifierCheckRegistration {
    pub check: CandidateVerifierCheckId,
    pub rules: &'static [PhysicalVerifierRuleRegistration],
}

const COLLISION_RULES: [PhysicalVerifierRuleRegistration; 1] = [PhysicalVerifierRuleRegistration {
    id: PhysicalVerifierRuleId::Collision,
    semantic_version: 1,
}];

const TERMINAL_CONTRACT_RULES: [PhysicalVerifierRuleRegistration; 2] = [
    PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::RepeaterDirection,
        semantic_version: 1,
    },
    PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::PinHandoverHalo,
        semantic_version: 1,
    },
];

const TERMINAL_STYLE_RULES: [PhysicalVerifierRuleRegistration; 1] =
    [PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::TerminalStyle,
        semantic_version: 1,
    }];

const REALISED_WORLD_RULES: [PhysicalVerifierRuleRegistration; 0] = [];

const CANDIDATE_VERIFIER_PIPELINE: [CandidateVerifierCheckRegistration; 4] = [
    CandidateVerifierCheckRegistration {
        check: CandidateVerifierCheckId::Collision,
        rules: &COLLISION_RULES,
    },
    CandidateVerifierCheckRegistration {
        check: CandidateVerifierCheckId::TerminalContract,
        rules: &TERMINAL_CONTRACT_RULES,
    },
    CandidateVerifierCheckRegistration {
        check: CandidateVerifierCheckId::RouteTerminals,
        rules: &TERMINAL_STYLE_RULES,
    },
    CandidateVerifierCheckRegistration {
        check: CandidateVerifierCheckId::RealisedWorld,
        rules: &REALISED_WORLD_RULES,
    },
];

pub(crate) fn candidate_verifier_pipeline() -> &'static [CandidateVerifierCheckRegistration] {
    &CANDIDATE_VERIFIER_PIPELINE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum RealisedWorldVerifierCheckId {
    CouplingAndConnectivity,
    TorchMergeStructure,
    SignalStrength,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct RealisedWorldVerifierCheckRegistration {
    pub check: RealisedWorldVerifierCheckId,
    pub rules: &'static [PhysicalVerifierRuleRegistration],
}

const COUPLING_CONNECTIVITY_RULES: [PhysicalVerifierRuleRegistration; 2] = [
    PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::Coupling,
        semantic_version: 1,
    },
    PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::Connectivity,
        semantic_version: 1,
    },
];

const TORCH_MERGE_RULES: [PhysicalVerifierRuleRegistration; 1] =
    [PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::TorchMergeStructure,
        semantic_version: 1,
    }];

const SIGNAL_STRENGTH_RULES: [PhysicalVerifierRuleRegistration; 1] =
    [PhysicalVerifierRuleRegistration {
        id: PhysicalVerifierRuleId::SignalStrength,
        semantic_version: 1,
    }];

pub(crate) const REALISED_WORLD_VERIFIER_PIPELINE: [RealisedWorldVerifierCheckRegistration; 3] = [
    RealisedWorldVerifierCheckRegistration {
        check: RealisedWorldVerifierCheckId::CouplingAndConnectivity,
        rules: &COUPLING_CONNECTIVITY_RULES,
    },
    RealisedWorldVerifierCheckRegistration {
        check: RealisedWorldVerifierCheckId::TorchMergeStructure,
        rules: &TORCH_MERGE_RULES,
    },
    RealisedWorldVerifierCheckRegistration {
        check: RealisedWorldVerifierCheckId::SignalStrength,
        rules: &SIGNAL_STRENGTH_RULES,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PhysicalVerifierRevisionDescriptor {
    pub policy: PhysicalVerifierPolicy,
    pub rules: Vec<PhysicalVerifierRuleRegistration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum PhysicalVerifierPolicy {
    LegacyCompatibility,
    ExpandedStrict,
}

pub(crate) fn physical_verifier_revision_descriptor() -> PhysicalVerifierRevisionDescriptor {
    let mut rules = Vec::new();
    for check in candidate_verifier_pipeline() {
        if check.check == CandidateVerifierCheckId::RealisedWorld {
            for realised_check in REALISED_WORLD_VERIFIER_PIPELINE {
                rules.extend_from_slice(realised_check.rules);
            }
        } else {
            rules.extend_from_slice(check.rules);
        }
    }
    PhysicalVerifierRevisionDescriptor {
        policy: PhysicalVerifierPolicy::LegacyCompatibility,
        rules,
    }
}

pub(crate) fn expanded_strict_physical_verifier_revision_descriptor(
) -> PhysicalVerifierRevisionDescriptor {
    PhysicalVerifierRevisionDescriptor {
        policy: PhysicalVerifierPolicy::ExpandedStrict,
        rules: vec![
            PhysicalVerifierRuleRegistration {
                id: PhysicalVerifierRuleId::ObservationEquality,
                semantic_version: 1,
            },
            PhysicalVerifierRuleRegistration {
                id: PhysicalVerifierRuleId::RouteContinuity,
                semantic_version: 5,
            },
            PhysicalVerifierRuleRegistration {
                id: PhysicalVerifierRuleId::Connectivity,
                semantic_version: 4,
            },
            PhysicalVerifierRuleRegistration {
                id: PhysicalVerifierRuleId::Coupling,
                semantic_version: 6,
            },
        ],
    }
}

/// Run the durable physical-rule sequence over a legacy planner adapter.
///
/// The planner still supplies its representation-specific reservation, net
/// and block materialisation while migration is in progress.  Rule ordering
/// and physical acceptance live here, so `planner::realise_and_verify` is a
/// thin compatibility entry point rather than a second verifier pipeline.
pub(crate) fn verify_legacy_candidate(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    size: (i32, i32, i32),
) -> Result<(RealisedCandidate, Reservation, Vec<Net>), PlannerError> {
    let mut reservation = None;
    let mut nets = None;
    let mut realised = None;

    for registration in candidate_verifier_pipeline() {
        match registration.check {
            CandidateVerifierCheckId::Collision => {
                reservation = Some(planner::verify_spacing(candidate)?);
                nets = Some(planner::verification_nets(candidate, netlist)?);
                realised = Some(planner::emit_candidate(candidate, netlist, size)?);
            }
            CandidateVerifierCheckId::TerminalContract => planner::verify_terminal_contract(
                candidate,
                &realised
                    .as_ref()
                    .expect("collision stage must realise before terminal checks")
                    .world,
                reservation
                    .as_ref()
                    .expect("collision stage must reserve before terminal checks"),
            )?,
            CandidateVerifierCheckId::RouteTerminals => {
                let realised = realised
                    .as_ref()
                    .expect("collision stage must realise before terminal checks");
                let reservation = reservation
                    .as_ref()
                    .expect("collision stage must reserve before terminal checks");
                let nets = nets
                    .as_ref()
                    .expect("collision stage must build nets before terminal checks");
                for (net, route) in candidate.routes().iter().enumerate() {
                    super::verify_route_terminals(
                        &realised.world,
                        reservation,
                        netlist,
                        nets,
                        net,
                        route.id(),
                        route.terminals(),
                    )
                    .map_err(PlannerError::PhysicalInvariant)?;
                }
            }
            CandidateVerifierCheckId::RealisedWorld => {
                let realised = realised
                    .as_ref()
                    .expect("collision stage must realise before world checks");
                super::verify_realised_world(
                    &realised.world,
                    reservation
                        .as_ref()
                        .expect("collision stage must reserve before world checks"),
                    netlist,
                    nets.as_ref()
                        .expect("collision stage must build nets before world checks"),
                    &realised.ports.gate_output_positions,
                    &realised.ports.input_positions,
                    &realised.ports.output_positions,
                )
                .map_err(PlannerError::PhysicalInvariant)?;
            }
        }
    }

    let realised = realised.expect("verifier pipeline must include collision setup");
    if netlist.gates.iter().any(|gate| gate.kind.is_sequential()) {
        verify_stateful_macros(candidate, netlist, &realised.world)?;
        return Ok((
            realised,
            reservation.expect("verifier pipeline must include collision setup"),
            nets.expect("verifier pipeline must include collision setup"),
        ));
    }
    let adapted = LegacyCandidateAdapter::adapt_plan(netlist, candidate, &realised.world)
        .map_err(|error| durable_legacy_error("typed adapter", error))?;
    let library = Library::default_library();
    certify_expanded_structure(&adapted.candidate, netlist, &library)
        .map_err(|error| durable_legacy_error("structural certification", error))?;
    let adapter = ExpandedCandidateAdapter::new(&adapted.candidate)
        .map_err(|error| durable_legacy_error("emission adapter", error))?;
    let durable = super::emission::emit_candidate(&adapter, size)
        .map_err(|error| durable_legacy_error("typed emission", error))?;
    verify_expanded_legacy_compatibility(&adapted.candidate, &durable)
        .map_err(|error| durable_legacy_error("typed physical verification", error))?;
    if let Some(detail) = semantic_world_difference(&durable.world, &realised.world) {
        return Err(durable_legacy_error("emission parity", detail));
    }

    Ok((
        RealisedCandidate {
            world: durable.world,
            ports: realised.ports,
        },
        reservation.expect("verifier pipeline must include collision setup"),
        nets.expect("verifier pipeline must include collision setup"),
    ))
}

fn verify_stateful_macros(
    candidate: &PlanCandidate,
    netlist: &Netlist,
    world: &World,
) -> Result<(), PlannerError> {
    for (gate, definition) in netlist.gates.iter().enumerate() {
        if definition.kind != GateKind::DffPosedge {
            continue;
        }
        let node = &candidate.primitive_nodes()[gate];
        if node.realisation != NodeRealisation::DffPosedge {
            return Err(PlannerError::UnrealisableNode {
                id: node.id.clone(),
                reason: "DFF is not realised by Design H".to_string(),
            });
        }
        let facing = candidate.facing_of(gate);
        let at = |offset: (i32, i32, i32)| {
            let (x, y, z) = super::geometry::rotate(offset, facing);
            Position::new(node.anchor.x + x, node.anchor.y + y, node.anchor.z + z)
        };
        let expected_repeaters = [
            ((0, 0, 0), Facing::North),
            ((0, 0, -1), Facing::North),
            ((1, 0, 0), Facing::West),
            ((1, 0, -1), Facing::West),
        ];
        for (offset, direction) in expected_repeaters {
            let position = at(offset);
            let block = world.get(position.x, position.y, position.z);
            let stored_facing = super::geometry::turn(direction, facing).opposite();
            if block.kind != BlockKind::Repeater || block.facing != Some(stored_facing) {
                return Err(PlannerError::UnrealisableNode {
                    id: node.id.clone(),
                    reason: format!(
                        "Design H repeater at ({}, {}, {}) is {:?} facing {:?}",
                        position.x, position.y, position.z, block.kind, block.facing
                    ),
                });
            }
        }
        let torch = at((2, 0, -1));
        let block = world.get(torch.x, torch.y, torch.z);
        if block.kind != BlockKind::WallTorch
            || block.facing != Some(super::geometry::turn(Facing::North, facing))
        {
            return Err(PlannerError::UnrealisableNode {
                id: node.id.clone(),
                reason: "Design H complementary-clock torch is missing or misoriented".to_string(),
            });
        }
    }
    Ok(())
}

fn durable_legacy_error(stage: &str, error: impl std::fmt::Display) -> PlannerError {
    PlannerError::UnrealisableNode {
        id: "durable-physical-candidate".to_string(),
        reason: format!("{stage} failed: {error}"),
    }
}

fn semantic_world_difference(
    durable: &crate::redstone::world::storage::World,
    legacy: &crate::redstone::world::storage::World,
) -> Option<String> {
    if durable.size() != legacy.size() {
        return Some(format!(
            "world sizes differ: durable {:?}, legacy {:?}",
            durable.size(),
            legacy.size()
        ));
    }
    let (size_x, size_y, size_z) = durable.size();
    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                if durable.get(x, y, z) != legacy.get(x, y, z) {
                    return Some(format!(
                        "first difference at ({x}, {y}, {z}): durable {:?}, legacy {:?}",
                        durable.get(x, y, z),
                        legacy.get(x, y, z)
                    ));
                }
            }
        }
    }
    None
}

/// A typed physical refusal for an expanded candidate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExpandedPhysicalError {
    #[error("comparator {primitive:?} electrical isolation fails at {at:?}")]
    ComparatorIsolation { primitive: PrimitiveId, at: Anchor },
    #[error("observation {observation:?} at {at:?} does not match the emitted block state")]
    ObservationMismatch {
        observation: ObservationId,
        at: Anchor,
    },
    #[error("route terminal {sink:?} at {at:?} is a repeater on the wrong axis")]
    WrongRepeaterAxis { sink: RoutedSinkId, at: Anchor },
    #[error(
        "route {route:?} sink {sink:?} path cell {at:?} is not dust or a repeater ({actual:?})"
    )]
    NonConductingRouteCell {
        route: RouteId,
        sink: RoutedSinkId,
        at: Anchor,
        actual: BlockKind,
    },
    #[error("route {route:?} sink {sink:?} is not physically connected from {from:?} to {to:?}")]
    DisconnectedRouteStep {
        route: RouteId,
        sink: RoutedSinkId,
        from: Anchor,
        to: Anchor,
    },
    #[error("route {sink:?} has an invalid supported repeater handover")]
    InvalidRepeaterHandover { sink: RoutedSinkId },
    #[error("junction {junction:?} claims a disconnected wire cell at {at:?}")]
    DisconnectedJunctionCell { junction: InstanceId, at: Anchor },
    #[error("routes {first:?} and {second:?} join through redstone dust at {at:?}")]
    CrossRouteConnectivity {
        first: RouteId,
        second: RouteId,
        at: Anchor,
    },
    #[error("route group {source_route:?} can energise foreign route {foreign:?} at {at:?}")]
    CrossRouteCoupling {
        source_route: RouteId,
        foreign: RouteId,
        at: Anchor,
    },
    #[error("route group {source_route:?} can energise foreign owner {foreign:?} at {at:?}")]
    CrossComponentCoupling {
        source_route: RouteId,
        foreign: StablePhysicalOwnerId,
        at: Anchor,
    },
    #[error(
        "junction {junction:?} contributor {contributor:?} on route {route:?} at {contributor_at:?} does not reach observation at {junction_at:?}"
    )]
    JunctionContributorDoesNotReach {
        junction: InstanceId,
        contributor: PhysicalEndpointId,
        route: Option<RouteId>,
        contributor_at: Anchor,
        junction_at: Anchor,
    },
    #[error(
        "junction {junction:?} has unlisted contributor {contributor:?} on route {route:?} at {contributor_at:?}; observation is at {junction_at:?}"
    )]
    UnlistedJunctionContributor {
        junction: InstanceId,
        contributor: PhysicalEndpointId,
        route: Option<RouteId>,
        contributor_at: Anchor,
        junction_at: Anchor,
    },
    #[error(
        "junction {junction:?} cannot resolve contributor {contributor:?} on route {route:?} at {at:?}"
    )]
    JunctionContributorLookupMissing {
        junction: InstanceId,
        contributor: PhysicalEndpointId,
        route: Option<RouteId>,
        at: Anchor,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StablePhysicalOwnerId {
    Primitive(PrimitiveId),
    Boundary(PhysicalEndpointId),
    Junction(InstanceId),
}

/// Verify the representation-independent physical facts available from the
/// expanded candidate and the exact typed ledger emitted from it.
///
/// Structural topology and pin closure have already been certified before
/// this function is called.  This stage independently reads the finished
/// world for observation equality, repeater conduction axes and dust-network
/// ownership.  Declared junction contributors and routes sharing one typed
/// source are unioned explicitly; every other cross-route dust join is a
/// physical short.
pub(crate) fn verify_expanded_candidate(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    let descriptor = expanded_strict_physical_verifier_revision_descriptor();
    debug_assert_eq!(descriptor.policy, PhysicalVerifierPolicy::ExpandedStrict);
    for registration in descriptor.rules {
        match registration.id {
            PhysicalVerifierRuleId::ObservationEquality => verify_observations(candidate, emitted)?,
            PhysicalVerifierRuleId::RouteContinuity => verify_route_continuity(candidate, emitted)?,
            PhysicalVerifierRuleId::Connectivity => {
                verify_junction_closure(candidate, emitted)?;
                verify_typed_connectivity(candidate, emitted)?;
            }
            PhysicalVerifierRuleId::Coupling => {
                verify_typed_coupling(candidate, emitted)?;
                verify_comparator_isolation(candidate, emitted)?;
            }
            PhysicalVerifierRuleId::Collision
            | PhysicalVerifierRuleId::TorchMergeStructure
            | PhysicalVerifierRuleId::SignalStrength
            | PhysicalVerifierRuleId::RepeaterDirection
            | PhysicalVerifierRuleId::TerminalStyle
            | PhysicalVerifierRuleId::PinHandoverHalo => {
                unreachable!("expanded strict descriptor contains a legacy-only rule")
            }
        }
    }
    Ok(())
}

/// Keep the old compiler's established acceptance while proving that its
/// lossless typed adapter and durable emitter agree.  Known legacy layouts
/// contain measured component/block coupling, so enabling the new strict
/// coupling gate here would silently switch shipping policy during an
/// extraction task.  New fragment candidates always use
/// [`verify_expanded_candidate`] and therefore do run that gate.
fn verify_expanded_legacy_compatibility(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    verify_observations(candidate, emitted)?;
    verify_repeater_handovers(candidate, emitted)?;
    verify_legacy_repeater_axes(candidate, emitted)?;
    verify_typed_connectivity(candidate, emitted)?;
    Ok(())
}

fn verify_observations(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    for (&observation, expected) in &candidate.observations {
        let at = expected.site.at;
        if emitted.world.get(at.x, at.y, at.z) != &expected.state {
            return Err(ExpandedPhysicalError::ObservationMismatch { observation, at });
        }
    }
    Ok(())
}

/// Supported diagonal steps must be declared at an actual logical primitive
/// endpoint. This never authorizes a solid path cell or an interior relay turn.
fn verify_repeater_handovers(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<BTreeSet<(RoutedSinkId, Anchor, Anchor)>, ExpandedPhysicalError> {
    use super::fragment_synth::candidate::RepeaterHandoverKind;
    let mut approved = BTreeSet::new();
    for (&sink, contracts) in &candidate.repeater_handovers {
        let invalid = || ExpandedPhysicalError::InvalidRepeaterHandover { sink };
        if contracts.is_empty() || contracts.len() > 2 {
            return Err(invalid());
        }
        let route = candidate.routes.get(&sink.route).ok_or_else(invalid)?;
        let branch = route
            .branches
            .iter()
            .find(|b| b.sink == sink)
            .ok_or_else(invalid)?;
        if branch.path.len() < 2 {
            return Err(invalid());
        }
        for contract in contracts {
            let placement = candidate
                .placements
                .get(&contract.primitive)
                .ok_or_else(invalid)?;
            let relay = placement
                .blocks
                .iter()
                .find(|b| b.state.kind == BlockKind::Repeater)
                .ok_or_else(invalid)?;
            let r = relay.at;
            let state = emitted.world.get(r.x, r.y, r.z);
            if state != &relay.state || state.kind != BlockKind::Repeater {
                return Err(invalid());
            }
            let facing = state.facing.ok_or_else(invalid)?;
            let rp = Position::new(r.x, r.y, r.z);
            let (pair, support) = match contract.kind {
                RepeaterHandoverKind::WeakFloorInput => {
                    let Some(binding) = candidate.connections.values().find(|b| b.sink == sink)
                    else {
                        return Err(invalid());
                    };
                    let spec = candidate
                        .instances
                        .instances
                        .iter()
                        .flat_map(|i| i.expanded.topology.connections.iter())
                        .find(|c| c.id == binding.id)
                        .ok_or_else(invalid)?;
                    if spec.target != ConnectionTarget::Primitive(contract.primitive) {
                        return Err(invalid());
                    }
                    let support = rp.offset(facing);
                    let dust = Anchor {
                        x: support.x,
                        y: support.y + 1,
                        z: support.z,
                    };
                    if branch.terminal.delayed_owner
                        != Some(super::routing::DelayedOwner::Primitive(contract.primitive))
                    {
                        return Err(invalid());
                    }
                    let pair = (dust, r);
                    if branch.path[branch.path.len() - 2..] != [pair.0, pair.1] {
                        return Err(invalid());
                    }
                    (pair, support)
                }
                RepeaterHandoverKind::StrongFloorOutput => {
                    if route.source != PhysicalEndpointId::PrimitiveOutput(contract.primitive) {
                        return Err(invalid());
                    }
                    let support = rp.offset(facing.opposite());
                    let dust = Anchor {
                        x: support.x,
                        y: support.y - 1,
                        z: support.z,
                    };
                    let pair = (r, dust);
                    if branch.path[..2] != [pair.0, pair.1] {
                        return Err(invalid());
                    }
                    (pair, support)
                }
            };
            if contract.support
                != (Anchor {
                    x: support.x,
                    y: support.y,
                    z: support.z,
                })
                || emitted.world.get(support.x, support.y, support.z).kind != BlockKind::Solid
                || emitted.world.get(pair.0.x, pair.0.y, pair.0.z).kind
                    != match contract.kind {
                        RepeaterHandoverKind::WeakFloorInput => BlockKind::RedstoneWire,
                        RepeaterHandoverKind::StrongFloorOutput => BlockKind::Repeater,
                    }
                || emitted.world.get(pair.1.x, pair.1.y, pair.1.z).kind
                    != match contract.kind {
                        RepeaterHandoverKind::WeakFloorInput => BlockKind::Repeater,
                        RepeaterHandoverKind::StrongFloorOutput => BlockKind::RedstoneWire,
                    }
                || !approved.insert((sink, pair.0, pair.1))
            {
                return Err(invalid());
            }
        }
    }
    Ok(approved)
}

fn comparator_interface_branch(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
    route: &super::routing::RealisedRouteTree,
    branch: &super::routing::RealisedRouteBranch,
) -> bool {
    use super::routing::RouteTerminalKind;
    let super::fragment_synth::candidate::RouteTarget::Connection(id) = branch.target else {
        return false;
    };
    if branch.path.as_slice() != [branch.terminal.at]
        || branch.root != branch.terminal.at
        || branch.terminal.delayed_owner.is_some()
    {
        return false;
    }
    for instance in &candidate.instances.instances {
        if let Some(req) = &instance.expanded.topology.comparator_subtract {
            if id != req.rear_input && id != req.side_input {
                continue;
            }
            let expected_kind = if id == req.rear_input {
                RouteTerminalKind::ComparatorRearSupport
            } else {
                RouteTerminalKind::ComparatorSideSupport
            };
            let Some(binding) = candidate.connections.get(&id) else {
                return false;
            };
            if route.source != binding.source || branch.terminal.kind != expected_kind {
                return false;
            }
            let Some((at, rear, side)) = subtract_cell_inputs(candidate, req) else {
                return false;
            };
            let support = if id == req.rear_input {
                rear.down()
            } else {
                side.support(&emitted.world).unwrap_or(side.at())
            };
            return branch.terminal.at
                == Anchor {
                    x: support.x,
                    y: support.y,
                    z: support.z,
                }
                && comparator_subtract_drivers_isolated(&emitted.world, at, rear, side).is_ok();
        }
        let Some(req) = &instance.expanded.topology.comparator_inverter else {
            continue;
        };
        let Some(spec) = instance
            .expanded
            .topology
            .connections
            .iter()
            .find(|c| c.id == id)
        else {
            continue;
        };
        if spec.target != ConnectionTarget::Primitive(req.comparator) {
            return false;
        }
        let Some(placement) = candidate.placements.get(&req.comparator) else {
            return false;
        };
        let Some(cell) = placement
            .blocks
            .iter()
            .find(|b| b.state.kind == BlockKind::Comparator)
        else {
            return false;
        };
        let at = Position::new(cell.at.x, cell.at.y, cell.at.z);
        let comparator = emitted.world.get(at.x, at.y, at.z);
        if comparator.kind != BlockKind::Comparator
            || comparator.extra_properties.get("mode").map(String::as_str) != Some("subtract")
        {
            return false;
        }
        if id == req.side_input && branch.terminal.kind == RouteTerminalKind::ComparatorSideSupport
        {
            let Some(binding) = candidate.connections.get(&id) else {
                return false;
            };
            if route.source != binding.source
                || !matches!(route.source, PhysicalEndpointId::PrimaryInput(_))
            {
                return false;
            }
            let Some(boundary) = candidate.boundaries.get(&route.source) else {
                return false;
            };
            let Some(lever) = boundary
                .blocks
                .iter()
                .find(|b| b.state.kind == BlockKind::Lever)
            else {
                return false;
            };
            let input = Position::new(lever.at.x, lever.at.y, lever.at.z);
            return branch.terminal.at
                == Anchor {
                    y: lever.at.y - 1,
                    ..lever.at
                }
                && comparator_cell_isolated(&emitted.world, at, input).is_ok();
        }
        if branch.terminal.kind == RouteTerminalKind::ComparatorRearSource
            && spec.source
                == super::fragment_synth::topology::ConnectionSource::Primitive(req.rear_constant)
            && route.source == PhysicalEndpointId::PrimitiveOutput(req.rear_constant)
        {
            let Some(facing) = comparator.facing else {
                return false;
            };
            let rear = at.offset(facing);
            return branch.terminal.at
                == Anchor {
                    x: rear.x,
                    y: rear.y,
                    z: rear.z,
                }
                && emitted.world.get(rear.x, rear.y, rear.z).kind == BlockKind::WallTorch;
        }
        return false;
    }
    false
}

fn verify_route_continuity(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    let supported = verify_repeater_handovers(candidate, emitted)?;
    for route in candidate.routes.values() {
        for branch in &route.branches {
            let comparator_interface =
                comparator_interface_branch(candidate, emitted, route, branch);
            for &at in &branch.path {
                let actual = emitted.world.get(at.x, at.y, at.z).kind;
                if !matches!(actual, BlockKind::RedstoneWire | BlockKind::Repeater)
                    && !(comparator_interface
                        && matches!(actual, BlockKind::Solid | BlockKind::WallTorch))
                {
                    return Err(ExpandedPhysicalError::NonConductingRouteCell {
                        route: route.id,
                        sink: branch.sink,
                        at,
                        actual,
                    });
                }
            }
            for triple in branch.path.windows(3) {
                let previous = triple[0];
                let at = triple[1];
                let next = triple[2];
                let state = emitted.world.get(at.x, at.y, at.z);
                if !route_step_is_legal(previous, at, next, state) {
                    return if state.kind == BlockKind::Repeater
                        && horizontal_direction(at, previous) != state.facing
                    {
                        Err(ExpandedPhysicalError::WrongRepeaterAxis {
                            sink: branch.sink,
                            at,
                        })
                    } else {
                        Err(ExpandedPhysicalError::DisconnectedRouteStep {
                            route: route.id,
                            sink: branch.sink,
                            from: at,
                            to: next,
                        })
                    };
                }
            }
            for (index, pair) in branch.path.windows(2).enumerate() {
                let from = pair[0];
                let to = pair[1];
                let from_state = emitted.world.get(from.x, from.y, from.z);
                let to_state = emitted.world.get(to.x, to.y, to.z);
                if index == 0
                    && from_state.kind == BlockKind::Repeater
                    && horizontal_direction(from, to) != from_state.facing.map(Facing::opposite)
                    && !supported.contains(&(branch.sink, from, to))
                {
                    return Err(ExpandedPhysicalError::DisconnectedRouteStep {
                        route: route.id,
                        sink: branch.sink,
                        from,
                        to,
                    });
                }
                if index + 2 == branch.path.len()
                    && to_state.kind == BlockKind::Repeater
                    && horizontal_direction(to, from) != to_state.facing
                    && !supported.contains(&(branch.sink, from, to))
                {
                    return Err(ExpandedPhysicalError::WrongRepeaterAxis {
                        sink: branch.sink,
                        at: to,
                    });
                }
                if from_state.kind == BlockKind::RedstoneWire
                    && to_state.kind == BlockKind::RedstoneWire
                    && !dust_step_connects(&emitted.world, from, to)
                {
                    return Err(ExpandedPhysicalError::DisconnectedRouteStep {
                        route: route.id,
                        sink: branch.sink,
                        from,
                        to,
                    });
                }
            }
        }
    }
    Ok(())
}

fn verify_legacy_repeater_axes(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    for route in candidate.routes.values() {
        for branch in &route.branches {
            for pair in branch.path.windows(2) {
                let previous = pair[0];
                let at = pair[1];
                let state = emitted.world.get(at.x, at.y, at.z);
                if state.kind == BlockKind::Repeater
                    && horizontal_direction(at, previous) != state.facing
                {
                    return Err(ExpandedPhysicalError::WrongRepeaterAxis {
                        sink: branch.sink,
                        at,
                    });
                }
            }
        }
    }
    Ok(())
}

fn dust_step_connects(
    world: &crate::redstone::world::storage::World,
    from: Anchor,
    to: Anchor,
) -> bool {
    let Some(direction) = horizontal_step_direction(from, to) else {
        return false;
    };
    dust_connections(world, Position::new(from.x, from.y, from.z), direction)
        .iter()
        .any(|position| position == Position::new(to.x, to.y, to.z))
}

fn horizontal_step_direction(from: Anchor, to: Anchor) -> Option<Facing> {
    match (to.x - from.x, to.z - from.z) {
        (-1, 0) => Some(Facing::West),
        (1, 0) => Some(Facing::East),
        (0, -1) => Some(Facing::North),
        (0, 1) => Some(Facing::South),
        _ => None,
    }
}

fn horizontal_direction(from: Anchor, to: Anchor) -> Option<Facing> {
    match (to.x - from.x, to.y - from.y, to.z - from.z) {
        (-1, 0, 0) => Some(Facing::West),
        (1, 0, 0) => Some(Facing::East),
        (0, 0, -1) => Some(Facing::North),
        (0, 0, 1) => Some(Facing::South),
        _ => None,
    }
}

fn verify_typed_connectivity(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    let parent = source_route_groups(candidate);
    let junction_cells = candidate
        .junctions
        .values()
        .flat_map(|junction| junction.cells.iter())
        .filter(|block| block.state.kind == BlockKind::RedstoneWire)
        .map(|block| Position::new(block.at.x, block.at.y, block.at.z))
        .collect::<HashSet<_>>();

    let world = &emitted.world;
    let mut declared_landings = BTreeMap::<Position, InstanceId>::new();
    for (&id, junction) in &candidate.junctions {
        for &contributor in &junction.contributors {
            if !matches!(contributor, PhysicalEndpointId::Landing(_)) {
                continue;
            }
            let (_, at) = resolve_junction_contributor(candidate, id, junction.at, contributor)?;
            declared_landings.insert(Position::new(at.x, at.y, at.z), id);
        }
    }
    let mut visited = HashSet::<Position>::new();
    for flat in world.positions_of(BlockKind::RedstoneWire) {
        let (x, y, z) = world.decode(flat);
        let start = Position::new(x, y, z);
        if junction_cells.contains(&start) || !visited.insert(start) {
            continue;
        }
        let mut queue = VecDeque::from([start]);
        let mut groups = BTreeMap::<RouteId, (RouteId, Anchor)>::new();
        while let Some(position) = queue.pop_front() {
            let at = Anchor {
                x: position.x,
                y: position.y,
                z: position.z,
            };
            if let Some(route) = route_owner(emitted.owner_at(at)) {
                let root = route_root(&parent, route);
                groups.entry(root).or_insert((route, at));
            }
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                for next in dust_connections(world, position, direction).iter() {
                    // Two explicitly listed injection terminals can touch
                    // inside the same OR region. Cut only that declared edge;
                    // retain all edges to unlisted or upstream route cells.
                    if declared_landings.get(&position).is_some()
                        && declared_landings.get(&position) == declared_landings.get(&next)
                    {
                        continue;
                    }
                    if !junction_cells.contains(&next) && visited.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        if groups.len() > 1 {
            let mut owners = groups.values();
            let &(first, first_at) = owners.next().expect("more than one group has a first");
            let &(second, second_at) = owners.next().expect("more than one group has a second");
            return Err(ExpandedPhysicalError::CrossRouteConnectivity {
                first,
                second,
                at: if first_at < second_at {
                    second_at
                } else {
                    first_at
                },
            });
        }
    }
    Ok(())
}

fn verify_junction_closure(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    for (&junction_id, junction) in &candidate.junctions {
        let mut listed = BTreeMap::new();
        for &contributor in &junction.contributors {
            let (route, contributor_at) =
                resolve_junction_contributor(candidate, junction_id, junction.at, contributor)?;
            listed.insert(contributor, (route, contributor_at));
            let seed = Position::new(contributor_at.x, contributor_at.y, contributor_at.z);
            let junction_position = Position::new(junction.at.x, junction.at.y, junction.at.z);
            if !directed_conductor_reach(&emitted.world, seed).contains(&junction_position) {
                return Err(ExpandedPhysicalError::JunctionContributorDoesNotReach {
                    junction: junction_id,
                    contributor,
                    route,
                    contributor_at,
                    junction_at: junction.at,
                });
            }
        }

        // Every declared junction wire must belong to its connected region.
        // Listed landings can bridge the region; arbitrary route conductors
        // cannot be relabelled as a junction to hide a foreign short.
        let region = junction
            .cells
            .iter()
            .filter(|block| block.state.kind == BlockKind::RedstoneWire)
            .map(|block| Position::new(block.at.x, block.at.y, block.at.z))
            .chain(
                listed
                    .values()
                    .map(|(_, at)| Position::new(at.x, at.y, at.z)),
            )
            .collect::<HashSet<_>>();
        let start = Position::new(junction.at.x, junction.at.y, junction.at.z);
        let mut reached = HashSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some(position) = queue.pop_front() {
            for next in potential_predecessors(position) {
                if region.contains(&next)
                    && !reached.contains(&next)
                    && directed_conductor_step(&emitted.world, position, next)
                {
                    reached.insert(next);
                    queue.push_back(next);
                }
            }
        }
        for block in junction
            .cells
            .iter()
            .filter(|block| block.state.kind == BlockKind::RedstoneWire)
        {
            if !reached.contains(&Position::new(block.at.x, block.at.y, block.at.z)) {
                return Err(ExpandedPhysicalError::DisconnectedJunctionCell {
                    junction: junction_id,
                    at: block.at,
                });
            }
        }

        let mut primitive_outputs = BTreeMap::<Anchor, Vec<PrimitiveId>>::new();
        for (&observation_id, observation) in &candidate.observations {
            let ObservationId::PrimitiveOutput(primitive) = observation_id else {
                continue;
            };
            primitive_outputs
                .entry(observation.site.at)
                .or_default()
                .push(primitive);
        }
        for &primitive in candidate.placements.keys() {
            let contributor = PhysicalEndpointId::PrimitiveOutput(primitive);
            if !candidate
                .observations
                .contains_key(&ObservationId::PrimitiveOutput(primitive))
            {
                return Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
                    junction: junction_id,
                    contributor,
                    route: None,
                    at: junction.at,
                });
            }
        }

        let observed = reverse_junction_contributors(
            candidate,
            emitted,
            junction_id,
            junction.at,
            &listed,
            &primitive_outputs,
        )?;
        for (&contributor, &(route, contributor_at)) in &listed {
            if !observed.contains(&contributor) {
                return Err(ExpandedPhysicalError::JunctionContributorDoesNotReach {
                    junction: junction_id,
                    contributor,
                    route,
                    contributor_at,
                    junction_at: junction.at,
                });
            }
        }
    }
    Ok(())
}

fn resolve_junction_contributor(
    candidate: &ExpandedPhysicalCandidate,
    junction: InstanceId,
    junction_at: Anchor,
    contributor: PhysicalEndpointId,
) -> Result<(Option<RouteId>, Anchor), ExpandedPhysicalError> {
    match contributor {
        PhysicalEndpointId::Landing(connection) => {
            let Some(binding) = candidate.connections.get(&connection) else {
                return Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
                    junction,
                    contributor,
                    route: None,
                    at: junction_at,
                });
            };
            let Some(route) = candidate.routes.get(&binding.route) else {
                return Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
                    junction,
                    contributor,
                    route: Some(binding.route),
                    at: junction_at,
                });
            };
            let Some(branch) = route.branches.iter().find(|branch| {
                branch.sink == binding.sink
                    && branch.target
                        == super::fragment_synth::candidate::RouteTarget::Connection(connection)
            }) else {
                return Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
                    junction,
                    contributor,
                    route: Some(binding.route),
                    at: junction_at,
                });
            };
            Ok((Some(binding.route), branch.terminal.at))
        }
        PhysicalEndpointId::PrimitiveOutput(primitive) => {
            let Some(observation) = candidate
                .observations
                .get(&ObservationId::PrimitiveOutput(primitive))
            else {
                return Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
                    junction,
                    contributor,
                    route: None,
                    at: junction_at,
                });
            };
            Ok((None, observation.site.at))
        }
        _ => Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
            junction,
            contributor,
            route: None,
            at: junction_at,
        }),
    }
}

fn reverse_junction_contributors(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
    junction: InstanceId,
    junction_at: Anchor,
    listed: &BTreeMap<PhysicalEndpointId, (Option<RouteId>, Anchor)>,
    primitive_outputs: &BTreeMap<Anchor, Vec<PrimitiveId>>,
) -> Result<BTreeSet<PhysicalEndpointId>, ExpandedPhysicalError> {
    let start = Position::new(junction_at.x, junction_at.y, junction_at.z);
    if !is_signal_path_kind(emitted.world.get(start.x, start.y, start.z).kind) {
        return Ok(BTreeSet::new());
    }
    let declared_carrier: HashSet<_> = candidate.junctions[&junction]
        .cells
        .iter()
        .filter(|b| b.state.kind == BlockKind::RedstoneWire)
        .map(|b| Position::new(b.at.x, b.at.y, b.at.z))
        .collect();
    let mut observed = BTreeSet::new();
    let mut visited = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(position) = queue.pop_front() {
        let at = Anchor {
            x: position.x,
            y: position.y,
            z: position.z,
        };
        if position != start {
            if let Some(primitives) = primitive_outputs.get(&at) {
                for &primitive in primitives {
                    let contributor = PhysicalEndpointId::PrimitiveOutput(primitive);
                    if !listed.contains_key(&contributor) {
                        return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                            junction,
                            contributor,
                            route: None,
                            contributor_at: at,
                            junction_at,
                        });
                    }
                    observed.insert(contributor);
                }
                continue;
            }

            match emitted.owner_at(at) {
                Some(PhysicalBlockRole::RouteTerminal { sink, target, .. }) => {
                    let source = route_source(candidate, junction, target, sink.route, at)?;
                    if source == PhysicalEndpointId::Junction(junction) {
                        // A tap inside the carrier is still part of the OR region.
                        // Inspect every physical predecessor, including foreign
                        // injections; stop only on branches outside the carrier.
                        if !declared_carrier.contains(&position) {
                            continue;
                        }
                    } else if matches!(target, PhysicalEndpointId::Landing(_)) {
                        if !listed.contains_key(&target) {
                            return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                                junction,
                                contributor: target,
                                route: Some(sink.route),
                                contributor_at: at,
                                junction_at,
                            });
                        }
                        observed.insert(target);
                        // A declared landing can inject into the interior of
                        // a distributed junction, rather than bounding it.
                        // Continue only through explicitly junction-owned
                        // cells or another listed landing; never follow the
                        // incoming source route past either terminal.
                        for predecessor in potential_predecessors(position) {
                            let predecessor_at = Anchor {
                                x: predecessor.x,
                                y: predecessor.y,
                                z: predecessor.z,
                            };
                            let declared_region = declared_carrier.contains(&predecessor)
                                || matches!(
                                    emitted.owner_at(predecessor_at),
                                    Some(PhysicalBlockRole::Junction(owner)) if owner == junction
                                )
                                || matches!(
                                    emitted.owner_at(predecessor_at),
                                    Some(PhysicalBlockRole::RouteTerminal { target, .. })
                                        if listed.contains_key(&target)
                                );
                            if declared_region
                                && !visited.contains(&predecessor)
                                && directed_conductor_step(&emitted.world, predecessor, position)
                            {
                                visited.insert(predecessor);
                                queue.push_back(predecessor);
                            }
                        }
                        continue;
                    } else {
                        return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                            junction,
                            contributor: source,
                            route: Some(sink.route),
                            contributor_at: at,
                            junction_at,
                        });
                    }
                }
                Some(PhysicalBlockRole::RouteConductor(route)) => {
                    let source = route_source(
                        candidate,
                        junction,
                        PhysicalEndpointId::Junction(junction),
                        route,
                        at,
                    )?;
                    if source == PhysicalEndpointId::Junction(junction) {
                        continue;
                    }
                    return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                        junction,
                        contributor: source,
                        route: Some(route),
                        contributor_at: at,
                        junction_at,
                    });
                }
                Some(PhysicalBlockRole::Boundary(contributor))
                | Some(PhysicalBlockRole::DeclaredOutputLamp(contributor)) => {
                    return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                        junction,
                        contributor,
                        route: None,
                        contributor_at: at,
                        junction_at,
                    });
                }
                Some(PhysicalBlockRole::Junction(other)) if other != junction => {
                    return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                        junction,
                        contributor: PhysicalEndpointId::Junction(other),
                        route: None,
                        contributor_at: at,
                        junction_at,
                    });
                }
                _ => {}
            }
        }

        for predecessor in potential_predecessors(position) {
            if visited.contains(&predecessor)
                || !directed_conductor_step(&emitted.world, predecessor, position)
            {
                continue;
            }
            visited.insert(predecessor);
            queue.push_back(predecessor);
        }
    }
    Ok(observed)
}

fn route_source(
    candidate: &ExpandedPhysicalCandidate,
    junction: InstanceId,
    contributor: PhysicalEndpointId,
    route: RouteId,
    at: Anchor,
) -> Result<PhysicalEndpointId, ExpandedPhysicalError> {
    candidate
        .routes
        .get(&route)
        .map(|route| route.source)
        .ok_or(ExpandedPhysicalError::JunctionContributorLookupMissing {
            junction,
            contributor,
            route: Some(route),
            at,
        })
}

fn directed_conductor_reach(world: &World, seed: Position) -> HashSet<Position> {
    if !is_signal_path_kind(world.get(seed.x, seed.y, seed.z).kind) {
        return HashSet::new();
    }
    let mut reached = HashSet::from([seed]);
    let mut queue = VecDeque::from([seed]);
    while let Some(position) = queue.pop_front() {
        for next in potential_predecessors(position) {
            if reached.contains(&next) || !directed_conductor_step(world, position, next) {
                continue;
            }
            reached.insert(next);
            queue.push_back(next);
        }
    }
    reached
}

fn potential_predecessors(position: Position) -> Vec<Position> {
    let mut positions = Vec::with_capacity(14);
    for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let horizontal = position.offset(direction);
        positions.push(horizontal.down());
        positions.push(horizontal);
        positions.push(horizontal.up());
    }
    positions.push(position.down());
    positions.push(position.up());
    positions
}

fn directed_conductor_step(world: &World, from: Position, to: Position) -> bool {
    let from_state = world.get(from.x, from.y, from.z);
    let to_state = world.get(to.x, to.y, to.z);
    if !is_signal_path_kind(from_state.kind) || !is_signal_path_kind(to_state.kind) {
        return false;
    }
    if from_state.kind == BlockKind::RedstoneWire && to_state.kind == BlockKind::RedstoneWire {
        let from_at = Anchor {
            x: from.x,
            y: from.y,
            z: from.z,
        };
        let to_at = Anchor {
            x: to.x,
            y: to.y,
            z: to.z,
        };
        return dust_step_connects(world, from_at, to_at);
    }
    let Some(direction) = adjacent_direction(from, to) else {
        return false;
    };
    let (drives_dust, block_power) =
        super::structural_output_in_world(world, from, from_state, direction);
    match to_state.kind {
        BlockKind::RedstoneWire => drives_dust,
        BlockKind::Repeater | BlockKind::Comparator => {
            to_state.facing == Some(direction.opposite())
                && (drives_dust || block_power != BlockPower::None)
        }
        _ => false,
    }
}

fn adjacent_direction(from: Position, to: Position) -> Option<Facing> {
    match (to.x - from.x, to.y - from.y, to.z - from.z) {
        (-1, 0, 0) => Some(Facing::West),
        (1, 0, 0) => Some(Facing::East),
        (0, -1, 0) => Some(Facing::Down),
        (0, 1, 0) => Some(Facing::Up),
        (0, 0, -1) => Some(Facing::North),
        (0, 0, 1) => Some(Facing::South),
        _ => None,
    }
}

fn is_signal_path_kind(kind: BlockKind) -> bool {
    matches!(
        kind,
        BlockKind::RedstoneWire
            | BlockKind::Repeater
            | BlockKind::Comparator
            | BlockKind::Torch
            | BlockKind::WallTorch
            | BlockKind::Lever
            | BlockKind::RedstoneBlock
            | BlockKind::Button
            | BlockKind::PressurePlate
            | BlockKind::WeightedPressurePlate
            | BlockKind::DaylightDetector
            | BlockKind::Observer
            | BlockKind::Target
    )
}

fn verify_comparator_isolation(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    for instance in &candidate.instances.instances {
        if let Some(req) = &instance.expanded.topology.comparator_subtract {
            let fail = |at: Position| ExpandedPhysicalError::ComparatorIsolation {
                primitive: req.comparator,
                at: Anchor {
                    x: at.x,
                    y: at.y,
                    z: at.z,
                },
            };
            let (at, rear, side) =
                subtract_cell_inputs(candidate, req).ok_or_else(|| fail(Position::new(0, 0, 0)))?;
            comparator_subtract_drivers_isolated(&emitted.world, at, rear, side).map_err(fail)?;
        }
        let Some(req) = &instance.expanded.topology.comparator_inverter else {
            continue;
        };
        let fail = |at: Position| ExpandedPhysicalError::ComparatorIsolation {
            primitive: req.comparator,
            at: Anchor {
                x: at.x,
                y: at.y,
                z: at.z,
            },
        };
        let placement = &candidate.placements[&req.comparator];
        let cell = placement
            .blocks
            .iter()
            .find(|b| b.state.kind == BlockKind::Comparator)
            .unwrap();
        let at = Position::new(cell.at.x, cell.at.y, cell.at.z);
        let source = candidate.connections[&req.side_input].source;
        let lever = candidate.boundaries[&source]
            .blocks
            .iter()
            .find(|b| b.state.kind == BlockKind::Lever)
            .unwrap();
        let input = Position::new(lever.at.x, lever.at.y, lever.at.z);
        comparator_cell_isolated(&emitted.world, at, input).map_err(fail)?;
    }
    Ok(())
}

fn subtract_cell_inputs(
    candidate: &ExpandedPhysicalCandidate,
    req: &super::fragment_synth::topology::ComparatorSubtractRequirements,
) -> Option<(Position, Position, ComparatorSupportDriver)> {
    let cell = candidate
        .placements
        .get(&req.comparator)?
        .blocks
        .iter()
        .find(|b| b.state.kind == BlockKind::Comparator)?;
    let input = |id| {
        let source = candidate.connections.get(&id)?.source;
        let lever = candidate
            .boundaries
            .get(&source)?
            .blocks
            .iter()
            .find(|b| b.state.kind == BlockKind::Lever)?;
        Some(Position::new(lever.at.x, lever.at.y, lever.at.z))
    };
    let side_source = candidate.connections.get(&req.side_input)?.source;
    let side = match side_source {
        PhysicalEndpointId::PrimaryInput(_) => {
            ComparatorSupportDriver::FloorLever(input(req.side_input)?)
        }
        PhysicalEndpointId::PrimitiveOutput(id) if req.allow_inverter_side => {
            let driver = candidate
                .placements
                .get(&id)?
                .blocks
                .iter()
                .find(|b| b.state.kind == BlockKind::Comparator)?;
            ComparatorSupportDriver::Comparator(Position::new(
                driver.at.x,
                driver.at.y,
                driver.at.z,
            ))
        }
        _ => return None,
    };
    Some((
        Position::new(cell.at.x, cell.at.y, cell.at.z),
        input(req.rear_input)?,
        side,
    ))
}

#[derive(Clone, Copy)]
enum ComparatorSupportDriver {
    FloorLever(Position),
    Comparator(Position),
}
impl ComparatorSupportDriver {
    fn at(self) -> Position {
        match self {
            Self::FloorLever(at) | Self::Comparator(at) => at,
        }
    }
    fn support(self, world: &World) -> Result<Position, Position> {
        let at = self.at();
        let block = world.get(at.x, at.y, at.z);
        match self {
            Self::FloorLever(_)
                if block.kind == BlockKind::Lever
                    && block.face == Some(crate::redstone::world::block::Face::Floor) =>
            {
                Ok(at.down())
            }
            Self::Comparator(_)
                if block.kind == BlockKind::Comparator
                    && block.extra_properties.get("mode").map(String::as_str)
                        == Some("subtract") =>
            {
                Ok(at.offset(block.facing.ok_or(at)?.opposite()))
            }
            _ => Err(at),
        }
    }
}

#[cfg(test)]
fn comparator_subtract_cell_isolated(
    world: &World,
    at: Position,
    rear_input: Position,
    side_input: Position,
) -> Result<(), Position> {
    comparator_subtract_drivers_isolated(
        world,
        at,
        rear_input,
        ComparatorSupportDriver::FloorLever(side_input),
    )
}

fn comparator_subtract_drivers_isolated(
    world: &World,
    at: Position,
    rear_input: Position,
    side_driver: ComparatorSupportDriver,
) -> Result<(), Position> {
    let comparator = world.get(at.x, at.y, at.z);
    if comparator.kind != BlockKind::Comparator
        || comparator.extra_properties.get("mode").map(String::as_str) != Some("subtract")
    {
        return Err(at);
    }
    let facing = comparator.facing.ok_or(at)?;
    let sides = match facing {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        Facing::East | Facing::West => [Facing::North, Facing::South],
        _ => return Err(at),
    };
    let rear = rear_input.down();
    let side = side_driver.support(world)?;
    if rear != at.offset(facing) {
        return Err(rear_input);
    }
    if !sides.into_iter().any(|f| side == at.offset(f)) || rear_input == side_driver.at() {
        return Err(side_driver.at());
    }
    for (driver, support, strong_only) in [
        (ComparatorSupportDriver::FloorLever(rear_input), rear, false),
        (side_driver, side, true),
    ] {
        if driver.support(world)? != support
            || world.get(support.x, support.y, support.z).kind != BlockKind::Solid
        {
            return Err(driver.at());
        }
        reject_potential_block_drivers(world, support, Some(driver.at()), strong_only)?;
    }
    let unused = sides
        .into_iter()
        .map(|f| at.offset(f))
        .find(|p| *p != side)
        .unwrap();
    match world.get(unused.x, unused.y, unused.z).kind {
        BlockKind::Air => (),
        BlockKind::Solid => reject_potential_block_drivers(world, unused, None, true)?,
        _ => return Err(unused),
    }
    Ok(())
}

/// Prove zero-state isolation using potential electrical outputs, independent
/// of the current lever, torch and diode states.
fn comparator_cell_isolated(world: &World, at: Position, input: Position) -> Result<(), Position> {
    use crate::redstone::world::block::Face;
    let comparator = world.get(at.x, at.y, at.z);
    let facing = comparator.facing.ok_or(at)?;
    let rear = at.offset(facing);
    let torch = world.get(rear.x, rear.y, rear.z);
    if torch.kind != BlockKind::WallTorch {
        return Err(rear);
    }
    let constant_support = rear.offset(torch.facing.ok_or(rear)?.opposite());
    let sides = match facing {
        Facing::North | Facing::South => [Facing::East, Facing::West],
        Facing::East | Facing::West => [Facing::North, Facing::South],
        _ => return Err(at),
    };
    let support = input.down();
    let lever = world.get(input.x, input.y, input.z);
    if lever.kind != BlockKind::Lever
        || lever.face != Some(Face::Floor)
        || world.get(support.x, support.y, support.z).kind != BlockKind::Solid
        || !sides.into_iter().any(|f| at.offset(f) == support)
    {
        return Err(input);
    }
    for (target, allowed, strong_only) in [
        (constant_support, None, false),
        (support, Some(input), true),
    ] {
        if world.get(target.x, target.y, target.z).kind != BlockKind::Solid {
            return Err(target);
        }
        reject_potential_block_drivers(world, target, allowed, strong_only)?;
    }
    let unused = sides
        .into_iter()
        .map(|f| at.offset(f))
        .find(|p| *p != support)
        .unwrap();
    match world.get(unused.x, unused.y, unused.z).kind {
        BlockKind::Air => (),
        BlockKind::Solid => reject_potential_block_drivers(world, unused, None, true)?,
        _ => return Err(unused),
    }
    Ok(())
}

fn reject_potential_block_drivers(
    world: &World,
    target: Position,
    allowed: Option<Position>,
    strong_only: bool,
) -> Result<(), Position> {
    for direction in [
        Facing::North,
        Facing::South,
        Facing::East,
        Facing::West,
        Facing::Up,
        Facing::Down,
    ] {
        let source = target.offset(direction);
        if Some(source) == allowed {
            continue;
        }
        let (_, power) = super::structural_output_in_world(
            world,
            source,
            world.get(source.x, source.y, source.z),
            direction.opposite(),
        );
        if power != BlockPower::None && (!strong_only || power == BlockPower::Strong) {
            return Err(source);
        }
    }
    Ok(())
}

fn verify_typed_coupling(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    let parent = typed_route_groups(candidate);
    let mut groups = BTreeMap::<RouteId, Vec<RouteId>>::new();
    for &route in candidate.routes.keys() {
        groups
            .entry(route_root(&parent, route))
            .or_default()
            .push(route);
    }
    for (&source_group, routes) in &groups {
        let mut seeds = BTreeSet::<Position>::new();
        for &route_id in routes {
            let route = &candidate.routes[&route_id];
            for block in &route.cells {
                seeds.insert(Position::new(block.at.x, block.at.y, block.at.z));
            }
            for branch in &route.branches {
                // A target primitive's repeater output is a new logical
                // signal. Its incoming route reaches the input but must not
                // seed the primitive's output as part of that input network.
                if matches!(
                    branch.terminal.delayed_owner,
                    Some(super::routing::DelayedOwner::Primitive(_))
                ) {
                    continue;
                }
                let at = branch.terminal.at;
                seeds.insert(Position::new(at.x, at.y, at.z));
            }
            if let Some(at) = source_observation(candidate, route.source) {
                seeds.insert(Position::new(at.x, at.y, at.z));
            }
        }
        let seed_cells = seeds.into_iter().collect::<Vec<_>>();
        let (network, powered) = super::net_network_and_reach(&emitted.world, &seed_cells);
        let allowed_components = allowed_component_owners(candidate, routes);
        for (at, role) in emitted.owners() {
            let position = Position::new(at.x, at.y, at.z);
            if !network.contains(&position) && !powered.contains(&position) {
                continue;
            }
            // Emission retains the incoming terminal's route identity even
            // when the block is owned by a target primitive. Coupling must
            // use that proven primitive owner, including for its output net.
            if let PhysicalBlockRole::RouteTerminal { sink, .. } = role {
                let primitive_owner = candidate
                    .routes
                    .get(&sink.route)
                    .and_then(|route| route.branches.iter().find(|branch| branch.sink == sink))
                    .and_then(|branch| match branch.terminal.delayed_owner {
                        Some(super::routing::DelayedOwner::Primitive(id)) => Some(id),
                        _ => None,
                    });
                if let Some(id) = primitive_owner {
                    let foreign = StablePhysicalOwnerId::Primitive(id);
                    if allowed_components.contains(&foreign) {
                        continue;
                    }
                    return Err(ExpandedPhysicalError::CrossComponentCoupling {
                        source_route: routes[0],
                        foreign,
                        at,
                    });
                }
            }
            if let Some(foreign) = route_owner(Some(role)) {
                if route_root(&parent, foreign) == source_group {
                    continue;
                }
                return Err(ExpandedPhysicalError::CrossRouteCoupling {
                    source_route: routes[0],
                    foreign,
                    at,
                });
            }
            if let Some(foreign) = component_owner(role) {
                if allowed_components.contains(&foreign) {
                    continue;
                }
                return Err(ExpandedPhysicalError::CrossComponentCoupling {
                    source_route: routes[0],
                    foreign,
                    at,
                });
            }
        }
    }
    Ok(())
}

fn allowed_component_owners(
    candidate: &ExpandedPhysicalCandidate,
    routes: &[RouteId],
) -> BTreeSet<StablePhysicalOwnerId> {
    let mut allowed = BTreeSet::new();
    for &route_id in routes {
        let route = &candidate.routes[&route_id];
        if let Some(owner) = endpoint_owner(route.source) {
            allowed.insert(owner);
        }
        for branch in &route.branches {
            match branch.target {
                super::fragment_synth::candidate::RouteTarget::DeclaredOutput(port) => {
                    allowed.insert(StablePhysicalOwnerId::Boundary(
                        PhysicalEndpointId::DeclaredOutput(port),
                    ));
                }
                super::fragment_synth::candidate::RouteTarget::Connection(connection) => {
                    if let Some(target) = candidate
                        .instances
                        .instances
                        .iter()
                        .flat_map(|instance| &instance.expanded.topology.connections)
                        .find(|spec| spec.id == connection)
                        .map(|spec| spec.target)
                    {
                        allowed.insert(match target {
                            ConnectionTarget::Primitive(primitive) => {
                                StablePhysicalOwnerId::Primitive(primitive)
                            }
                            ConnectionTarget::Junction(instance) => {
                                StablePhysicalOwnerId::Junction(instance)
                            }
                        });
                    }
                }
            }
        }
    }
    allowed
}

fn endpoint_owner(endpoint: PhysicalEndpointId) -> Option<StablePhysicalOwnerId> {
    match endpoint {
        PhysicalEndpointId::PrimaryInput(_) | PhysicalEndpointId::DeclaredOutput(_) => {
            Some(StablePhysicalOwnerId::Boundary(endpoint))
        }
        PhysicalEndpointId::PrimitiveOutput(primitive) => {
            Some(StablePhysicalOwnerId::Primitive(primitive))
        }
        PhysicalEndpointId::Junction(instance) => Some(StablePhysicalOwnerId::Junction(instance)),
        PhysicalEndpointId::Landing(_) => None,
    }
}

fn component_owner(role: PhysicalBlockRole) -> Option<StablePhysicalOwnerId> {
    match role {
        PhysicalBlockRole::Primitive(primitive) => {
            Some(StablePhysicalOwnerId::Primitive(primitive))
        }
        PhysicalBlockRole::Boundary(endpoint) | PhysicalBlockRole::DeclaredOutputLamp(endpoint) => {
            Some(StablePhysicalOwnerId::Boundary(endpoint))
        }
        PhysicalBlockRole::Junction(instance) => Some(StablePhysicalOwnerId::Junction(instance)),
        PhysicalBlockRole::RouteConductor(_)
        | PhysicalBlockRole::RouteFloor(_)
        | PhysicalBlockRole::RouteTerminal { .. } => None,
    }
}

fn source_observation(
    candidate: &ExpandedPhysicalCandidate,
    endpoint: PhysicalEndpointId,
) -> Option<Anchor> {
    let observation = match endpoint {
        PhysicalEndpointId::PrimaryInput(port) => ObservationId::PrimaryInput(port),
        PhysicalEndpointId::PrimitiveOutput(primitive) => ObservationId::PrimitiveOutput(primitive),
        PhysicalEndpointId::Junction(instance) => ObservationId::JunctionOutput(instance),
        PhysicalEndpointId::DeclaredOutput(_) | PhysicalEndpointId::Landing(_) => return None,
    };
    candidate
        .observations
        .get(&observation)
        .map(|observation| observation.site.at)
}

fn typed_route_groups(candidate: &ExpandedPhysicalCandidate) -> BTreeMap<RouteId, RouteId> {
    let mut parent = source_route_groups(candidate);
    let mut routes_by_source = BTreeMap::<PhysicalEndpointId, Vec<RouteId>>::new();
    for route in candidate.routes.values() {
        routes_by_source
            .entry(route.source)
            .or_default()
            .push(route.id);
    }
    for junction in candidate.junctions.values() {
        let mut joined = routes_by_source
            .get(&PhysicalEndpointId::Junction(junction.id))
            .cloned()
            .unwrap_or_default();
        for contributor in &junction.contributors {
            match contributor {
                PhysicalEndpointId::Landing(connection) => {
                    if let Some(binding) = candidate.connections.get(connection) {
                        joined.push(binding.route);
                    }
                }
                contributor => {
                    if let Some(routes) = routes_by_source.get(contributor) {
                        joined.extend(routes);
                    }
                }
            }
        }
        union_all(&mut parent, &joined);
    }
    for instance in &candidate.instances.instances {
        let mut routes_by_primitive = BTreeMap::<PrimitiveId, Vec<RouteId>>::new();
        for connection in &instance.expanded.topology.connections {
            let ConnectionTarget::Primitive(primitive) = connection.target else {
                continue;
            };
            let Some(binding) = candidate.connections.get(&connection.id) else {
                continue;
            };
            routes_by_primitive
                .entry(primitive)
                .or_default()
                .push(binding.route);
        }
        for (primitive, routes) in routes_by_primitive {
            let is_torch_input_merge = instance
                .expanded
                .topology
                .primitives
                .iter()
                .find(|specification| specification.id == primitive)
                .is_some_and(|specification| specification.primitive == Primitive::Torch);
            if is_torch_input_merge && routes.len() > 1 {
                union_all(&mut parent, &routes);
            }
        }
    }
    parent
}

fn source_route_groups(candidate: &ExpandedPhysicalCandidate) -> BTreeMap<RouteId, RouteId> {
    let mut parent = candidate
        .routes
        .keys()
        .copied()
        .map(|route| (route, route))
        .collect::<BTreeMap<_, _>>();
    let mut routes_by_source = BTreeMap::<PhysicalEndpointId, Vec<RouteId>>::new();
    for route in candidate.routes.values() {
        routes_by_source
            .entry(route.source)
            .or_default()
            .push(route.id);
    }
    for routes in routes_by_source.values() {
        union_all(&mut parent, routes);
    }
    parent
}

fn route_owner(role: Option<PhysicalBlockRole>) -> Option<RouteId> {
    match role {
        Some(PhysicalBlockRole::RouteConductor(route)) => Some(route),
        Some(PhysicalBlockRole::RouteTerminal { sink, .. }) => Some(sink.route),
        _ => None,
    }
}

fn union_all(parent: &mut BTreeMap<RouteId, RouteId>, routes: &[RouteId]) {
    let Some((&first, rest)) = routes.split_first() else {
        return;
    };
    for &route in rest {
        union_routes(parent, first, route);
    }
}

fn route_root(parent: &BTreeMap<RouteId, RouteId>, mut route: RouteId) -> RouteId {
    while parent
        .get(&route)
        .copied()
        .is_some_and(|next| next != route)
    {
        route = parent[&route];
    }
    route
}

fn union_routes(parent: &mut BTreeMap<RouteId, RouteId>, left: RouteId, right: RouteId) {
    let left = route_root(parent, left);
    let right = route_root(parent, right);
    if left != right {
        parent.insert(left.max(right), left.min(right));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn composed_subtract_side_requires_a_direct_strong_comparator_output() {
        use crate::redstone::{
            simulator::position::Position,
            world::{
                block::{BlockKind, BlockState, Face, Facing},
                storage::World,
            },
        };
        let mut world = World::new(16, 4, 16);
        let mut solid = BlockState::air();
        solid.kind = BlockKind::Solid;
        for x in 0..16 {
            for z in 0..16 {
                world.set(x, 0, z, solid.clone());
            }
        }
        let at = Position::new(8, 1, 6);
        let driver = Position::new(8, 1, 4);
        let rear = Position::new(9, 2, 6);
        for (p, facing) in [(at, Facing::East), (driver, Facing::North)] {
            let mut b = BlockState::air();
            b.kind = BlockKind::Comparator;
            b.facing = Some(facing);
            b.extra_properties.insert("mode".into(), "subtract".into());
            world.set(p.x, p.y, p.z, b);
        }
        let mut lever = BlockState::air();
        lever.kind = BlockKind::Lever;
        lever.face = Some(Face::Floor);
        world.set(rear.x, rear.y, rear.z, lever);
        for p in [rear.down(), Position::new(8, 1, 5)] {
            world.set(p.x, p.y, p.z, solid.clone());
        }
        let check = |w: &World| {
            super::comparator_subtract_drivers_isolated(
                w,
                at,
                rear,
                super::ComparatorSupportDriver::Comparator(driver),
            )
        };
        assert_eq!(check(&world), Ok(()));
        for mutation in 0..4 {
            let mut bad = world.clone();
            match mutation {
                0 | 1 => {
                    let mut b = bad.get(driver.x, driver.y, driver.z).clone();
                    if mutation == 0 {
                        b.facing = Some(Facing::South);
                    } else {
                        b.extra_properties.insert("mode".into(), "compare".into());
                    }
                    bad.set(driver.x, driver.y, driver.z, b);
                }
                2 => {
                    let mut b = BlockState::air();
                    b.kind = BlockKind::Repeater;
                    b.facing = Some(Facing::West);
                    bad.set(7, 1, 5, b);
                }
                _ => bad.set(8, 1, 5, BlockState::air()),
            }
            assert!(check(&bad).is_err());
        }
    }

    #[test]
    fn subtract_isolation_distinguishes_rear_weak_power_and_side_strong_power() {
        use crate::redstone::{
            simulator::position::Position,
            world::{
                block::{BlockKind, BlockState, Face, Facing},
                storage::World,
            },
        };
        fn put(world: &mut World, at: Position, kind: BlockKind, facing: Option<Facing>) {
            let mut b = BlockState::air();
            b.kind = kind;
            b.facing = facing;
            if kind == BlockKind::Lever {
                b.face = Some(Face::Floor);
            }
            if kind == BlockKind::Comparator {
                b.extra_properties.insert("mode".into(), "subtract".into());
            }
            world.set(at.x, at.y, at.z, b);
        }
        let mut world = World::new(10, 4, 10);
        for x in 0..10 {
            for z in 0..10 {
                put(&mut world, Position::new(x, 0, z), BlockKind::Solid, None);
            }
        }
        let at = Position::new(4, 1, 4);
        let rear = Position::new(5, 2, 4);
        let side = Position::new(4, 2, 3);
        put(&mut world, at, BlockKind::Comparator, Some(Facing::East));
        for input in [rear, side] {
            put(&mut world, input, BlockKind::Lever, Some(Facing::North));
            put(&mut world, input.down(), BlockKind::Solid, None);
        }
        let check = |w: &World| super::comparator_subtract_cell_isolated(w, at, rear, side);
        assert_eq!(check(&world), Ok(()));
        for (source, kind, facing, support) in [
            (
                Position::new(6, 1, 4),
                BlockKind::Repeater,
                Some(Facing::East),
                None,
            ),
            (
                Position::new(6, 1, 4),
                BlockKind::WallTorch,
                Some(Facing::West),
                Some(Position::new(7, 1, 4)),
            ),
            (
                Position::new(3, 1, 3),
                BlockKind::Repeater,
                Some(Facing::West),
                None,
            ),
            (
                Position::new(4, 1, 6),
                BlockKind::Repeater,
                Some(Facing::South),
                Some(Position::new(4, 1, 5)),
            ),
        ] {
            let mut bad = world.clone();
            if let Some(p) = support {
                put(&mut bad, p, BlockKind::Solid, None);
            }
            put(&mut bad, source, kind, facing);
            assert_eq!(check(&bad), Err(source));
        }
        let mut away = world.clone();
        put(
            &mut away,
            Position::new(6, 1, 4),
            BlockKind::Repeater,
            Some(Facing::West),
        );
        assert_eq!(check(&away), Ok(()));
        for which in 0..4 {
            let mut bad = world.clone();
            match which {
                0 => {
                    let mut b = bad.get(at.x, at.y, at.z).clone();
                    b.extra_properties.insert("mode".into(), "compare".into());
                    bad.set(at.x, at.y, at.z, b);
                    None::<String>
                }
                1 => {
                    let mut b = bad.get(rear.x, rear.y, rear.z).clone();
                    b.face = Some(Face::Wall);
                    bad.set(rear.x, rear.y, rear.z, b);
                    None
                }
                2 => {
                    put(&mut bad, side.down(), BlockKind::Air, None);
                    None
                }
                _ => {
                    put(
                        &mut bad,
                        Position::new(4, 1, 5),
                        BlockKind::RedstoneWire,
                        None,
                    );
                    None
                }
            };
            assert!(check(&bad).is_err());
        }
    }

    #[test]
    fn comparator_isolation_rejects_potential_foreign_drivers_even_when_unpowered() {
        use crate::redstone::{
            simulator::position::Position,
            world::{
                block::{BlockKind, BlockState, Face, Facing},
                storage::World,
            },
        };
        fn put(world: &mut World, at: Position, kind: BlockKind, facing: Option<Facing>) {
            let mut b = BlockState::air();
            b.kind = kind;
            b.facing = facing;
            if kind == BlockKind::Lever {
                b.face = Some(Face::Floor);
            }
            world.set(at.x, at.y, at.z, b);
        }
        let mut world = World::new(8, 4, 8);
        for x in 0..8 {
            for z in 0..8 {
                put(&mut world, Position::new(x, 0, z), BlockKind::Solid, None);
            }
        }
        let cmp = Position::new(3, 1, 3);
        let input = Position::new(3, 2, 2);
        put(&mut world, cmp, BlockKind::Comparator, Some(Facing::East));
        put(&mut world, input, BlockKind::Lever, Some(Facing::North));
        put(&mut world, input.down(), BlockKind::Solid, None);
        put(
            &mut world,
            Position::new(4, 1, 3),
            BlockKind::WallTorch,
            Some(Facing::West),
        );
        put(&mut world, Position::new(5, 1, 3), BlockKind::Solid, None);
        assert_eq!(super::comparator_cell_isolated(&world, cmp, input), Ok(()));
        for (source, kind, facing, extra_support) in [
            (
                Position::new(5, 2, 3),
                BlockKind::Lever,
                Some(Facing::North),
                None,
            ),
            (
                Position::new(2, 1, 2),
                BlockKind::Repeater,
                Some(Facing::West),
                None,
            ),
            (
                Position::new(3, 1, 5),
                BlockKind::Repeater,
                Some(Facing::South),
                Some(Position::new(3, 1, 4)),
            ),
            (Position::new(5, 2, 3), BlockKind::RedstoneWire, None, None),
        ] {
            let mut bad = world.clone();
            if let Some(at) = extra_support {
                put(&mut bad, at, BlockKind::Solid, None);
            }
            put(&mut bad, source, kind, facing);
            assert_eq!(
                super::comparator_cell_isolated(&bad, cmp, input),
                Err(source)
            );
        }
        let mut away = world.clone();
        put(
            &mut away,
            Position::new(2, 1, 2),
            BlockKind::Repeater,
            Some(Facing::East),
        );
        assert_eq!(super::comparator_cell_isolated(&away, cmp, input), Ok(()));
    }

    use super::{
        candidate_verifier_pipeline, directed_conductor_reach, directed_conductor_step,
        expanded_strict_physical_verifier_revision_descriptor,
        physical_verifier_revision_descriptor, verify_expanded_candidate, verify_junction_closure,
        verify_route_continuity, verify_typed_connectivity, verify_typed_coupling,
        CandidateVerifierCheckId, ExpandedPhysicalError, PhysicalVerifierPolicy,
        PhysicalVerifierRuleId, StablePhysicalOwnerId,
    };
    use crate::compile::emission::{
        emit_candidate as emit_typed, PhysicalBlockRef, PhysicalBlockRole, PhysicalCandidateView,
    };
    use crate::compile::fragment_synth::candidate::{
        ConnectionBinding, ExpandedPhysicalCandidate, PlacedBlock, RealisedJunction,
        RealisedRouteBranch, RealisedRouteTree, RouteTarget, TerminalRecord,
    };
    use crate::compile::fragment_synth::identity::{
        ConnectionId, InstanceId, ObservationId, PhysicalEndpointId, PortId, PrimitiveId, RouteId,
        RoutedSinkId, TopologyNodeId,
    };
    use crate::compile::fragment_synth::instance_graph::InstanceGraph;
    use crate::compile::fragment_synth::legacy_adapter::LegacyCandidateAdapter;
    use crate::compile::fragment_synth::realise::ExpandedCandidateAdapter;
    use crate::compile::geometry::Anchor;
    use crate::compile::planner::{PortPlacements, RouteTerminalKind};
    use crate::compile::{compile_legacy, Gate, Netlist};
    use crate::redstone::simulator::position::Position;
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

    struct ShortedRoutes {
        blocks: Vec<(Anchor, BlockState, PhysicalBlockRole)>,
    }

    impl PhysicalCandidateView for ShortedRoutes {
        fn visit_blocks(&self, visitor: &mut dyn FnMut(PhysicalBlockRef<'_>)) {
            for (at, state, role) in &self.blocks {
                visitor(PhysicalBlockRef {
                    at: *at,
                    state,
                    role: *role,
                });
            }
        }
    }

    fn candidate_with_path(path: Vec<(Anchor, BlockState)>) -> ExpandedPhysicalCandidate {
        let route = RouteId(0);
        let sink = RoutedSinkId { route, ordinal: 0 };
        let root = path.first().unwrap().0;
        let (terminal_at, terminal_state) = path.last().unwrap().clone();
        let cells = path
            .iter()
            .cloned()
            .map(|(at, state)| PlacedBlock { at, state })
            .collect();
        let tree = RealisedRouteTree {
            id: route,
            source: PhysicalEndpointId::PrimaryInput(PortId(0)),
            cells,
            floors: Vec::new(),
            branches: vec![RealisedRouteBranch {
                sink,
                target: RouteTarget::DeclaredOutput(PortId(0)),
                root,
                path: path.iter().map(|(at, _)| *at).collect(),
                terminal: TerminalRecord {
                    sink,
                    at: terminal_at,
                    state: terminal_state,
                    kind: RouteTerminalKind::DirectedDustIntoSupport,
                    repeaters: path
                        .iter()
                        .filter(|(_, state)| state.kind == BlockKind::Repeater)
                        .count() as u64,
                    delayed_owner: None,
                },
            }],
        };
        ExpandedPhysicalCandidate {
            instances: InstanceGraph {
                instances: Vec::new(),
                assignments: Vec::new(),
                primary_inputs: vec![PortId(0)],
                declared_outputs: vec![PortId(0)],
            },
            placements: Default::default(),
            boundaries: Default::default(),
            connections: Default::default(),
            routes: [(route, tree)].into_iter().collect(),
            repeater_handovers: Default::default(),
            junctions: Default::default(),
            observations: Default::default(),
            pins: PortPlacements::default(),
            pin_contracts: Default::default(),
            pin_name_bindings: Default::default(),
        }
    }

    fn emitted_path(
        candidate: &ExpandedPhysicalCandidate,
    ) -> crate::compile::emission::EmittedWorld {
        let route = candidate.routes.values().next().unwrap();
        let sink = route.branches[0].sink;
        let last = route.branches[0].terminal.at;
        let view = ShortedRoutes {
            blocks: route
                .cells
                .iter()
                .map(|block| {
                    let role = if block.at == last {
                        PhysicalBlockRole::RouteTerminal {
                            sink,
                            target: PhysicalEndpointId::DeclaredOutput(PortId(0)),
                            kind: crate::compile::emission::TerminalKind::DirectedDustIntoSupport,
                            repeaters: route.branches[0].terminal.repeaters,
                        }
                    } else {
                        PhysicalBlockRole::RouteConductor(route.id)
                    };
                    (block.at, block.state.clone(), role)
                })
                .collect(),
        };
        emit_typed(&view, (16, 8, 16)).unwrap()
    }

    #[test]
    fn the_durable_registry_preserves_the_complete_physical_rule_order() {
        assert_eq!(
            candidate_verifier_pipeline()
                .iter()
                .map(|entry| entry.check)
                .collect::<Vec<_>>(),
            vec![
                CandidateVerifierCheckId::Collision,
                CandidateVerifierCheckId::TerminalContract,
                CandidateVerifierCheckId::RouteTerminals,
                CandidateVerifierCheckId::RealisedWorld,
            ]
        );
        assert_eq!(
            physical_verifier_revision_descriptor()
                .rules
                .iter()
                .map(|rule| (rule.id, rule.semantic_version))
                .collect::<Vec<_>>(),
            vec![
                (PhysicalVerifierRuleId::Collision, 1),
                (PhysicalVerifierRuleId::RepeaterDirection, 1),
                (PhysicalVerifierRuleId::PinHandoverHalo, 1),
                (PhysicalVerifierRuleId::TerminalStyle, 1),
                (PhysicalVerifierRuleId::Coupling, 1),
                (PhysicalVerifierRuleId::Connectivity, 1),
                (PhysicalVerifierRuleId::TorchMergeStructure, 1),
                (PhysicalVerifierRuleId::SignalStrength, 1),
            ]
        );
    }

    #[test]
    fn typed_connectivity_rejects_two_unrelated_route_owners_in_one_dust_component() {
        let mut candidate =
            candidate_with_path(vec![(Anchor { x: 2, y: 1, z: 2 }, crate::compile::dust())]);
        let first = RouteId(0);
        let second = RouteId(1);
        let mut second_tree = candidate.routes[&first].clone();
        second_tree.id = second;
        second_tree.source = PhysicalEndpointId::PrimaryInput(PortId(1));
        second_tree.branches[0].sink = RoutedSinkId {
            route: second,
            ordinal: 0,
        };
        second_tree.branches[0].terminal.sink = second_tree.branches[0].sink;
        candidate.routes.insert(second, second_tree);
        let view = ShortedRoutes {
            blocks: vec![
                (
                    Anchor { x: 2, y: 1, z: 2 },
                    crate::compile::dust(),
                    PhysicalBlockRole::RouteConductor(first),
                ),
                (
                    Anchor { x: 3, y: 1, z: 2 },
                    crate::compile::dust(),
                    PhysicalBlockRole::RouteConductor(second),
                ),
            ],
        };
        let emitted = emit_typed(&view, (8, 4, 8)).expect("fixture emits");

        assert!(matches!(
            verify_typed_connectivity(&candidate, &emitted),
            Err(ExpandedPhysicalError::CrossRouteConnectivity { first, second, .. })
                if [first, second].into_iter().collect::<std::collections::BTreeSet<_>>()
                    == [RouteId(0), RouteId(1)].into_iter().collect()
        ));
    }

    #[test]
    fn junction_contributors_may_join_only_at_the_junction_cell() {
        let first = RouteId(0);
        let second = RouteId(1);
        let first_connection = ConnectionId::External {
            instance: InstanceId(0),
            input_index: 0,
        };
        let second_connection = ConnectionId::External {
            instance: InstanceId(0),
            input_index: 1,
        };
        let first_sink = RoutedSinkId {
            route: first,
            ordinal: 0,
        };
        let second_sink = RoutedSinkId {
            route: second,
            ordinal: 0,
        };
        let mut candidate =
            candidate_with_path(vec![(Anchor { x: 2, y: 1, z: 2 }, crate::compile::dust())]);
        candidate.routes.get_mut(&first).unwrap().branches[0].target =
            RouteTarget::Connection(first_connection);
        let mut second_tree = candidate.routes[&first].clone();
        second_tree.id = second;
        second_tree.source = PhysicalEndpointId::PrimaryInput(PortId(1));
        second_tree.branches[0].sink = second_sink;
        second_tree.branches[0].terminal.sink = second_sink;
        second_tree.branches[0].target = RouteTarget::Connection(second_connection);
        candidate.routes.insert(second, second_tree);
        candidate.connections.insert(
            first_connection,
            ConnectionBinding {
                id: first_connection,
                source: PhysicalEndpointId::PrimaryInput(PortId(0)),
                landing: PhysicalEndpointId::Landing(first_connection),
                route: first,
                sink: first_sink,
            },
        );
        candidate.connections.insert(
            second_connection,
            ConnectionBinding {
                id: second_connection,
                source: PhysicalEndpointId::PrimaryInput(PortId(1)),
                landing: PhysicalEndpointId::Landing(second_connection),
                route: second,
                sink: second_sink,
            },
        );
        let junction = InstanceId(0);
        let junction_at = Anchor { x: 5, y: 1, z: 5 };
        candidate.junctions.insert(
            junction,
            RealisedJunction {
                id: junction,
                at: junction_at,
                facing: crate::compile::geometry::CellFacing::NORTH,
                contributors: vec![
                    PhysicalEndpointId::Landing(first_connection),
                    PhysicalEndpointId::Landing(second_connection),
                ],
                cells: vec![PlacedBlock {
                    at: junction_at,
                    state: crate::compile::dust(),
                }],
            },
        );

        let shortcut = emit_typed(
            &ShortedRoutes {
                blocks: vec![
                    (
                        Anchor { x: 2, y: 1, z: 2 },
                        crate::compile::dust(),
                        PhysicalBlockRole::RouteConductor(first),
                    ),
                    (
                        Anchor { x: 3, y: 1, z: 2 },
                        crate::compile::dust(),
                        PhysicalBlockRole::RouteConductor(second),
                    ),
                ],
            },
            (8, 4, 8),
        )
        .unwrap();
        assert!(matches!(
            verify_typed_connectivity(&candidate, &shortcut),
            Err(ExpandedPhysicalError::CrossRouteConnectivity { first: a, second: b, .. })
                if [a, b]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    == [first, second].into_iter().collect()
        ));

        let legitimate = emit_typed(
            &ShortedRoutes {
                blocks: vec![
                    (
                        Anchor { x: 4, y: 1, z: 5 },
                        crate::compile::dust(),
                        PhysicalBlockRole::RouteConductor(first),
                    ),
                    (
                        junction_at,
                        crate::compile::dust(),
                        PhysicalBlockRole::Junction(junction),
                    ),
                    (
                        Anchor { x: 6, y: 1, z: 5 },
                        crate::compile::dust(),
                        PhysicalBlockRole::RouteConductor(second),
                    ),
                ],
            },
            (8, 4, 8),
        )
        .unwrap();
        assert_eq!(verify_typed_connectivity(&candidate, &legitimate), Ok(()));

        // Adjacent, explicitly listed injection terminals may join before
        // the observed junction cell. An unlisted adjacent route may not.
        let first_at = Anchor { x: 3, y: 1, z: 5 };
        let second_at = Anchor { x: 4, y: 1, z: 5 };
        candidate.routes.get_mut(&first).unwrap().branches[0]
            .terminal
            .at = first_at;
        candidate.routes.get_mut(&second).unwrap().branches[0]
            .terminal
            .at = second_at;
        let mut adjacent_blocks = vec![
            (
                first_at,
                crate::compile::dust(),
                PhysicalBlockRole::RouteTerminal {
                    sink: first_sink,
                    target: PhysicalEndpointId::Landing(first_connection),
                    kind: super::super::emission::TerminalKind::BareMergeDust,
                    repeaters: 0,
                },
            ),
            (
                second_at,
                crate::compile::dust(),
                PhysicalBlockRole::RouteTerminal {
                    sink: second_sink,
                    target: PhysicalEndpointId::Landing(second_connection),
                    kind: super::super::emission::TerminalKind::BareMergeDust,
                    repeaters: 0,
                },
            ),
            (
                junction_at,
                crate::compile::dust(),
                PhysicalBlockRole::Junction(junction),
            ),
        ];
        let adjacent = emit_typed(
            &ShortedRoutes {
                blocks: adjacent_blocks.clone(),
            },
            (8, 4, 8),
        )
        .unwrap();
        assert_eq!(verify_typed_connectivity(&candidate, &adjacent), Ok(()));
        assert_eq!(verify_junction_closure(&candidate, &adjacent), Ok(()));
        candidate
            .junctions
            .get_mut(&junction)
            .unwrap()
            .contributors
            .pop();
        assert!(matches!(
            verify_typed_connectivity(&candidate, &adjacent),
            Err(ExpandedPhysicalError::CrossRouteConnectivity { .. })
        ));
        candidate
            .junctions
            .get_mut(&junction)
            .unwrap()
            .contributors
            .push(PhysicalEndpointId::Landing(second_connection));
        let foreign = RouteId(2);
        let mut foreign_tree = candidate.routes[&first].clone();
        foreign_tree.id = foreign;
        foreign_tree.source = PhysicalEndpointId::PrimaryInput(PortId(2));
        candidate.routes.insert(foreign, foreign_tree);
        adjacent_blocks.push((
            Anchor { x: 4, y: 1, z: 4 },
            crate::compile::dust(),
            PhysicalBlockRole::RouteConductor(foreign),
        ));
        let contaminated = emit_typed(
            &ShortedRoutes {
                blocks: adjacent_blocks,
            },
            (8, 4, 8),
        )
        .unwrap();
        assert!(matches!(
            verify_typed_connectivity(&candidate, &contaminated),
            Err(ExpandedPhysicalError::CrossRouteConnectivity { .. })
        ));
    }

    #[test]
    fn expanded_strict_rejects_a_stone_cell_inside_a_declared_branch() {
        let broken_at = Anchor { x: 3, y: 1, z: 2 };
        let candidate = candidate_with_path(vec![
            (Anchor { x: 2, y: 1, z: 2 }, crate::compile::dust()),
            (broken_at, crate::compile::stone()),
            (Anchor { x: 4, y: 1, z: 2 }, crate::compile::dust()),
        ]);
        let emitted = emitted_path(&candidate);
        let route_id = RouteId(0);
        let sink = RoutedSinkId {
            route: route_id,
            ordinal: 0,
        };

        assert!(matches!(
            verify_expanded_candidate(&candidate, &emitted),
            Err(ExpandedPhysicalError::NonConductingRouteCell {
                route,
                sink: rejected_sink,
                at,
                ..
            }) if route == route_id && rejected_sink == sink && at == broken_at
        ));
    }

    #[test]
    fn repeater_output_cannot_turn_before_the_next_path_cell() {
        let previous = Anchor { x: 2, y: 1, z: 2 };
        let repeater = Anchor { x: 3, y: 1, z: 2 };
        let turned = Anchor { x: 3, y: 1, z: 3 };
        let candidate = candidate_with_path(vec![
            (previous, crate::compile::dust()),
            (repeater, crate::compile::repeater(Facing::East)),
            (turned, crate::compile::dust()),
        ]);
        let emitted = emitted_path(&candidate);
        let route_id = RouteId(0);
        let sink = RoutedSinkId {
            route: route_id,
            ordinal: 0,
        };

        let actual = verify_route_continuity(&candidate, &emitted);
        assert!(
            matches!(
                actual,
                Err(ExpandedPhysicalError::DisconnectedRouteStep {
                    route,
                    sink: rejected_sink,
                    from,
                    to,
                }) if route == route_id && rejected_sink == sink && from == repeater && to == turned
            ),
            "{actual:?}"
        );
    }

    #[test]
    fn strict_coupling_rejects_power_reaching_a_foreign_primitive_owner() {
        let route_id = RouteId(0);
        let route_at = Anchor { x: 4, y: 2, z: 4 };
        let candidate = candidate_with_path(vec![(route_at, crate::compile::dust())]);
        let foreign = PrimitiveId {
            instance: InstanceId(7),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(3),
        };
        let foreign_at = Anchor {
            x: route_at.x,
            y: route_at.y - 1,
            z: route_at.z,
        };
        let view = ShortedRoutes {
            blocks: vec![
                (
                    route_at,
                    crate::compile::dust(),
                    PhysicalBlockRole::RouteConductor(route_id),
                ),
                (
                    foreign_at,
                    crate::compile::stone(),
                    PhysicalBlockRole::Primitive(foreign),
                ),
            ],
        };
        let emitted = emit_typed(&view, (16, 8, 16)).unwrap();

        assert!(matches!(
            verify_typed_coupling(&candidate, &emitted),
            Err(ExpandedPhysicalError::CrossComponentCoupling {
                source_route,
                foreign: StablePhysicalOwnerId::Primitive(rejected),
                at,
            }) if source_route == route_id && rejected == foreign && at == foreign_at
        ));
    }

    #[test]
    fn strict_coupling_preserves_the_declared_source_and_target_owners() {
        let route_at = Anchor { x: 4, y: 2, z: 4 };
        let target_at = Anchor { x: 4, y: 1, z: 4 };
        let candidate = candidate_with_path(vec![(route_at, crate::compile::dust())]);
        let view = ShortedRoutes {
            blocks: vec![
                (
                    route_at,
                    crate::compile::dust(),
                    PhysicalBlockRole::Boundary(PhysicalEndpointId::PrimaryInput(PortId(0))),
                ),
                (
                    target_at,
                    crate::compile::stone(),
                    PhysicalBlockRole::Boundary(PhysicalEndpointId::DeclaredOutput(PortId(0))),
                ),
            ],
        };
        let emitted = emit_typed(&view, (16, 8, 16)).unwrap();

        assert_eq!(verify_typed_coupling(&candidate, &emitted), Ok(()));
    }

    #[test]
    fn junction_closure_preserves_valid_legacy_bare_and_mixed_merges() {
        let fixtures = [
            Netlist {
                inputs: vec!["a".to_string(), "b".to_string()],
                outputs: vec!["y".to_string()],
                gates: vec![Gate::merge("y", &["a", "b"])],
            },
            Netlist {
                inputs: vec!["a".to_string(), "b".to_string()],
                outputs: vec!["y".to_string(), "xa".to_string()],
                gates: vec![Gate::merge("y", &["a", "b"]), Gate::nor("xa", &["a"])],
            },
        ];

        for netlist in fixtures {
            let compiled = compile_legacy(&netlist).expect("legacy merge fixture compiles");
            let adapted = LegacyCandidateAdapter::adapt(&netlist, &compiled)
                .expect("legacy merge fixture adapts");
            let adapter =
                ExpandedCandidateAdapter::new(&adapted.candidate).expect("candidate adapts");
            let emitted = emit_typed(&adapter, compiled.world.size()).expect("candidate emits");

            assert_eq!(
                verify_junction_closure(&adapted.candidate, &emitted),
                Ok(()),
                "valid merge was rejected: {netlist:?}"
            );
        }
    }

    #[test]
    fn junction_contributor_cannot_reach_only_a_powered_support() {
        let route = RouteId(0);
        let sink = RoutedSinkId { route, ordinal: 0 };
        let connection = ConnectionId::External {
            instance: InstanceId(0),
            input_index: 0,
        };
        let terminal_at = Anchor { x: 3, y: 2, z: 3 };
        let junction_at = Anchor { x: 3, y: 1, z: 3 };
        let terminal = crate::compile::dust();
        let support = crate::compile::stone();
        let mut candidate = candidate_with_path(vec![(terminal_at, terminal.clone())]);
        candidate.routes.get_mut(&route).unwrap().branches[0].target =
            RouteTarget::Connection(connection);
        candidate.routes.get_mut(&route).unwrap().branches[0]
            .terminal
            .kind = RouteTerminalKind::BareMergeDust;
        candidate.connections.insert(
            connection,
            ConnectionBinding {
                id: connection,
                source: PhysicalEndpointId::PrimaryInput(PortId(0)),
                landing: PhysicalEndpointId::Landing(connection),
                route,
                sink,
            },
        );
        let junction = InstanceId(0);
        candidate.junctions.insert(
            junction,
            RealisedJunction {
                id: junction,
                at: junction_at,
                facing: crate::compile::geometry::CellFacing::NORTH,
                contributors: vec![PhysicalEndpointId::Landing(connection)],
                cells: vec![PlacedBlock {
                    at: junction_at,
                    state: support.clone(),
                }],
            },
        );
        let emitted = emit_typed(
            &ShortedRoutes {
                blocks: vec![
                    (
                        terminal_at,
                        terminal,
                        PhysicalBlockRole::RouteTerminal {
                            sink,
                            target: PhysicalEndpointId::Landing(connection),
                            kind: crate::compile::emission::TerminalKind::BareMergeDust,
                            repeaters: 0,
                        },
                    ),
                    (junction_at, support, PhysicalBlockRole::Junction(junction)),
                ],
            },
            (8, 4, 8),
        )
        .expect("fixture emits");

        let actual = verify_junction_closure(&candidate, &emitted);
        assert!(
            matches!(
                actual,
                Err(ExpandedPhysicalError::JunctionContributorDoesNotReach {
                    junction: rejected_junction,
                    contributor: PhysicalEndpointId::Landing(rejected_connection),
                    route: Some(rejected_route),
                    contributor_at: rejected_at,
                    junction_at: rejected_junction_at,
                }) if rejected_junction == junction
                    && rejected_connection == connection
                    && rejected_route == route
                    && rejected_at == terminal_at
                    && rejected_junction_at == junction_at
            ),
            "powered support counted as conductive reach: {actual:?}"
        );
    }

    #[test]
    fn route_owned_powered_support_is_not_an_unlisted_contributor() {
        let route = RouteId(0);
        let junction = InstanceId(0);
        let junction_at = Anchor { x: 3, y: 2, z: 3 };
        let support_at = Anchor { x: 3, y: 1, z: 3 };
        let mut candidate =
            candidate_with_path(vec![(Anchor { x: 6, y: 1, z: 6 }, crate::compile::dust())]);
        candidate.junctions.insert(
            junction,
            RealisedJunction {
                id: junction,
                at: junction_at,
                facing: crate::compile::geometry::CellFacing::NORTH,
                contributors: Vec::new(),
                cells: vec![PlacedBlock {
                    at: junction_at,
                    state: crate::compile::dust(),
                }],
            },
        );
        let emitted = emit_typed(
            &ShortedRoutes {
                blocks: vec![
                    (
                        junction_at,
                        crate::compile::dust(),
                        PhysicalBlockRole::Junction(junction),
                    ),
                    (
                        support_at,
                        crate::compile::stone(),
                        PhysicalBlockRole::RouteConductor(route),
                    ),
                ],
            },
            (8, 4, 8),
        )
        .expect("fixture emits");

        assert_eq!(verify_junction_closure(&candidate, &emitted), Ok(()));
    }

    #[test]
    fn adjacent_outbound_landing_terminal_is_not_an_incoming_contributor() {
        let junction = InstanceId(0);
        let route = RouteId(0);
        let sink = RoutedSinkId { route, ordinal: 0 };
        let connection = ConnectionId::External {
            instance: InstanceId(1),
            input_index: 0,
        };
        let junction_at = Anchor { x: 3, y: 1, z: 3 };
        let terminal_at = Anchor { x: 4, y: 1, z: 3 };
        let terminal = crate::compile::dust();
        let mut candidate = candidate_with_path(vec![(terminal_at, terminal.clone())]);
        let outbound = candidate.routes.get_mut(&route).expect("route exists");
        outbound.source = PhysicalEndpointId::Junction(junction);
        outbound.branches[0].target = RouteTarget::Connection(connection);
        outbound.branches[0].terminal.kind = RouteTerminalKind::BareMergeDust;
        candidate.connections.insert(
            connection,
            ConnectionBinding {
                id: connection,
                source: PhysicalEndpointId::Junction(junction),
                landing: PhysicalEndpointId::Landing(connection),
                route,
                sink,
            },
        );
        candidate.junctions.insert(
            junction,
            RealisedJunction {
                id: junction,
                at: junction_at,
                facing: crate::compile::geometry::CellFacing::NORTH,
                contributors: Vec::new(),
                cells: vec![PlacedBlock {
                    at: junction_at,
                    state: crate::compile::dust(),
                }],
            },
        );
        let emitted = emit_typed(
            &ShortedRoutes {
                blocks: vec![
                    (
                        junction_at,
                        crate::compile::dust(),
                        PhysicalBlockRole::Junction(junction),
                    ),
                    (
                        terminal_at,
                        terminal,
                        PhysicalBlockRole::RouteTerminal {
                            sink,
                            target: PhysicalEndpointId::Landing(connection),
                            kind: crate::compile::emission::TerminalKind::BareMergeDust,
                            repeaters: 0,
                        },
                    ),
                ],
            },
            (8, 4, 8),
        )
        .expect("fixture emits");

        let actual = verify_junction_closure(&candidate, &emitted);
        assert_eq!(
            actual,
            Ok(()),
            "outbound landing terminal was classified as incoming: {actual:?}"
        );

        // Once this tap is declared inside the shared carrier, a foreign
        // input directly driving it must not disappear at the outbound cut.
        candidate
            .junctions
            .get_mut(&junction)
            .unwrap()
            .cells
            .push(PlacedBlock {
                at: terminal_at,
                state: crate::compile::dust(),
            });
        let foreign_at = Anchor { x: 4, y: 1, z: 4 };
        let foreign = PhysicalEndpointId::PrimaryInput(PortId(9));
        let mut blocks = emitted
            .owners()
            .map(|(at, role)| (at, emitted.world.get(at.x, at.y, at.z).clone(), role))
            .collect::<Vec<_>>();
        let lever = crate::compile::lever(true);
        blocks.push((foreign_at, lever, PhysicalBlockRole::Boundary(foreign)));
        let injected = emit_typed(&ShortedRoutes { blocks }, (8, 4, 8)).unwrap();
        assert!(matches!(
            verify_junction_closure(&candidate, &injected),
            Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                contributor, contributor_at, ..
            }) if contributor == foreign && contributor_at == foreign_at
        ));
    }

    #[test]
    fn missing_landing_branch_fails_closed_with_typed_context() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("fixture adapts");
        let adapter = ExpandedCandidateAdapter::new(&adapted.candidate).expect("candidate adapts");
        let emitted = emit_typed(&adapter, compiled.world.size()).expect("candidate emits");
        let (&junction, realised) = adapted
            .candidate
            .junctions
            .iter()
            .next()
            .expect("bare merge has a junction");
        let junction_at = realised.at;
        let connection = realised
            .contributors
            .iter()
            .find_map(|contributor| match contributor {
                PhysicalEndpointId::Landing(connection) => Some(*connection),
                _ => None,
            })
            .expect("bare merge has a landing");
        let binding = adapted.candidate.connections[&connection].clone();
        adapted
            .candidate
            .routes
            .get_mut(&binding.route)
            .expect("landing route exists")
            .branches
            .retain(|branch| branch.sink != binding.sink);

        let actual = verify_junction_closure(&adapted.candidate, &emitted);
        assert!(
            matches!(
                actual,
                Err(ExpandedPhysicalError::JunctionContributorLookupMissing {
                    junction: rejected_junction,
                    contributor: PhysicalEndpointId::Landing(rejected_connection),
                    route: Some(rejected_route),
                    at,
                }) if rejected_junction == junction
                    && rejected_connection == connection
                    && rejected_route == binding.route
                    && at == junction_at
            ),
            "missing landing branch failed open: {actual:?}"
        );
    }

    #[test]
    fn foreign_instance_primitive_direct_join_is_unlisted() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![
                Gate::merge("y", &["a", "b"]),
                Gate::nor("xa", &["a"]),
                Gate::nor("xb", &["b"]),
            ],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("fixture adapts");
        let adapter = ExpandedCandidateAdapter::new(&adapted.candidate).expect("candidate adapts");
        let emitted = emit_typed(&adapter, compiled.world.size()).expect("candidate emits");
        let (&junction, realised) = adapted
            .candidate
            .junctions
            .iter_mut()
            .find(|(_, junction)| {
                junction
                    .contributors
                    .iter()
                    .all(|endpoint| matches!(endpoint, PhysicalEndpointId::PrimitiveOutput(_)))
            })
            .expect("fixture has an all-isolated merge");
        let expected_junction_at = realised.at;
        let original = match realised.contributors.pop().expect("has contributor") {
            PhysicalEndpointId::PrimitiveOutput(primitive) => primitive,
            other => panic!("expected primitive contributor, got {other:?}"),
        };
        let foreign = PrimitiveId {
            instance: InstanceId(junction.0 + 100),
            node: original.node,
        };
        let at = adapted
            .candidate
            .observations
            .remove(&ObservationId::PrimitiveOutput(original))
            .expect("primitive observation exists")
            .site
            .at;
        let mut placement = adapted
            .candidate
            .placements
            .remove(&original)
            .expect("primitive placement exists");
        placement.id = foreign;
        adapted.candidate.placements.insert(foreign, placement);
        let observation = crate::compile::fragment_synth::candidate::VerifiedObservation {
            site: crate::compile::fragment_synth::identity::ObservationSite {
                id: ObservationId::PrimitiveOutput(foreign),
                at,
                logical_owner: Some(foreign.instance),
                display_label: None,
            },
            state: emitted.world.get(at.x, at.y, at.z).clone(),
        };
        adapted
            .candidate
            .observations
            .insert(ObservationId::PrimitiveOutput(foreign), observation);

        let actual = verify_junction_closure(&adapted.candidate, &emitted);
        assert!(
            matches!(
                actual,
                Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                    junction: rejected_junction,
                    contributor: PhysicalEndpointId::PrimitiveOutput(rejected),
                    route: None,
                    contributor_at,
                    junction_at,
                }) if rejected_junction == junction
                    && rejected == foreign
                    && contributor_at == at
                    && junction_at == expected_junction_at
            ),
            "foreign-instance primitive direct join was accepted: {actual:?}"
        );
    }

    #[test]
    fn repeater_output_face_cannot_be_walked_in_reverse_to_a_junction() {
        let junction = InstanceId(0);
        let primitive = PrimitiveId {
            instance: junction,
            node: TopologyNodeId(0),
        };
        let repeater_at = Anchor { x: 3, y: 1, z: 3 };
        let junction_at = Anchor { x: 4, y: 1, z: 3 };
        let mut candidate =
            candidate_with_path(vec![(Anchor { x: 1, y: 1, z: 1 }, crate::compile::dust())]);
        candidate.routes.clear();
        candidate.junctions.insert(
            junction,
            RealisedJunction {
                id: junction,
                at: junction_at,
                facing: crate::compile::geometry::CellFacing::NORTH,
                contributors: vec![PhysicalEndpointId::PrimitiveOutput(primitive)],
                cells: vec![PlacedBlock {
                    at: junction_at,
                    state: crate::compile::dust(),
                }],
            },
        );
        candidate.observations.insert(
            ObservationId::PrimitiveOutput(primitive),
            crate::compile::fragment_synth::candidate::VerifiedObservation {
                site: crate::compile::fragment_synth::identity::ObservationSite {
                    id: ObservationId::PrimitiveOutput(primitive),
                    at: repeater_at,
                    logical_owner: Some(junction),
                    display_label: None,
                },
                state: crate::compile::repeater(Facing::East),
            },
        );

        let emit = |repeater| {
            emit_typed(
                &ShortedRoutes {
                    blocks: vec![
                        (
                            repeater_at,
                            repeater,
                            PhysicalBlockRole::Primitive(primitive),
                        ),
                        (
                            junction_at,
                            crate::compile::dust(),
                            PhysicalBlockRole::Junction(junction),
                        ),
                    ],
                },
                (8, 4, 8),
            )
            .expect("fixture emits")
        };
        let forward = emit(crate::compile::repeater(Facing::East));
        assert!(directed_conductor_step(
            &forward.world,
            Position::new(repeater_at.x, repeater_at.y, repeater_at.z),
            Position::new(junction_at.x, junction_at.y, junction_at.z),
        ));
        assert!(directed_conductor_reach(
            &forward.world,
            Position::new(repeater_at.x, repeater_at.y, repeater_at.z),
        )
        .contains(&Position::new(junction_at.x, junction_at.y, junction_at.z,)));
        assert_eq!(verify_junction_closure(&candidate, &forward), Ok(()));

        let reverse = emit(crate::compile::repeater(Facing::West));
        assert!(!directed_conductor_step(
            &reverse.world,
            Position::new(repeater_at.x, repeater_at.y, repeater_at.z),
            Position::new(junction_at.x, junction_at.y, junction_at.z),
        ));
        let actual = verify_junction_closure(&candidate, &reverse);
        assert!(
            matches!(
                actual,
                Err(ExpandedPhysicalError::JunctionContributorDoesNotReach {
                    junction: rejected_junction,
                    contributor: PhysicalEndpointId::PrimitiveOutput(rejected),
                    route: None,
                    contributor_at,
                    junction_at: rejected_junction_at,
                }) if rejected_junction == junction
                    && rejected == primitive
                    && contributor_at == repeater_at
                    && rejected_junction_at == junction_at
            ),
            "repeater output face was traversed in reverse: {actual:?}"
        );
    }

    #[test]
    fn verifier_revisions_distinguish_legacy_compatibility_from_expanded_strict() {
        let legacy = physical_verifier_revision_descriptor();
        let strict = expanded_strict_physical_verifier_revision_descriptor();

        assert_eq!(legacy.policy, PhysicalVerifierPolicy::LegacyCompatibility);
        assert_eq!(strict.policy, PhysicalVerifierPolicy::ExpandedStrict);
        assert_ne!(legacy, strict);
        assert!(!strict.rules.iter().any(|rule| matches!(
            rule.id,
            PhysicalVerifierRuleId::TorchMergeStructure | PhysicalVerifierRuleId::SignalStrength
        )));
    }
}
