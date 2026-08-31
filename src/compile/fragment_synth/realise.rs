//! Lossless expanded-candidate adaptation and realisation orchestration.
//!
//! Structural certification deliberately happens before block adaptation or
//! emission.  Physical certification is a separate durable stage: until
//! `compile::verification` exposes that stage for expanded candidates, the
//! public orchestration returns an explicit error instead of calling an old
//! planner verifier or describing an emitted world as physically certified.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::compile::emission::{
    emit_candidate, EmissionError, EmittedWorld, PhysicalBlockRef, PhysicalBlockRole,
    PhysicalCandidateView, TerminalKind,
};
use crate::compile::fragment_synth::candidate::{
    CandidateError, ExpandedPhysicalCandidate, PlacedBlock, RouteTarget,
};
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, PhysicalEndpointId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::topology::ConnectionTarget;
use crate::compile::fragment_synth::verify::{
    certify_expanded_structure, CertificationError as StructuralCertificationError,
    StructuralCertificate,
};
use crate::compile::geometry::Anchor;
use crate::compile::planner::RouteTerminalKind;
use crate::compile::topology::Library;
use crate::compile::verification::{verify_expanded_candidate, ExpandedPhysicalError};
use crate::compile::Netlist;
use crate::redstone::world::block::BlockKind;
use crate::redstone::world::storage::World;

const WORLD_MARGIN: i32 = 2;

