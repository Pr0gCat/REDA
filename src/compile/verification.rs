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
use super::planner::{self, PlanCandidate, PlannerError, RealisedCandidate};
use super::routing::route_step_is_legal;
use super::topology::Library;
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
                semantic_version: 1,
            },
            PhysicalVerifierRuleRegistration {
                id: PhysicalVerifierRuleId::Connectivity,
                semantic_version: 1,
            },
            PhysicalVerifierRuleRegistration {
                id: PhysicalVerifierRuleId::Coupling,
                semantic_version: 2,
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
            PhysicalVerifierRuleId::Coupling => verify_typed_coupling(candidate, emitted)?,
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

fn verify_route_continuity(
    candidate: &ExpandedPhysicalCandidate,
    emitted: &EmittedWorld,
) -> Result<(), ExpandedPhysicalError> {
    for route in candidate.routes.values() {
        for branch in &route.branches {
            for &at in &branch.path {
                let actual = emitted.world.get(at.x, at.y, at.z).kind;
                if !matches!(actual, BlockKind::RedstoneWire | BlockKind::Repeater) {
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
    let parent = typed_route_groups(candidate);

    let world = &emitted.world;
    let mut visited = HashSet::<Position>::new();
    for flat in world.positions_of(BlockKind::RedstoneWire) {
        let (x, y, z) = world.decode(flat);
        let start = Position::new(x, y, z);
        if !visited.insert(start) {
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
                    if visited.insert(next) {
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
                        continue;
                    }
                    if matches!(target, PhysicalEndpointId::Landing(_)) {
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
                        continue;
                    }
                    return Err(ExpandedPhysicalError::UnlistedJunctionContributor {
                        junction,
                        contributor: source,
                        route: Some(sink.route),
                        contributor_at: at,
                        junction_at,
                    });
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
    for junction in candidate.junctions.values() {
        let mut joined = routes_by_source
            .get(&PhysicalEndpointId::Junction(junction.id))
            .cloned()
            .unwrap_or_default();
        for contributor in &junction.contributors {
            if let PhysicalEndpointId::Landing(connection) = contributor {
                if let Some(binding) = candidate.connections.get(connection) {
                    joined.push(binding.route);
                }
            }
        }
        union_all(&mut parent, &joined);
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
                blocks: Vec::new(),
            },
            placements: Default::default(),
            boundaries: Default::default(),
            connections: Default::default(),
            routes: [(route, tree)].into_iter().collect(),
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
