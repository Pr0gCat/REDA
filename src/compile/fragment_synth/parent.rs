//! Parent-owned composition: translate certified child worlds into one
//! global world, build the root terminal hardware, and route one trunk per
//! boundary signal through the corridor with the durable physical router.
//!
//! Ownership at a child portal follows the planner's pin contract.  The
//! child owns its handover repeater one cell south of the caller cell; the
//! parent owns the caller cell itself and everything north of it.  A trunk
//! that feeds a child ends with a repeater in the caller cell driving the
//! child's repeater; a trunk fed by a child starts as dust in the caller
//! cell driven by the child's repeater.  Root caller cells on the `z = 0`
//! row stay external: the parent builds a south-facing repeater in each root
//! input's handover cell and lets the router place each root output's
//! terminal repeater in its handover cell, exactly as the seed does for a
//! pinned port.
//!
//! Child cells, pending terminal exits, and every trunk already laid are
//! reserved before a route runs; every routed cell is then checked against
//! the corridor, so a trunk can neither couple to a child nor leave the
//! parent's space.  No wire is drawn by hand.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::allocation::{AllocationPlan, Prism, TrunkEnd, TrunkOwner};
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId,
    TopologyNodeId,
};
use crate::compile::fragment_synth::leaf::LeafArtifact;
use crate::compile::fragment_synth::partition::ChunkId;
use crate::compile::geometry::Anchor;
use crate::compile::planner::PortRole;
use crate::compile::routing::{
    NonEmptyRouteSinks, PhysicalReservationKind, PhysicalReservationOwner, PhysicalReservations,
    PhysicalRouter, RealisedRouteTree, RouteEndpoint, RouteRequest, RouteSink, RouteTarget,
    RouteTerminalKind, RouterFailure, RouterLimits, TerminalContract, TerminalRequirement,
};
use crate::compile::{repeater, stone};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

#[derive(Debug, Clone)]
pub struct ComposedCircuit {
    pub world: World,
    /// One realised tree per plan trunk, in plan order.
    pub trunks: Vec<RealisedRouteTree>,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ComposeError {
    #[error("artifact for chunk {chunk:?} appears more than once")]
    DuplicateArtifact { chunk: ChunkId },
    #[error("the plan allocates chunk {chunk:?} but no artifact was given")]
    MissingArtifact { chunk: ChunkId },
    #[error("artifact for chunk {chunk:?} has no allocation in the plan")]
    UnexpectedArtifact { chunk: ChunkId },
    #[error("translating chunk {chunk:?} overflows i32")]
    CoordinateOverflow { chunk: ChunkId },
    #[error("chunk {chunk:?} cell lands at {at:?}, outside its allocated region")]
    Escape { chunk: ChunkId, at: Anchor },
    #[error("{at:?} is already occupied")]
    Overlap { at: Anchor },
    #[error("parent cell {at:?} is outside the composed world")]
    OutOfBounds { at: Anchor },
    #[error("route floor at {at:?} collides with {kind:?}")]
    InvalidFloor { at: Anchor, kind: BlockKind },
    #[error("parent corridor is only {depth} cells deep; at least 6 are required")]
    CorridorTooShallow { depth: i32 },
    #[error("trunk {signal} end has no portal on its child")]
    MissingPortal { signal: String },
    #[error("trunk {signal} has no sink")]
    NoSinks { signal: String },
    #[error("trunk {signal} could not be routed: {failure}")]
    Route {
        signal: String,
        #[source]
        failure: RouterFailure,
    },
    #[error("trunk {signal} routed through {at:?}, outside the corridor")]
    RouteEscaped { signal: String, at: Anchor },
}

fn place(world: &mut World, at: Anchor, state: BlockState) -> Result<(), ComposeError> {
    if world.index(at.x, at.y, at.z).is_none() {
        return Err(ComposeError::OutOfBounds { at });
    }
    if world.get(at.x, at.y, at.z).kind != BlockKind::Air {
        return Err(ComposeError::Overlap { at });
    }
    world.set(at.x, at.y, at.z, state);
    Ok(())
}

fn route_clearance(at: Anchor) -> BTreeSet<Anchor> {
    let mut cells = BTreeSet::new();
    for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
        let side = step(at, facing);
        for dy in -1..=1 {
            if side.y + dy >= 0 {
                cells.insert(Anchor {
                    y: side.y + dy,
                    ..side
                });
            }
        }
    }
    cells
}