#[derive(Debug, Clone, Copy)]
struct TerminalClaim {
    sink: RoutedSinkId,
    target: PhysicalEndpointId,
    kind: TerminalKind,
    repeaters: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalCellOwner {
    Primitive(PrimitiveId),
    Boundary(PhysicalEndpointId),
    Junction(InstanceId),
    Route(RouteId),
    RouteFloor(RouteId),
}

impl TerminalClaim {
    fn role(self) -> PhysicalBlockRole {
        PhysicalBlockRole::RouteTerminal {
            sink: self.sink,
            target: self.target,
            kind: self.kind,
            repeaters: self.repeaters,
        }
    }
}

/// A checked, name-free view of an expanded candidate's exact block claims.
///
/// Construction proves every route terminal names exactly one already-owned
/// block with the recorded state.  Visiting then replaces that block's generic
/// owner role with the typed terminal role, so the physical cell is never
/// claimed twice merely to retain terminal metadata.
pub(crate) struct ExpandedCandidateAdapter<'a> {
    candidate: &'a ExpandedPhysicalCandidate,
    terminals: BTreeMap<Anchor, TerminalClaim>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExpandedAdapterError {
    #[error("expanded candidate ownership is invalid: {0}")]
    Candidate(#[from] CandidateError),
    #[error("route terminals {first:?} and {second:?} both name physical cell {at:?}")]
    DuplicateTerminalCell {
        at: Anchor,
        first: RoutedSinkId,
        second: RoutedSinkId,
    },
    #[error("route terminal {sink:?} at {at:?} has no physical owner")]
    MissingTerminalCell { sink: RoutedSinkId, at: Anchor },
    #[error("route terminal {sink:?} at {at:?} disagrees with its physical owner's state")]
    TerminalStateMismatch { sink: RoutedSinkId, at: Anchor },
    #[error("route terminal {sink:?} at {at:?} is owned by an incompatible physical component")]
    TerminalOwnerMismatch { sink: RoutedSinkId, at: Anchor },
    #[error("candidate block {at:?} cannot be represented in an origin-based world")]
    NegativeWorldCoordinate { at: Anchor },
    #[error("candidate block {at:?} plus the world margin exceeds i32 dimensions")]
    WorldDimensionOverflow { at: Anchor },
}

impl<'a> ExpandedCandidateAdapter<'a> {
    pub(crate) fn new(
        candidate: &'a ExpandedPhysicalCandidate,
    ) -> Result<Self, ExpandedAdapterError> {
        candidate.validate_physical_ownership()?;

        let mut terminals = BTreeMap::<Anchor, TerminalClaim>::new();
        for route in candidate.routes.values() {
            for branch in &route.branches {
                let claim = TerminalClaim {
                    sink: branch.sink,
                    target: target_endpoint(branch.target),
                    kind: terminal_kind(branch.terminal.kind),
                    repeaters: branch.terminal.repeaters,
                };
                if let Some(first) = terminals.insert(branch.terminal.at, claim) {
                    return Err(ExpandedAdapterError::DuplicateTerminalCell {
                        at: branch.terminal.at,
                        first: first.sink,
                        second: branch.sink,
                    });
                }

                let mut owner = None;
                for (block, physical_owner) in
                    owned_blocks(candidate).filter(|(block, _)| block.at == branch.terminal.at)
                {
                    if block.state != branch.terminal.state {
                        return Err(ExpandedAdapterError::TerminalStateMismatch {
                            sink: branch.sink,
                            at: branch.terminal.at,
                        });
                    }
                    if owner.replace(block).is_some() {
                        return Err(ExpandedAdapterError::DuplicateTerminalCell {
                            at: branch.terminal.at,
                            first: branch.sink,
                            second: branch.sink,
                        });
                    }
                    if !terminal_owner_matches(
                        candidate,
                        route.id,
                        route.source,
                        branch,
                        physical_owner,
                    ) {
                        return Err(ExpandedAdapterError::TerminalOwnerMismatch {
                            sink: branch.sink,
                            at: branch.terminal.at,
                        });
                    }
                }
                if owner.is_none() {
                    return Err(ExpandedAdapterError::MissingTerminalCell {
                        sink: branch.sink,
                        at: branch.terminal.at,
                    });
                }
            }
        }

        Ok(Self {
            candidate,
            terminals,
        })
    }

    fn role_at(&self, at: Anchor, fallback: PhysicalBlockRole) -> PhysicalBlockRole {
        self.terminals
            .get(&at)
            .copied()
            .map(TerminalClaim::role)
            .unwrap_or(fallback)
    }

    pub(crate) fn deterministic_world_size(&self) -> Result<(i32, i32, i32), ExpandedAdapterError> {
        let mut maximum = None::<Anchor>;
        let mut error = None;
        self.visit_blocks(&mut |block| {
            if error.is_some() {
                return;
            }
            if block.at.x < 0 || block.at.y < 0 || block.at.z < 0 {
                error = Some(ExpandedAdapterError::NegativeWorldCoordinate { at: block.at });
                return;
            }
            maximum = Some(match maximum {
                Some(current) => Anchor {
                    x: current.x.max(block.at.x),
                    y: current.y.max(block.at.y),
                    z: current.z.max(block.at.z),
                },
                None => block.at,
            });
        });
        if let Some(error) = error {
            return Err(error);
        }
        let Some(maximum) = maximum else {
            return Ok((1, 1, 1));
        };
        let dimension = |coordinate: i32| {
            coordinate
                .checked_add(1)
                .and_then(|value| value.checked_add(WORLD_MARGIN))
                .ok_or(ExpandedAdapterError::WorldDimensionOverflow { at: maximum })
        };
        Ok((
            dimension(maximum.x)?,
            dimension(maximum.y)?,
            dimension(maximum.z)?,
        ))
    }
}

impl PhysicalCandidateView for ExpandedCandidateAdapter<'_> {
    fn visit_blocks(&self, visitor: &mut dyn FnMut(PhysicalBlockRef<'_>)) {
        for (&id, placement) in &self.candidate.placements {
            visit_owned(
                self,
                &placement.blocks,
                PhysicalBlockRole::Primitive(id),
                visitor,
            );
        }
        for (&endpoint, boundary) in &self.candidate.boundaries {
            for block in &boundary.blocks {
                let fallback = if matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_))
                    && block.state.kind == BlockKind::Lamp
                {
                    PhysicalBlockRole::DeclaredOutputLamp(endpoint)
                } else {
                    PhysicalBlockRole::Boundary(endpoint)
                };
                visitor(PhysicalBlockRef {
                    at: block.at,
                    state: &block.state,
                    role: self.role_at(block.at, fallback),
                });
            }
        }
        for (&id, junction) in &self.candidate.junctions {
            visit_owned(
                self,
                &junction.cells,
                PhysicalBlockRole::Junction(id),
                visitor,
            );
        }
        for (&id, route) in &self.candidate.routes {
            visit_owned(
                self,
                &route.cells,
                PhysicalBlockRole::RouteConductor(id),
                visitor,
            );
            visit_owned(
                self,
                &route.floors,
                PhysicalBlockRole::RouteFloor(id),
                visitor,
            );
        }
    }
}

fn visit_owned<'a>(
    adapter: &ExpandedCandidateAdapter<'_>,
    blocks: &'a [PlacedBlock],
    fallback: PhysicalBlockRole,
    visitor: &mut dyn FnMut(PhysicalBlockRef<'a>),
) {
    for block in blocks {
        visitor(PhysicalBlockRef {
            at: block.at,
            state: &block.state,
            role: adapter.role_at(block.at, fallback),
        });
    }
}

fn owned_blocks(
    candidate: &ExpandedPhysicalCandidate,
) -> impl Iterator<Item = (&PlacedBlock, TerminalCellOwner)> {
    candidate
        .placements
        .iter()
        .flat_map(|(&primitive, placement)| {
            placement
                .blocks
                .iter()
                .map(move |block| (block, TerminalCellOwner::Primitive(primitive)))
        })
        .chain(
            candidate
                .boundaries
                .iter()
                .flat_map(|(&endpoint, boundary)| {
                    boundary
                        .blocks
                        .iter()
                        .map(move |block| (block, TerminalCellOwner::Boundary(endpoint)))
                }),
        )
        .chain(
            candidate
                .junctions
                .iter()
                .flat_map(|(&junction, placement)| {
                    placement
                        .cells
                        .iter()
                        .map(move |block| (block, TerminalCellOwner::Junction(junction)))
                }),
        )
        .chain(candidate.routes.iter().flat_map(|(&route, placement)| {
            placement
                .cells
                .iter()
                .map(move |block| (block, TerminalCellOwner::Route(route)))
                .chain(
                    placement
                        .floors
                        .iter()
                        .map(move |block| (block, TerminalCellOwner::RouteFloor(route))),
                )
        }))
}

fn terminal_owner_matches(
    candidate: &ExpandedPhysicalCandidate,
    route: RouteId,
    source: PhysicalEndpointId,
    branch: &crate::compile::fragment_synth::candidate::RealisedRouteBranch,
    owner: TerminalCellOwner,
) -> bool {
    use crate::compile::fragment_synth::candidate::DelayedOwner;

    connection_target_owner_matches(candidate, route, branch.target, owner)
        && match branch.terminal.delayed_owner {
            Some(DelayedOwner::Route(expected)) => {
                expected == route && owner == TerminalCellOwner::Route(route)
            }
            Some(DelayedOwner::Primitive(expected)) => {
                owner == TerminalCellOwner::Primitive(expected)
            }
            Some(DelayedOwner::InputBinding(port)) => {
                owner == TerminalCellOwner::Boundary(PhysicalEndpointId::PrimaryInput(port))
            }
            None => match owner {
                TerminalCellOwner::Route(actual) => actual == route,
                TerminalCellOwner::Primitive(primitive) => {
                    source == PhysicalEndpointId::PrimitiveOutput(primitive)
                        && matches!(branch.target, RouteTarget::DeclaredOutput(_))
                }
                TerminalCellOwner::Boundary(endpoint) => source == endpoint,
                TerminalCellOwner::Junction(instance) => {
                    source == PhysicalEndpointId::Junction(instance)
                }
                TerminalCellOwner::RouteFloor(_) => false,
            },
        }
}

fn connection_target_owner_matches(
    candidate: &ExpandedPhysicalCandidate,
    route: RouteId,
    target: RouteTarget,
    owner: TerminalCellOwner,
) -> bool {
    let RouteTarget::Connection(connection) = target else {
        return true;
    };
    let Some(target) = topology_connection_target(candidate, connection) else {
        return false;
    };
    match (target, owner) {
        // An ordinary routed terminal is the concrete Landing(connection).
        // A landing absorbed by a component must instead name the exact
        // primitive or junction selected by the certified topology.
        (_, TerminalCellOwner::Route(actual)) => actual == route,
        (ConnectionTarget::Primitive(expected), TerminalCellOwner::Primitive(actual)) => {
            actual == expected
        }
        (ConnectionTarget::Junction(expected), TerminalCellOwner::Junction(actual)) => {
            actual == expected
        }
        _ => false,
    }
}

fn topology_connection_target(
    candidate: &ExpandedPhysicalCandidate,
    connection: ConnectionId,
) -> Option<ConnectionTarget> {
    let mut targets = candidate
        .instances
        .instances
        .iter()
        .flat_map(|instance| instance.expanded.topology.connections.iter())
        .filter(|specification| specification.id == connection)
        .map(|specification| specification.target);
    let target = targets.next()?;
    targets.next().is_none().then_some(target)
}

fn target_endpoint(target: RouteTarget) -> PhysicalEndpointId {
    match target {
        RouteTarget::Connection(connection) => PhysicalEndpointId::Landing(connection),
        RouteTarget::DeclaredOutput(port) => PhysicalEndpointId::DeclaredOutput(port),
    }
}

fn terminal_kind(kind: RouteTerminalKind) -> TerminalKind {
    match kind {
        RouteTerminalKind::RepeaterIntoSupport => TerminalKind::RepeaterIntoSupport,
        RouteTerminalKind::DirectedDustIntoSupport => TerminalKind::DirectedDustIntoSupport,
        RouteTerminalKind::BareMergeDust => TerminalKind::BareMergeDust,
        RouteTerminalKind::BareMergeRepeater => TerminalKind::BareMergeRepeater,
        RouteTerminalKind::OutputTerminalRepeater => TerminalKind::OutputTerminalRepeater,
    }
}

/// Public emission failure without exposing crate-private emission internals.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(transparent)]
pub struct EmissionFailure(EmissionError);