/// One trunk end resolved to the router's vocabulary.
struct Terminal {
    endpoint: PhysicalEndpointId,
    /// Cell the route starts at or terminates in.
    anchor: Anchor,
    /// Toward the corridor: a source's exit, a sink's entry.
    facing: Facing,
    /// Sink only: the cell the terminal drives.
    support: Anchor,
    target: Option<RouteTarget>,
}

fn resolve(plan: &AllocationPlan, signal: &str, end: &TrunkEnd) -> Result<Terminal, ComposeError> {
    let missing = || ComposeError::MissingPortal {
        signal: signal.to_owned(),
    };
    match (&end.owner, end.role) {
        (TrunkOwner::Root, role) => {
            let index = plan
                .root_ports
                .iter()
                .position(|port| port.signal == signal && port.role == role)
                .ok_or_else(missing)?;
            let port = PortId(u32::try_from(index).map_err(|_| missing())?);
            let handover = end.pin.handover(role);
            Ok(match role {
                PortRole::Input => Terminal {
                    endpoint: PhysicalEndpointId::PrimaryInput(port),
                    anchor: end.pin.net_cell(role),
                    facing: end.pin.toward,
                    support: handover,
                    target: None,
                },
                PortRole::Output => Terminal {
                    endpoint: PhysicalEndpointId::DeclaredOutput(port),
                    anchor: handover,
                    facing: end.pin.toward.opposite(),
                    support: end.pin.at,
                    target: Some(RouteTarget::DeclaredOutput(port)),
                },
            })
        }
        (TrunkOwner::Child(chunk), role) => {
            let child_index = plan
                .children
                .iter()
                .position(|child| &child.chunk == chunk)
                .ok_or_else(missing)?;
            let child = &plan.children[child_index];
            let portal_index = child
                .portals
                .iter()
                .position(|portal| portal.signal == signal && portal.role == role)
                .ok_or_else(missing)?;
            let instance = InstanceId(u32::try_from(child_index).map_err(|_| missing())?);
            let slot = u16::try_from(portal_index).map_err(|_| missing())?;
            let handover = end.pin.handover(role);
            Ok(match role {
                PortRole::Output => Terminal {
                    endpoint: PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                        instance,
                        node: TopologyNodeId(slot),
                    }),
                    anchor: end.pin.at,
                    facing: end.pin.toward,
                    support: handover,
                    target: None,
                },
                PortRole::Input => {
                    let connection = ConnectionId::External {
                        instance,
                        input_index: slot,
                    };
                    Terminal {
                        endpoint: PhysicalEndpointId::Landing(connection),
                        anchor: end.pin.at,
                        facing: end.pin.toward.opposite(),
                        support: handover,
                        target: Some(RouteTarget::Connection(connection)),
                    }
                }
            })
        }
    }
}

fn reserve_box(
    reservations: &mut PhysicalReservations,
    prism: &Prism,
    owner: PhysicalReservationOwner,
) {
    for y in prism.min.y..=prism.max.y {
        for z in prism.min.z..=prism.max.z {
            for x in prism.min.x..=prism.max.x {
                reservations.reserve(Anchor { x, y, z }, owner, PhysicalReservationKind::KeepOut);
            }
        }
    }
}

fn step(at: Anchor, facing: Facing) -> Anchor {
    let next = Position::new(at.x, at.y, at.z).offset(facing);
    Anchor {
        x: next.x,
        y: next.y,
        z: next.z,
    }
}