impl From<EmissionError> for EmissionFailure {
    fn from(source: EmissionError) -> Self {
        Self(source)
    }
}

#[derive(Debug, Error)]
pub enum CertificationError {
    #[error(transparent)]
    Structural(#[from] StructuralCertificationError),
    #[error(transparent)]
    Adapter(#[from] ExpandedAdapterError),
    #[error(transparent)]
    Emission(#[from] EmissionFailure),
    #[error(transparent)]
    Physical(#[from] ExpandedPhysicalError),
}

/// A world for which both structural and physical certification succeeded.
///
/// The fields are private so an emitted-but-unverified world cannot be
/// constructed or relabelled as certified outside this module.
#[derive(Debug, Clone)]
pub struct CertifiedWorld {
    emitted: EmittedWorld,
    structure: StructuralCertificate,
}

impl CertifiedWorld {
    pub fn world(&self) -> &World {
        &self.emitted.world
    }

    pub fn structural_certificate(&self) -> &StructuralCertificate {
        &self.structure
    }

    pub fn into_world(self) -> World {
        self.emitted.world
    }
}

/// Structurally certified and exactly emitted input for the durable physical
/// verifier.  Only that verifier may turn this value into [`CertifiedWorld`].
pub(crate) struct PendingPhysicalVerification {
    pub(crate) emitted: EmittedWorld,
    pub(crate) structure: StructuralCertificate,
}

pub(crate) fn prepare_expanded_for_physical_verification(
    candidate: &ExpandedPhysicalCandidate,
    netlist: &Netlist,
    library: &Library,
) -> Result<PendingPhysicalVerification, CertificationError> {
    let structure = certify_expanded_structure(candidate, netlist, library)?;
    let adapter = ExpandedCandidateAdapter::new(candidate)?;
    let size = adapter.deterministic_world_size()?;
    let emitted = emit_candidate(&adapter, size).map_err(EmissionFailure::from)?;
    Ok(PendingPhysicalVerification { emitted, structure })
}

/// Structurally certify, adapt and emit an expanded candidate, then run the
/// durable typed physical verifier over the exact emitted world and ownership
/// ledger before sealing the result.
pub fn realise_and_verify_expanded(
    candidate: &ExpandedPhysicalCandidate,
    netlist: &Netlist,
    library: &Library,
) -> Result<CertifiedWorld, CertificationError> {
    let pending = prepare_expanded_for_physical_verification(candidate, netlist, library)?;
    verify_expanded_candidate(candidate, &pending.emitted)?;
    Ok(CertifiedWorld {
        emitted: pending.emitted,
        structure: pending.structure,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::legacy_adapter::LegacyCandidateAdapter;
    use crate::compile::topology::GateKind;
    use crate::compile::{compile_legacy, Gate};
    use crate::redstone::world::block::{BlockKind, Facing};

    fn not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![Gate {
                name: "g1".to_string(),
                inputs: vec!["a".to_string()],
                output: "y".to_string(),
                kind: GateKind::Nor(1),
            }],
        }
    }

    fn assert_worlds_identical(left: &World, right: &World) {
        assert_eq!(left.size(), right.size());
        let (size_x, size_y, size_z) = left.size();
        for y in 0..size_y {
            for z in 0..size_z {
                for x in 0..size_x {
                    assert_eq!(left.get(x, y, z), right.get(x, y, z), "({x}, {y}, {z})");
                }
            }
        }
    }

    #[test]
    fn adapter_preserves_exact_terminal_state_and_typed_target_once() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let (terminal_at, exact, sink, target, repeaters) = {
            let (&route_id, route) = adapted
                .candidate
                .routes
                .iter_mut()
                .find(|(_, route)| {
                    route.branches.iter().any(|branch| {
                        route
                            .cells
                            .iter()
                            .any(|block| block.at == branch.terminal.at)
                    })
                })
                .expect("fixture has a route-owned terminal");
            let branch = route
                .branches
                .iter_mut()
                .find(|branch| {
                    route
                        .cells
                        .iter()
                        .any(|block| block.at == branch.terminal.at)
                })
                .expect("route has a physical terminal");
            let terminal_at = branch.terminal.at;
            let mut exact = branch.terminal.state.clone();
            exact.kind = BlockKind::Repeater;
            exact.name = "minecraft:repeater".to_string();
            exact.facing = Some(Facing::West);
            exact.delay = 4;
            exact.power = 13;
            exact
                .extra_properties
                .insert("locked".to_string(), "true".to_string());
            branch.terminal.state = exact.clone();
            branch.terminal.kind = RouteTerminalKind::RepeaterIntoSupport;
            branch.terminal.delayed_owner =
                Some(crate::compile::fragment_synth::candidate::DelayedOwner::Route(route_id));
            let sink = branch.sink;
            let target = target_endpoint(branch.target);
            let repeaters = branch.terminal.repeaters;
            route
                .cells
                .iter_mut()
                .find(|block| block.at == terminal_at)
                .expect("terminal is route-owned")
                .state = exact.clone();
            (terminal_at, exact, sink, target, repeaters)
        };

        let adapter = ExpandedCandidateAdapter::new(&adapted.candidate).unwrap();
        let emitted = emit_candidate(&adapter, compiled.world.size()).unwrap();

        assert_eq!(
            emitted
                .world
                .get(terminal_at.x, terminal_at.y, terminal_at.z),
            &exact
        );
        assert_eq!(
            emitted.owner_at(terminal_at),
            Some(PhysicalBlockRole::RouteTerminal {
                sink,
                target,
                kind: TerminalKind::RepeaterIntoSupport,
                repeaters,
            })
        );
        assert_eq!(
            emitted
                .owners()
                .filter(|(at, _)| *at == terminal_at)
                .count(),
            1,
            "terminal cell is one physical claim, not conductor plus terminal"
        );
    }

    #[test]
    fn expanded_adapter_matches_the_candidates_previous_exact_emission() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let expected = adapted
            .candidate
            .emit_world(compiled.world.size())
            .expect("old exact emitter accepts adapted candidate");
        let adapter = ExpandedCandidateAdapter::new(&adapted.candidate).unwrap();
        let actual = emit_candidate(&adapter, compiled.world.size()).unwrap();

        assert_worlds_identical(&actual.world, &expected);
    }

    #[test]
    fn structural_corruption_is_rejected_before_invalid_geometry_reaches_emission() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        adapted.candidate.instances.instances[0]
            .expanded
            .topology
            .primitives
            .clear();
        adapted
            .candidate
            .placements
            .values_mut()
            .next()
            .unwrap()
            .blocks[0]
            .at = Anchor { x: -1, y: 0, z: 0 };

        assert!(matches!(
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library()),
            Err(CertificationError::Structural(_))
        ));
    }