/// The cells a trunk end needs kept free until its own trunk runs: its own
/// three-wide column through the access band at every height, plus the one-cell ring
/// the router's coupling rule reads around the anchor and the exit cell.
fn guard_cells(terminal: &Terminal, top: i32) -> Vec<Anchor> {
    let exit = step(terminal.anchor, terminal.facing);
    let mut cells = Vec::new();
    for core in [terminal.anchor, exit, step(exit, terminal.facing)] {
        for dx in -1..=1 {
            for y in 0..=top {
                cells.push(Anchor {
                    x: core.x + dx,
                    y,
                    ..core
                });
            }
        }
    }
    for core in [terminal.anchor, exit] {
        for facing in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let side = step(core, facing);
            for dy in [-1, 0, 1] {
                cells.push(Anchor {
                    y: side.y + dy,
                    ..side
                });
            }
        }
    }
    cells.retain(|at| at.y >= 0);
    cells
}

/// Compose `artifacts` under `plan` and route every trunk with `router`.
pub fn compose(
    plan: &AllocationPlan,
    artifacts: &[LeafArtifact],
    router: &impl PhysicalRouter,
    limits: RouterLimits,
) -> Result<ComposedCircuit, ComposeError> {
    let mut by_id: BTreeMap<&ChunkId, &LeafArtifact> = BTreeMap::new();
    let mut duplicates = BTreeSet::new();
    for artifact in artifacts {
        if by_id.insert(&artifact.chunk, artifact).is_some() {
            duplicates.insert(artifact.chunk.clone());
        }
    }
    if let Some(chunk) = duplicates.into_iter().next() {
        return Err(ComposeError::DuplicateArtifact { chunk });
    }
    let leaves: Vec<&LeafArtifact> = plan
        .children
        .iter()
        .map(|child| {
            by_id
                .remove(&child.chunk)
                .ok_or_else(|| ComposeError::MissingArtifact {
                    chunk: child.chunk.clone(),
                })
        })
        .collect::<Result<_, _>>()?;
    if let Some(chunk) = by_id.keys().next() {
        return Err(ComposeError::UnexpectedArtifact {
            chunk: (*chunk).clone(),
        });
    }
    let ends: Vec<(Terminal, Vec<Terminal>)> = plan
        .trunks
        .iter()
        .map(|trunk| {
            Ok((
                resolve(plan, &trunk.signal, &trunk.source)?,
                trunk
                    .sinks
                    .iter()
                    .map(|end| resolve(plan, &trunk.signal, end))
                    .collect::<Result<_, _>>()?,
            ))
        })
        .collect::<Result<_, ComposeError>>()?;

    let corridor = plan.corridor.region;
    let corridor_depth = corridor.max.z - corridor.min.z + 1;
    // Below six rows the root and child terminal runways overlap, leaving at
    // least one endpoint permanently guarded by the other.
    if corridor_depth < 6 {
        return Err(ComposeError::CorridorTooShallow {
            depth: corridor_depth,
        });
    }
    let far = plan
        .children
        .iter()
        .map(|child| child.halo.max)
        .fold(corridor.max, |far, at| Anchor {
            x: far.x.max(at.x),
            y: far.y.max(at.y),
            z: far.z.max(at.z),
        });
    let mut world = World::new(
        far.x
            .checked_add(1)
            .ok_or(ComposeError::OutOfBounds { at: far })?,
        far.y
            .checked_add(1)
            .ok_or(ComposeError::OutOfBounds { at: far })?,
        far.z
            .checked_add(1)
            .ok_or(ComposeError::OutOfBounds { at: far })?,
    );
    let mut reservations = PhysicalReservations::new();

    // 1. Children: translate every occupied cell and reserve it as the exact
    //    conductor it is, so no trunk may touch or neighbour it.
    for (index, (child, leaf)) in plan.children.iter().zip(&leaves).enumerate() {
        let (sx, sy, sz) = leaf.world.size();
        let overflow = || ComposeError::CoordinateOverflow {
            chunk: child.chunk.clone(),
        };
        for y in 0..sy {
            for z in 0..sz {
                for x in 0..sx {
                    let state = leaf.world.get(x, y, z);
                    if state.kind == BlockKind::Air {
                        continue;
                    }
                    let at = Anchor {
                        x: x.checked_add(child.origin.x).ok_or_else(overflow)?,
                        y: y.checked_add(child.origin.y).ok_or_else(overflow)?,
                        z: z.checked_add(child.origin.z).ok_or_else(overflow)?,
                    };
                    if !child.region.contains(at) {
                        return Err(ComposeError::Escape {
                            chunk: child.chunk.clone(),
                            at,
                        });
                    }
                    place(&mut world, at, state.clone())?;
                    reservations.reserve(
                        at,
                        PhysicalReservationOwner::KeepOut(index as u32),
                        PhysicalReservationKind::Conductor(state.clone()),
                    );
                }
            }
        }
    }

    // 2. Root inputs: parent-owned handover repeater on a floor, as the seed
    //    builds for a pinned input.  Root outputs get their terminal repeater
    //    from the router.
    for port in &plan.root_ports {
        if port.role == PortRole::Input {
            let handover = port.pin.handover(PortRole::Input);
            let floor = Anchor {
                y: handover
                    .y
                    .checked_sub(1)
                    .ok_or(ComposeError::OutOfBounds { at: handover })?,
                ..handover
            };
            place(&mut world, floor, stone())?;
            place(&mut world, handover, repeater(port.pin.toward))?;
            reservations.reserve(
                floor,
                PhysicalReservationOwner::KeepOut(u32::MAX),
                PhysicalReservationKind::Floor(stone()),
            );
            reservations.reserve(
                handover,
                PhysicalReservationOwner::KeepOut(u32::MAX),
                PhysicalReservationKind::Conductor(repeater(port.pin.toward)),
            );
        }
    }

    // 3. Every trunk end's anchor, exit/approach and ring are held by the end
    //    itself until its trunk runs, so no earlier trunk can occupy or
    //    neighbour them.
    for (source, sinks) in &ends {
        for terminal in std::iter::once(source).chain(sinks) {
            for at in guard_cells(terminal, far.y) {
                reservations.reserve(
                    at,
                    PhysicalReservationOwner::Endpoint(terminal.endpoint),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }

    // 4. The two rows beside each portal row are an access band: a trunk
    //    crosses them only in its own guarded column, so no trunk can wander
    //    along a portal row and box in a later trunk's exit.
    for z in [1, 2, corridor.max.z - 1, corridor.max.z] {
        reserve_box(
            &mut reservations,
            &Prism {
                min: Anchor { x: 0, y: 0, z },
                max: Anchor {
                    x: far.x,
                    y: far.y,
                    z,
                },
            },
            PhysicalReservationOwner::KeepOut(u32::MAX - 1),
        );
    }

    // 5. Child halos and the external root row are guarded until their own
    // endpoint is consumed; the post-route corridor check remains authority.
    for (index, child) in plan.children.iter().enumerate() {
        reserve_box(
            &mut reservations,
            &child.halo,
            PhysicalReservationOwner::KeepOut(index as u32),
        );
    }
    reserve_box(
        &mut reservations,
        &Prism {
            min: Anchor { x: 0, y: 0, z: 0 },
            max: Anchor {
                x: far.x,
                y: far.y,
                z: 0,
            },
        },
        PhysicalReservationOwner::KeepOut(u32::MAX),
    );

    // 6. Trunks in plan (signal-name) order, each seeing every earlier one.
    let mut trunks = Vec::with_capacity(plan.trunks.len());
    for (index, (trunk, (source, sinks))) in plan.trunks.iter().zip(&ends).enumerate() {
        let route = RouteId(index as u32);
        let mut attempt = reservations.clone();
        for terminal in std::iter::once(source).chain(sinks) {
            for at in guard_cells(terminal, far.y) {
                attempt.release_endpoint_keep_out(at, terminal.endpoint);
            }
        }
        let route_sinks = sinks
            .iter()
            .enumerate()
            .map(|(ordinal, sink)| RouteSink {
                id: RoutedSinkId {
                    route,
                    ordinal: ordinal as u16,
                },
                endpoint: sink.endpoint,
                anchor: sink.anchor,
                allowed_entry: sink.facing,
                terminal: TerminalContract::Sink {
                    target: sink.target.expect("sinks resolve with a target"),
                    support: sink.support,
                    requirement: match sink.target {
                        Some(RouteTarget::DeclaredOutput(_)) => {
                            TerminalRequirement::Exact(RouteTerminalKind::OutputTerminalRepeater)
                        }
                        _ => TerminalRequirement::Repeater,
                    },
                },
            })
            .collect();
        let route_sinks =
            NonEmptyRouteSinks::new(route_sinks).map_err(|_| ComposeError::NoSinks {
                signal: trunk.signal.clone(),
            })?;
        let tree = router
            .route(RouteRequest {
                id: route,
                source: RouteEndpoint {
                    id: source.endpoint,
                    anchor: source.anchor,
                    allowed_exit: source.facing,
                    terminal: TerminalContract::Source {
                        signal_strength: MAX_SIGNAL_STRENGTH,
                    },
                },
                sinks: &route_sinks,
                reservations: &attempt,
                limits,
            })
            .map_err(|failure| ComposeError::Route {
                signal: trunk.signal.clone(),
                failure,
            })?;

        // A trunk may leave the corridor only at its own terminal cells and
        // the floor it lays under them.
        let allowed = |at: Anchor| {
            corridor.contains(at)
                || std::iter::once(source).chain(sinks).any(|terminal| {
                    at == terminal.anchor
                        || at
                            == Anchor {
                                y: terminal.anchor.y - 1,
                                ..terminal.anchor
                            }
                })
        };
        for block in tree.owned_blocks() {
            if !allowed(block.at) {
                return Err(ComposeError::RouteEscaped {
                    signal: trunk.signal.clone(),
                    at: block.at,
                });
            }
        }
        // This endpoint is now consumed. Release all of its temporary runway
        // and replace it with clearance around the route that was actually laid.
        for terminal in std::iter::once(source).chain(sinks) {
            for at in guard_cells(terminal, far.y) {
                reservations.release_endpoint_keep_out(at, terminal.endpoint);
            }
        }
        for block in &tree.cells {
            place(&mut world, block.at, block.state.clone())?;
            reservations.reserve(
                block.at,
                PhysicalReservationOwner::Route(route),
                PhysicalReservationKind::Conductor(block.state.clone()),
            );
        }
        for block in &tree.floors {
            if world.index(block.at.x, block.at.y, block.at.z).is_none() {
                return Err(ComposeError::OutOfBounds { at: block.at });
            }
            let existing = world.get(block.at.x, block.at.y, block.at.z);
            if existing.kind == BlockKind::Air {
                world.set(block.at.x, block.at.y, block.at.z, block.state.clone());
            } else if existing != &block.state {
                return Err(ComposeError::InvalidFloor {
                    at: block.at,
                    kind: existing.kind,
                });
            }
            reservations.reserve(
                block.at,
                PhysicalReservationOwner::RouteStair(route),
                PhysicalReservationKind::Floor(block.state.clone()),
            );
        }
        for block in &tree.cells {
            for at in route_clearance(block.at) {
                if reservations.get(&at).is_none() {
                    reservations.reserve(
                        at,
                        PhysicalReservationOwner::KeepOut(route.0),
                        PhysicalReservationKind::KeepOut,
                    );
                }
            }
        }
        trunks.push(tree);
    }

    Ok(ComposedCircuit { world, trunks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::{allocate, AllocationLimits};
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::fragment_synth::schedule::synthesise_children;
    use crate::compile::routing::DurablePhysicalRouter;
    use crate::compile::{drive_caller_cell, probe_caller_cell, Gate, Netlist};
    use crate::redstone::simulator::Simulator;

    const LIMITS: AllocationLimits = AllocationLimits {
        delay_budget_ticks: 4,
        corridor_capacity: 8,
    };

    fn netlist(inputs: &[&str], outputs: &[&str], gates: Vec<Gate>) -> Netlist {
        Netlist {
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            gates,
        }
    }

    fn composed(net: &Netlist) -> (AllocationPlan, ComposedCircuit) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), 1).unwrap();
        let plan = allocate(net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 2).unwrap();
        let circuit = compose(
            &plan,
            &artifacts,
            &DurablePhysicalRouter,
            SearchConfig::checked_defaults().router_limits,
        )
        .unwrap();
        (plan, circuit)
    }

    fn observe(plan: &AllocationPlan, circuit: &ComposedCircuit, inputs: &[bool]) -> Vec<bool> {
        let mut world = circuit.world.clone();
        let mut input = inputs.iter().copied();
        for port in &plan.root_ports {
            let at = (port.pin.at.x, port.pin.at.y, port.pin.at.z);
            match port.role {
                PortRole::Input => drive_caller_cell(&mut world, at, input.next().unwrap()),
                PortRole::Output => probe_caller_cell(&mut world, at),
            }
        }
        assert!(input.next().is_none());
        let mut simulator = Simulator::new(world);
        simulator.run_until_stable(400).unwrap();
        plan.root_ports
            .iter()
            .filter(|port| port.role == PortRole::Output)
            .map(|port| {
                simulator
                    .world()
                    .get(port.pin.at.x, port.pin.at.y, port.pin.at.z)
                    .lit
            })
            .collect()
    }

    #[test]
    fn odd_inversion_composes_without_shorting_input_to_output() {
        let net = netlist(&["x"], &["y"], vec![Gate::nor("y", &["x"])]);
        let (plan, circuit) = composed(&net);
        assert_eq!(circuit.trunks.len(), 2);
        for (trunk, tree) in plan.trunks.iter().zip(&circuit.trunks) {
            assert_eq!(tree.branches.len(), trunk.sinks.len());
        }
        assert_eq!(observe(&plan, &circuit, &[false]), [true]);
        assert_eq!(observe(&plan, &circuit, &[true]), [false]);
    }

    #[test]
    fn fanout_shares_one_trunk_and_drives_both_outputs() {
        let net = netlist(
            &["x"],
            &["b", "c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        );
        let (plan, circuit) = composed(&net);
        let a = plan.trunks.iter().position(|t| t.signal == "a").unwrap();
        assert_eq!(circuit.trunks[a].branches.len(), 2);
        assert_eq!(observe(&plan, &circuit, &[false]), [false, false]);
        assert_eq!(observe(&plan, &circuit, &[true]), [true, true]);
    }

    #[test]
    fn independent_inputs_do_not_couple_in_the_shared_corridor() {
        let net = netlist(
            &["x", "y"],
            &["nx", "ny"],
            vec![Gate::nor("nx", &["x"]), Gate::nor("ny", &["y"])],
        );
        let (plan, circuit) = composed(&net);
        assert_eq!(observe(&plan, &circuit, &[false, true]), [true, false]);
        assert_eq!(observe(&plan, &circuit, &[true, false]), [false, true]);
    }

    #[test]
    fn malformed_artifact_sets_are_typed() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let plan = allocate(&net, &chunks, LIMITS).unwrap();
        let artifacts = synthesise_children(&chunks, &plan, 1).unwrap();
        let limits = SearchConfig::checked_defaults().router_limits;
        assert_eq!(
            compose(&plan, &artifacts[..1], &DurablePhysicalRouter, limits).err(),
            Some(ComposeError::MissingArtifact {
                chunk: plan.children[1].chunk.clone()
            })
        );
        let mut doubled = artifacts.clone();
        doubled.push(artifacts[0].clone());
        assert_eq!(
            compose(&plan, &doubled, &DurablePhysicalRouter, limits).err(),
            Some(ComposeError::DuplicateArtifact {
                chunk: artifacts[0].chunk.clone()
            })
        );
    }
}