    #[test]
    fn a_valid_expanded_candidate_is_sealed_only_after_physical_verification() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");

        let certified =
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library())
                .expect("the typed physical verifier accepts the adapted candidate");
        let expected = adapted
            .candidate
            .emit_world(certified.world().size())
            .expect("the previous exact emitter accepts the certified size");
        assert_worlds_identical(certified.world(), &expected);
    }

    #[test]
    fn a_rotated_terminal_repeater_is_rejected_by_typed_physical_verification() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let (&route_id, route) = adapted
            .candidate
            .routes
            .iter()
            .find(|(_, route)| {
                route
                    .branches
                    .iter()
                    .any(|branch| branch.terminal.state.kind == BlockKind::Repeater)
            })
            .expect("the NOR input fixture has a repeater terminal");
        let branch = route
            .branches
            .iter()
            .find(|branch| branch.terminal.state.kind == BlockKind::Repeater)
            .unwrap();
        let sink = branch.sink;
        let at = branch.terminal.at;
        let facing = branch.terminal.state.facing.unwrap();
        let rotated = match facing {
            Facing::East | Facing::West => Facing::North,
            Facing::North | Facing::South => Facing::East,
            Facing::Up | Facing::Down => unreachable!("a route repeater is horizontal"),
        };
        let route = adapted.candidate.routes.get_mut(&route_id).unwrap();
        route
            .branches
            .iter_mut()
            .find(|branch| branch.sink == sink)
            .unwrap()
            .terminal
            .state
            .facing = Some(rotated);
        route
            .cells
            .iter_mut()
            .find(|block| block.at == at)
            .expect("the route owns its ordinary terminal")
            .state
            .facing = Some(rotated);

        assert!(matches!(
            realise_and_verify_expanded(
                &adapted.candidate,
                &netlist,
                &Library::default_library(),
            ),
            Err(CertificationError::Physical(
                ExpandedPhysicalError::WrongRepeaterAxis {
                    sink: rejected,
                    at: rejected_at,
                }
            )) if rejected == sink && rejected_at == at
        ));
    }

    #[test]
    fn a_non_conducting_cell_inside_a_declared_branch_is_not_certified() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let (route_id, broken_at) = adapted
            .candidate
            .routes
            .iter()
            .find_map(|(&route_id, route)| {
                route.branches.iter().find_map(|branch| {
                    branch
                        .path
                        .iter()
                        .skip(1)
                        .take(branch.path.len().saturating_sub(2))
                        .copied()
                        .find(|at| route.cells.iter().any(|block| block.at == *at))
                        .map(|at| (route_id, at))
                })
            })
            .expect("fixture has a route-owned internal branch cell");
        adapted
            .candidate
            .routes
            .get_mut(&route_id)
            .unwrap()
            .cells
            .iter_mut()
            .find(|block| block.at == broken_at)
            .unwrap()
            .state = crate::compile::stone();

        assert!(matches!(
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library(),),
            Err(CertificationError::Physical(_))
        ));
    }

    #[test]
    fn a_terminal_cannot_relabel_a_foreign_routes_conductor() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["y".to_string(), "xa".to_string(), "xb".to_string()],
            gates: vec![
                Gate::merge("y", &["a", "b"]),
                Gate::nor("xa", &["a"]),
                Gate::nor("xb", &["b"]),
            ],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let route_ids = adapted.candidate.routes.keys().copied().collect::<Vec<_>>();
        let victim = route_ids[0];
        let foreign = route_ids
            .iter()
            .copied()
            .find(|route| *route != victim && !adapted.candidate.routes[route].cells.is_empty())
            .expect("fixture has another physical route");
        let foreign_block = adapted.candidate.routes[&foreign]
            .cells
            .iter()
            .find(|block| {
                !adapted.candidate.routes[&foreign]
                    .branches
                    .iter()
                    .any(|branch| branch.terminal.at == block.at)
            })
            .cloned()
            .expect("foreign route has a non-terminal conductor");
        let branch = adapted
            .candidate
            .routes
            .get_mut(&victim)
            .unwrap()
            .branches
            .first_mut()
            .unwrap();
        branch.root = foreign_block.at;
        branch.path = vec![foreign_block.at];
        branch.terminal.at = foreign_block.at;
        branch.terminal.state = foreign_block.state;
        branch.terminal.delayed_owner = None;

        assert!(ExpandedCandidateAdapter::new(&adapted.candidate).is_err());
    }

    #[test]
    fn connection_terminal_cannot_claim_an_unrelated_primitive_owner() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: vec!["x".to_string(), "y".to_string()],
            gates: vec![Gate::nor("x", &["a"]), Gate::nor("y", &["b"])],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let (connection, intended) = adapted
            .candidate
            .instances
            .instances
            .iter()
            .flat_map(|instance| instance.expanded.topology.connections.iter())
            .find_map(|connection| match connection.target {
                ConnectionTarget::Primitive(primitive) => Some((connection.id, primitive)),
                ConnectionTarget::Junction(_) => None,
            })
            .expect("fixture has a primitive-targeted connection");
        let (unrelated, unrelated_block) = adapted
            .candidate
            .placements
            .iter()
            .find_map(|(&primitive, placement)| {
                (primitive != intended).then(|| {
                    (
                        primitive,
                        placement
                            .blocks
                            .iter()
                            .find(|block| block.state.kind != BlockKind::Solid)
                            .cloned()
                            .expect("a primitive placement has its functional block"),
                    )
                })
            })
            .expect("fixture has another primitive");
        adapted
            .candidate
            .placements
            .get_mut(&unrelated)
            .unwrap()
            .delayed = Some(
            crate::compile::fragment_synth::candidate::DelayedComponent {
                at: unrelated_block.at,
                owner: crate::compile::fragment_synth::candidate::DelayedOwner::Primitive(
                    unrelated,
                ),
            },
        );
        let binding = adapted.candidate.connections[&connection].clone();
        let branch = adapted
            .candidate
            .routes
            .get_mut(&binding.route)
            .unwrap()
            .branches
            .iter_mut()
            .find(|branch| branch.sink == binding.sink)
            .unwrap();
        branch.root = unrelated_block.at;
        branch.path = vec![unrelated_block.at];
        branch.terminal.at = unrelated_block.at;
        branch.terminal.state = unrelated_block.state;
        branch.terminal.delayed_owner =
            Some(crate::compile::fragment_synth::candidate::DelayedOwner::Primitive(unrelated));
        let sink = branch.sink;
        let at = branch.terminal.at;

        let error = match ExpandedCandidateAdapter::new(&adapted.candidate) {
            Ok(_) => panic!("adapter accepted C1's terminal on unrelated primitive P2"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ExpandedAdapterError::TerminalOwnerMismatch {
                sink: rejected,
                at: rejected_at,
            } if rejected == sink && rejected_at == at
        ));
    }

    #[test]
    fn terminal_kind_cannot_claim_dust_when_the_emitted_cell_is_a_repeater() {
        let netlist = not_netlist();
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("legacy fixture adapts");
        let branch = adapted
            .candidate
            .routes
            .values_mut()
            .flat_map(|route| route.branches.iter_mut())
            .find(|branch| branch.terminal.state.kind == BlockKind::Repeater)
            .expect("fixture has a repeater terminal");
        branch.terminal.kind = RouteTerminalKind::DirectedDustIntoSupport;

        assert!(realise_and_verify_expanded(
            &adapted.candidate,
            &netlist,
            &Library::default_library(),
        )
        .is_err());
    }

    #[test]
    fn listed_landing_must_physically_reach_its_junction_observation() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("fixture adapts");
        let (&junction_id, junction) = adapted
            .candidate
            .junctions
            .iter_mut()
            .next()
            .expect("bare merge has a junction");
        let decoy_at = Anchor {
            x: junction.at.x + 64,
            y: junction.at.y,
            z: junction.at.z,
        };
        let decoy_state = crate::compile::dust();
        junction.at = decoy_at;
        junction.cells.push(PlacedBlock {
            at: decoy_at,
            state: decoy_state.clone(),
        });
        for observation_id in [
            crate::compile::fragment_synth::identity::ObservationId::JunctionOutput(junction_id),
            crate::compile::fragment_synth::identity::ObservationId::InstanceOutput(junction_id),
        ] {
            let observation = adapted
                .candidate
                .observations
                .get_mut(&observation_id)
                .expect("junction observation exists");
            observation.site.at = decoy_at;
            observation.state = decoy_state.clone();
        }

        let actual =
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library());
        assert!(
            matches!(
                actual,
                Err(CertificationError::Physical(
                    ExpandedPhysicalError::JunctionContributorDoesNotReach {
                        junction,
                        contributor: PhysicalEndpointId::Landing(_),
                        route: Some(_),
                        junction_at,
                        ..
                    }
                )) if junction == junction_id && junction_at == decoy_at
            ),
            "listed landing whose physical route misses the verified junction was accepted: {actual:?}"
        );
    }

    #[test]
    fn listed_primitive_output_must_physically_reach_its_junction_observation() {
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
        let (&junction_id, junction) = adapted
            .candidate
            .junctions
            .iter_mut()
            .find(|(_, junction)| {
                junction
                    .contributors
                    .iter()
                    .all(|endpoint| matches!(endpoint, PhysicalEndpointId::PrimitiveOutput(_)))
            })
            .expect("fanout inputs produce an all-isolated merge");
        let decoy_at = Anchor {
            x: junction.at.x + 64,
            y: junction.at.y,
            z: junction.at.z,
        };
        let decoy_state = crate::compile::dust();
        junction.at = decoy_at;
        junction.cells.push(PlacedBlock {
            at: decoy_at,
            state: decoy_state.clone(),
        });
        for observation_id in [
            crate::compile::fragment_synth::identity::ObservationId::JunctionOutput(junction_id),
            crate::compile::fragment_synth::identity::ObservationId::InstanceOutput(junction_id),
        ] {
            let observation = adapted
                .candidate
                .observations
                .get_mut(&observation_id)
                .expect("junction observation exists");
            observation.site.at = decoy_at;
            observation.state = decoy_state.clone();
        }

        let actual =
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library());
        assert!(
            matches!(
                actual,
                Err(CertificationError::Physical(
                    ExpandedPhysicalError::JunctionContributorDoesNotReach {
                        junction,
                        contributor: PhysicalEndpointId::PrimitiveOutput(_),
                        route: None,
                        junction_at,
                        ..
                    }
                )) if junction == junction_id && junction_at == decoy_at
            ),
            "listed primitive whose output misses the verified junction was accepted: {actual:?}"
        );
    }

    #[test]
    fn unlisted_same_source_branchless_route_cannot_join_a_junction() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("fixture adapts");
        let (&junction_id, junction) = adapted
            .candidate
            .junctions
            .iter()
            .next()
            .expect("bare merge has a junction");
        let junction_at = junction.at;
        let listed_connection = match junction.contributors[0] {
            PhysicalEndpointId::Landing(connection) => connection,
            other => panic!("bare merge contributor is not a landing: {other:?}"),
        };
        let listed_route = adapted.candidate.connections[&listed_connection].route;
        let source = adapted.candidate.routes[&listed_route].source;
        let new_route = RouteId(
            adapted
                .candidate
                .routes
                .keys()
                .map(|route| route.0)
                .max()
                .unwrap_or(0)
                + 1,
        );
        let occupied = adapted
            .candidate
            .placements
            .values()
            .flat_map(|placement| placement.blocks.iter())
            .chain(
                adapted
                    .candidate
                    .boundaries
                    .values()
                    .flat_map(|placement| placement.blocks.iter()),
            )
            .chain(
                adapted
                    .candidate
                    .junctions
                    .values()
                    .flat_map(|junction| junction.cells.iter()),
            )
            .chain(
                adapted
                    .candidate
                    .routes
                    .values()
                    .flat_map(|route| route.cells.iter().chain(route.floors.iter())),
            )
            .map(|block| block.at)
            .collect::<std::collections::BTreeSet<_>>();
        let spur_at = [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .into_iter()
            .map(|(dx, dz)| Anchor {
                x: junction.at.x + dx,
                y: junction.at.y,
                z: junction.at.z + dz,
            })
            .find(|at| {
                !occupied.contains(at)
                    && !occupied.contains(&Anchor {
                        x: at.x,
                        y: at.y - 1,
                        z: at.z,
                    })
            })
            .expect("junction has a free supported spur direction");
        adapted.candidate.routes.insert(
            new_route,
            crate::compile::fragment_synth::candidate::RealisedRouteTree {
                id: new_route,
                source,
                cells: vec![PlacedBlock {
                    at: spur_at,
                    state: crate::compile::dust(),
                }],
                floors: vec![PlacedBlock {
                    at: Anchor {
                        x: spur_at.x,
                        y: spur_at.y - 1,
                        z: spur_at.z,
                    },
                    state: crate::compile::stone(),
                }],
                branches: Vec::new(),
            },
        );

        let actual =
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library());
        assert!(
            matches!(
                actual,
                Err(CertificationError::Physical(
                    ExpandedPhysicalError::UnlistedJunctionContributor {
                        junction,
                        contributor,
                        route: Some(route),
                        contributor_at,
                        junction_at: rejected_junction_at,
                    }
                )) if junction == junction_id
                    && contributor == source
                    && route == new_route
                    && contributor_at == spur_at
                    && rejected_junction_at == junction_at
            ),
            "unlisted branchless route {new_route:?} joined the junction at {spur_at:?}: {actual:?}"
        );
    }

    #[test]
    fn listed_route_id_does_not_authorise_an_extra_branchless_spur() {
        let netlist = Netlist {
            inputs: vec!["a".to_string(), "b".to_string()],
            outputs: Vec::new(),
            gates: vec![Gate::merge("y", &["a", "b"])],
        };
        let compiled = compile_legacy(&netlist).expect("fixture compiles");
        let mut adapted =
            LegacyCandidateAdapter::adapt(&netlist, &compiled).expect("fixture adapts");
        let (&junction_id, junction) = adapted
            .candidate
            .junctions
            .iter()
            .next()
            .expect("bare merge has a junction");
        let junction_at = junction.at;
        let listed_connection = junction
            .contributors
            .iter()
            .find_map(|contributor| match contributor {
                PhysicalEndpointId::Landing(connection) => Some(*connection),
                _ => None,
            })
            .expect("bare merge has a listed landing");
        let listed_route = adapted.candidate.connections[&listed_connection].route;
        let source = adapted.candidate.routes[&listed_route].source;
        let occupied = adapted
            .candidate
            .placements
            .values()
            .flat_map(|placement| placement.blocks.iter())
            .chain(
                adapted
                    .candidate
                    .boundaries
                    .values()
                    .flat_map(|placement| placement.blocks.iter()),
            )
            .chain(
                adapted
                    .candidate
                    .junctions
                    .values()
                    .flat_map(|junction| junction.cells.iter()),
            )
            .chain(
                adapted
                    .candidate
                    .routes
                    .values()
                    .flat_map(|route| route.cells.iter().chain(route.floors.iter())),
            )
            .map(|block| block.at)
            .collect::<std::collections::BTreeSet<_>>();
        let spur_at = [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .into_iter()
            .map(|(dx, dz)| Anchor {
                x: junction_at.x + dx,
                y: junction_at.y,
                z: junction_at.z + dz,
            })
            .find(|at| {
                !occupied.contains(at)
                    && !occupied.contains(&Anchor {
                        x: at.x,
                        y: at.y - 1,
                        z: at.z,
                    })
            })
            .expect("junction has a free supported spur direction");
        let route = adapted
            .candidate
            .routes
            .get_mut(&listed_route)
            .expect("listed route exists");
        route.cells.push(PlacedBlock {
            at: spur_at,
            state: crate::compile::dust(),
        });
        route.floors.push(PlacedBlock {
            at: Anchor {
                x: spur_at.x,
                y: spur_at.y - 1,
                z: spur_at.z,
            },
            state: crate::compile::stone(),
        });

        let actual =
            realise_and_verify_expanded(&adapted.candidate, &netlist, &Library::default_library());
        assert!(
            matches!(
                actual,
                Err(CertificationError::Physical(
                    ExpandedPhysicalError::UnlistedJunctionContributor {
                        junction,
                        contributor,
                        route: Some(rejected_route),
                        contributor_at,
                        junction_at: rejected_junction_at,
                    }
                )) if junction == junction_id
                    && contributor == source
                    && rejected_route == listed_route
                    && contributor_at == spur_at
                    && rejected_junction_at == junction_at
            ),
            "extra branchless spur on listed route {listed_route:?} was accepted: {actual:?}"
        );
    }
}
