//! Lossless migration of one legacy seed into typed candidate state.
//!
//! This adapter is differential-test scaffolding, not a seed policy for the
//! fragment synthesizer.  It consumes ownership metadata recorded by the old
//! emitter and materialises each primitive independently; it never infers
//! ownership by scanning the completed legacy world.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::compile::fragment_synth::candidate::{
    endpoint_for_driver, BoundaryPlacement, CandidateError, ConnectionBinding, DelayedComponent,
    DelayedOwner, ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedJunction,
    RealisedRouteBranch, RealisedRouteTree, RouteTarget, TerminalRecord, VerifiedObservation,
};
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, ObservationId, ObservationSite, PhysicalEndpointId, PortId, RouteId,
    RoutedSinkId,
};
use crate::compile::fragment_synth::instance_graph::{InstanceGraph, PhysicalSink};
use crate::compile::fragment_synth::topology::{ConnectionTarget, OutputSpec};
use crate::compile::geometry::{self, Anchor};
use crate::compile::planner::{self, NodeRealisation, PortPlacements, PortRole};
use crate::compile::topology::Library;
use crate::compile::{self, physical, CompiledCircuit, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::{BlockKind, BlockState};
use crate::redstone::world::storage::World;

#[derive(Debug, Clone)]
pub struct AdaptedLegacyCandidate {
    pub candidate: ExpandedPhysicalCandidate,
    pub input_positions: BTreeMap<String, (i32, i32, i32)>,
    pub output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    pub gate_facings: Vec<geometry::CellFacing>,
}

pub struct LegacyCandidateAdapter;

/// Test-only explicit baseline seam.  The independent seed builder has no
/// value of this type in its constructor, so legacy generation cannot be
/// injected into production seed construction by accident.
#[cfg(test)]
pub(super) trait LegacyOracle {
    fn compile_legacy(
        &self,
        netlist: &Netlist,
    ) -> Result<CompiledCircuit, crate::compile::CompileError>;
}

#[derive(Debug, Error)]
pub enum LegacyAdapterError {
    #[error("compiled circuit does not carry a matching legacy seed: {0}")]
    LegacySeed(#[from] planner::PlannerError),
    #[error("typed instance graph could not be built: {0}")]
    InstanceGraph(#[from] crate::compile::fragment_synth::instance_graph::SynthesisError),
    #[error("typed candidate could not derive compatibility views: {0}")]
    Candidate(#[from] CandidateError),
    #[error("legacy route `{route}` has malformed block/floor metadata")]
    MalformedRoute { route: String },
    #[error("legacy route `{route}` names unknown source `{signal}`")]
    UnknownRouteSource { route: String, signal: String },
    #[error("legacy route `{route}` names unknown sink `{sink}`")]
    UnknownRouteSink { route: String, sink: String },
    #[error("typed identity width exceeded while adapting legacy metadata")]
    IdentityOverflow,
    #[error("legacy terminal for route `{route}` at {at:?} has no recorded block")]
    MissingTerminalState { route: String, at: Anchor },
}

impl LegacyCandidateAdapter {
    #[cfg(test)]
    pub(super) fn adapt_from_oracle(
        netlist: &Netlist,
        oracle: &dyn LegacyOracle,
    ) -> Result<AdaptedLegacyCandidate, LegacyAdapterError> {
        let compiled = oracle
            .compile_legacy(netlist)
            .map_err(planner::PlannerError::PhysicalInvariant)?;
        Self::adapt(netlist, &compiled)
    }

    pub fn adapt(
        netlist: &Netlist,
        compiled: &CompiledCircuit,
    ) -> Result<AdaptedLegacyCandidate, LegacyAdapterError> {
        let seed = planner::seed_from_legacy(netlist, compiled)?;
        Self::adapt_plan(netlist, &seed, &compiled.world)
    }

    /// Adapt a planner candidate directly, including moved candidates which
    /// no longer carry a replayable legacy emission.
    ///
    /// `world` must be the exact world emitted from `seed`.  It supplies only
    /// observation states; ownership and route topology still come from the
    /// candidate's explicit metadata and are never inferred by scanning block
    /// kinds.
    pub(crate) fn adapt_plan(
        netlist: &Netlist,
        seed: &planner::PlanCandidate,
        world: &World,
    ) -> Result<AdaptedLegacyCandidate, LegacyAdapterError> {
        let instances = InstanceGraph::one_to_one_legacy(netlist, &Library::default_library())?;
        let mut pins = PortPlacements::default();
        for node in seed.primitive_nodes() {
            let (name, toward) = match node.realisation {
                NodeRealisation::InputTerminal { toward } => {
                    (node.id.strip_prefix("input:"), toward)
                }
                NodeRealisation::OutputTerminal { toward } => {
                    (node.id.strip_prefix("output:"), toward)
                }
                _ => continue,
            };
            let name = name.ok_or_else(|| LegacyAdapterError::MalformedRoute {
                route: format!("pinned terminal {} has no port-qualified identity", node.id),
            })?;
            pins.pin(name, node.anchor, toward);
        }
        let mut candidate = ExpandedPhysicalCandidate::empty(instances, pins);
        candidate.bind_pin_contracts(netlist)?;
        let size = world.size();
        let mut source_stubs = Vec::new();

        let input_port_by_name = netlist
            .inputs
            .iter()
            .enumerate()
            .map(|(index, name)| {
                Ok((
                    name.as_str(),
                    PortId(u32::try_from(index).map_err(|_| LegacyAdapterError::IdentityOverflow)?),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, LegacyAdapterError>>()?;
        let gate_by_output = netlist
            .gates
            .iter()
            .enumerate()
            .map(|(index, gate)| (gate.output.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        let output_port_by_name = netlist
            .outputs
            .iter()
            .enumerate()
            .map(|(index, name)| {
                Ok((
                    name.as_str(),
                    PortId(u32::try_from(index).map_err(|_| LegacyAdapterError::IdentityOverflow)?),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, LegacyAdapterError>>()?;

        for (index, gate) in netlist.gates.iter().enumerate() {
            let instance = &candidate.instances.instances[index];
            let anchor = seed.anchors()[index];
            let facing = seed.facing_of(index);
            let (mut blocks, output_at, output_pin) = materialise_gate(gate, anchor, facing, size);

            if let Some(output_port) = netlist.outputs.iter().position(|name| name == &gate.output)
            {
                let endpoint = PhysicalEndpointId::DeclaredOutput(PortId(
                    u32::try_from(output_port).map_err(|_| LegacyAdapterError::IdentityOverflow)?,
                ));
                let port = match endpoint {
                    PhysicalEndpointId::DeclaredOutput(port) => port,
                    _ => unreachable!(),
                };
                if let Some(pin) = candidate.pin_contracts.get(&endpoint).copied() {
                    candidate.boundaries.insert(
                        endpoint,
                        BoundaryPlacement {
                            endpoint,
                            delayed: None,
                            blocks: Vec::new(),
                        },
                    );
                    candidate.observations.insert(
                        ObservationId::DeclaredOutput(port),
                        VerifiedObservation {
                            site: ObservationSite {
                                id: ObservationId::DeclaredOutput(port),
                                at: pin.at,
                                logical_owner: Some(instance.id),
                                display_label: Some(gate.output.clone()),
                            },
                            state: world.get(pin.at.x, pin.at.y, pin.at.z).clone(),
                        },
                    );
                } else {
                    let lamp_at = Anchor {
                        y: output_pin.y - 1,
                        ..output_pin
                    };
                    blocks.retain(|block| block.at != lamp_at);
                    let lamp = PlacedBlock {
                        at: lamp_at,
                        state: compile::lamp(),
                    };
                    candidate.boundaries.insert(
                        endpoint,
                        BoundaryPlacement {
                            endpoint,
                            delayed: None,
                            blocks: vec![lamp.clone()],
                        },
                    );
                    candidate.observations.insert(
                        ObservationId::DeclaredOutput(port),
                        observation(
                            ObservationId::DeclaredOutput(port),
                            lamp,
                            Some(instance.id),
                            Some(gate.output.clone()),
                        ),
                    );
                }
            }

            match &instance.expanded.topology.output {
                OutputSpec::Primitive(primitive) => {
                    source_stubs.push((
                        PhysicalEndpointId::PrimitiveOutput(*primitive),
                        source_stub_blocks(world, output_pin),
                    ));
                    let primitive_kind = instance
                        .expanded
                        .topology
                        .primitives
                        .iter()
                        .find(|specification| specification.id == *primitive)
                        .map(|specification| specification.primitive)
                        .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                            route: format!("primitive {primitive:?} is absent from its topology"),
                        })?;
                    let primitive_blocks =
                        physical_primitive_blocks(primitive_kind, anchor, facing, &blocks)?;
                    candidate.placements.insert(
                        *primitive,
                        PrimitivePlacement {
                            id: *primitive,
                            variant: u16::from(facing.index()),
                            facing,
                            anchor,
                            delayed: primitive_blocks
                                .iter()
                                .find(|block| {
                                    matches!(
                                        block.state.kind,
                                        BlockKind::Torch | BlockKind::Repeater
                                    )
                                })
                                .map(|block| DelayedComponent {
                                    at: block.at,
                                    owner: DelayedOwner::Primitive(*primitive),
                                }),
                            blocks: primitive_blocks,
                        },
                    );
                    let state = world.get(output_at.x, output_at.y, output_at.z).clone();
                    candidate.observations.insert(
                        ObservationId::PrimitiveOutput(*primitive),
                        VerifiedObservation {
                            site: ObservationSite {
                                id: ObservationId::PrimitiveOutput(*primitive),
                                at: output_at,
                                logical_owner: Some(instance.id),
                                display_label: None,
                            },
                            state: state.clone(),
                        },
                    );
                    candidate.observations.insert(
                        ObservationId::InstanceOutput(instance.id),
                        VerifiedObservation {
                            site: ObservationSite {
                                id: ObservationId::InstanceOutput(instance.id),
                                at: output_at,
                                logical_owner: Some(instance.id),
                                display_label: Some(gate.output.clone()),
                            },
                            state,
                        },
                    );
                }
                OutputSpec::Junction { contributors, .. } => {
                    candidate.junctions.insert(
                        instance.id,
                        RealisedJunction {
                            id: instance.id,
                            at: anchor,
                            facing,
                            contributors: contributors.iter().map(contributor_endpoint).collect(),
                            cells: blocks,
                        },
                    );
                    let state = world.get(anchor.x, anchor.y, anchor.z).clone();
                    candidate.observations.insert(
                        ObservationId::JunctionOutput(instance.id),
                        VerifiedObservation {
                            site: ObservationSite {
                                id: ObservationId::JunctionOutput(instance.id),
                                at: anchor,
                                logical_owner: Some(instance.id),
                                display_label: Some(gate.output.clone()),
                            },
                            state: state.clone(),
                        },
                    );
                    candidate.observations.insert(
                        ObservationId::InstanceOutput(instance.id),
                        VerifiedObservation {
                            site: ObservationSite {
                                id: ObservationId::InstanceOutput(instance.id),
                                at: anchor,
                                logical_owner: Some(instance.id),
                                display_label: Some(gate.output.clone()),
                            },
                            state,
                        },
                    );
                }
            }
        }

        for (input_index, name) in netlist.inputs.iter().enumerate() {
            let port = PortId(
                u32::try_from(input_index).map_err(|_| LegacyAdapterError::IdentityOverflow)?,
            );
            let anchor = seed.anchors()[netlist.gates.len() + input_index];
            let node = &seed.primitive_nodes()[netlist.gates.len() + input_index];
            let (blocks, delayed) = match node.realisation {
                NodeRealisation::Primitive(crate::compile::topology::Primitive::Lever) => {
                    let facing = seed.facing_of(netlist.gates.len() + input_index);
                    (materialise_input(anchor, facing, size), None)
                }
                NodeRealisation::InputTerminal { toward } => {
                    let pin = planner::PortPin { at: anchor, toward };
                    let handover = pin.handover(PortRole::Input);
                    (
                        materialise_input_terminal(anchor, toward, size),
                        Some(DelayedComponent {
                            at: handover,
                            owner: DelayedOwner::InputBinding(port),
                        }),
                    )
                }
                _ => {
                    return Err(LegacyAdapterError::MalformedRoute {
                        route: format!("primary input {name} has non-input realisation"),
                    })
                }
            };
            let endpoint = PhysicalEndpointId::PrimaryInput(port);
            let state = world.get(anchor.x, anchor.y, anchor.z).clone();
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed,
                    blocks,
                },
            );
            candidate.observations.insert(
                ObservationId::PrimaryInput(port),
                VerifiedObservation {
                    site: ObservationSite {
                        id: ObservationId::PrimaryInput(port),
                        at: anchor,
                        logical_owner: None,
                        display_label: Some(name.clone()),
                    },
                    state,
                },
            );
        }

        for (route_index, legacy) in seed.routes().iter().enumerate() {
            if legacy.anchors().len() != legacy.realisation().len()
                || legacy.anchors().len() != legacy.floors().len()
                || legacy.terminals().len() != legacy.branch_paths().len()
            {
                return Err(LegacyAdapterError::MalformedRoute {
                    route: format!(
                        "{} (anchors={}, blocks={}, floors={}, terminals={}, branch_paths={})",
                        legacy.id(),
                        legacy.anchors().len(),
                        legacy.realisation().len(),
                        legacy.floors().len(),
                        legacy.terminals().len(),
                        legacy.branch_paths().len(),
                    ),
                });
            }
            let route = RouteId(
                u32::try_from(route_index).map_err(|_| LegacyAdapterError::IdentityOverflow)?,
            );
            let owner = legacy.owner().unwrap_or(legacy.id());
            let source = source_endpoint(
                owner,
                route,
                &input_port_by_name,
                &gate_by_output,
                &candidate,
            )?;
            let mut cells = Vec::new();
            let mut floors = Vec::new();
            for ((&at, _recorded_state), _recorded_floor) in legacy
                .anchors()
                .iter()
                .zip(legacy.realisation())
                .zip(legacy.floors())
            {
                let final_state = world.get(at.x, at.y, at.z);
                if is_route_conductor(final_state) {
                    push_block_once(&mut cells, at, final_state.clone())?;
                } else if final_state.kind != BlockKind::Air {
                    push_block_once(&mut floors, at, final_state.clone())?;
                }
                let floor_at = Anchor { y: at.y - 1, ..at };
                if floor_at.x >= 0
                    && floor_at.y >= 0
                    && floor_at.z >= 0
                    && floor_at.x < size.0
                    && floor_at.y < size.1
                    && floor_at.z < size.2
                {
                    let final_floor = world.get(floor_at.x, floor_at.y, floor_at.z);
                    if final_floor.kind != BlockKind::Air && !is_route_conductor(final_floor) {
                        push_block_once(&mut floors, floor_at, final_floor.clone())?;
                    }
                }
            }

            let mut branches = Vec::new();
            for (ordinal, terminal) in legacy.terminals().iter().enumerate() {
                let path = legacy.branch_paths()[ordinal]
                    .iter()
                    .copied()
                    .filter(|at| is_route_conductor(world.get(at.x, at.y, at.z)))
                    .collect::<Vec<_>>();
                let root = *path
                    .first()
                    .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                        route: format!("{} branch {ordinal} has no conductor", legacy.id()),
                    })?;
                for &at in &path {
                    push_block_once(&mut cells, at, world.get(at.x, at.y, at.z).clone())?;
                    if at.y > 0 {
                        let floor_at = Anchor { y: at.y - 1, ..at };
                        let floor = world.get(floor_at.x, floor_at.y, floor_at.z);
                        if floor.kind != BlockKind::Air && !is_route_conductor(floor) {
                            push_block_once(&mut floors, floor_at, floor.clone())?;
                        }
                    }
                }
                let sink = RoutedSinkId {
                    route,
                    ordinal: u16::try_from(ordinal)
                        .map_err(|_| LegacyAdapterError::IdentityOverflow)?,
                };
                let state = legacy
                    .anchors()
                    .iter()
                    .position(|anchor| anchor == &terminal.sink.anchor)
                    .map(|index| legacy.realisation()[index].clone())
                    .ok_or_else(|| LegacyAdapterError::MissingTerminalState {
                        route: legacy.id().to_string(),
                        at: terminal.sink.anchor,
                    })?;
                if terminal.kind == planner::RouteTerminalKind::OutputTerminalRepeater {
                    let &port = output_port_by_name
                        .get(terminal.sink.gate.as_str())
                        .ok_or_else(|| LegacyAdapterError::UnknownRouteSink {
                            route: legacy.id().to_string(),
                            sink: terminal.sink.gate.clone(),
                        })?;
                    branches.push(RealisedRouteBranch {
                        sink,
                        target: RouteTarget::DeclaredOutput(port),
                        root,
                        path,
                        terminal: TerminalRecord {
                            sink,
                            at: terminal.sink.anchor,
                            state,
                            kind: terminal.kind,
                            repeaters: terminal.repeaters,
                            delayed_owner: Some(DelayedOwner::Route(route)),
                        },
                    });
                    continue;
                }
                let sink_instance =
                    *gate_by_output
                        .get(terminal.sink.gate.as_str())
                        .ok_or_else(|| LegacyAdapterError::UnknownRouteSink {
                            route: legacy.id().to_string(),
                            sink: terminal.sink.gate.clone(),
                        })?;
                let connection = ConnectionId::External {
                    instance: InstanceId(
                        u32::try_from(sink_instance)
                            .map_err(|_| LegacyAdapterError::IdentityOverflow)?,
                    ),
                    input_index: u16::try_from(terminal.sink.input_index)
                        .map_err(|_| LegacyAdapterError::IdentityOverflow)?,
                };
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
                branches.push(RealisedRouteBranch {
                    sink,
                    target: RouteTarget::Connection(connection),
                    root,
                    path,
                    terminal: TerminalRecord {
                        sink,
                        at: terminal.sink.anchor,
                        state: state.clone(),
                        kind: terminal.kind,
                        repeaters: terminal.repeaters,
                        delayed_owner: (state.kind == BlockKind::Repeater)
                            .then_some(DelayedOwner::Route(route)),
                    },
                });
            }
            candidate.routes.insert(
                route,
                RealisedRouteTree {
                    id: route,
                    source,
                    cells,
                    floors,
                    branches,
                },
            );
        }

        materialise_declared_output_routes(&mut candidate)?;
        materialise_merge_primitives(&mut candidate)?;
        materialise_source_stubs(&mut candidate, source_stubs)?;
        normalise_physical_ownership(&mut candidate)?;
        refresh_route_repeater_counts(&mut candidate);

        let compatibility = candidate.compatibility_views(netlist)?;
        Ok(AdaptedLegacyCandidate {
            candidate,
            input_positions: compatibility.input_positions,
            output_positions: compatibility.output_positions,
            gate_output_positions: compatibility.gate_output_positions,
            gate_facings: compatibility.gate_facings,
        })
    }
}

fn materialise_declared_output_routes(
    candidate: &mut ExpandedPhysicalCandidate,
) -> Result<(), LegacyAdapterError> {
    let assignments = candidate
        .instances
        .assignments
        .iter()
        .filter_map(|assignment| match assignment.sink {
            PhysicalSink::DeclaredOutput(port) => Some((port, assignment.driver.clone())),
            PhysicalSink::InstanceInput { .. } => None,
        })
        .collect::<Vec<_>>();
    let mut next_route = match candidate.routes.keys().next_back() {
        Some(route) => route
            .0
            .checked_add(1)
            .ok_or(LegacyAdapterError::IdentityOverflow)?,
        None => 0,
    };

    for (port, driver) in assignments {
        let endpoint = PhysicalEndpointId::DeclaredOutput(port);
        if candidate.pin_contracts.contains_key(&endpoint) {
            continue;
        }
        let source =
            endpoint_for_driver(&driver).ok_or_else(|| LegacyAdapterError::MalformedRoute {
                route: format!("declared output {port:?} has a non-singular driver"),
            })?;
        let output = candidate
            .observations
            .get(&ObservationId::DeclaredOutput(port))
            .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                route: format!("declared output {port:?} has no observation"),
            })?;
        let terminal_at = Anchor {
            y: output.site.at.y + 1,
            ..output.site.at
        };
        let terminal_state = compile::dust();
        let existing_route = candidate
            .routes
            .values()
            .find(|route| route.source == source)
            .map(|route| route.id);
        let route = if let Some(route) = existing_route {
            route
        } else {
            let route = allocate_route(&mut next_route)?;
            candidate.routes.insert(
                route,
                RealisedRouteTree {
                    id: route,
                    source,
                    cells: Vec::new(),
                    floors: Vec::new(),
                    branches: Vec::new(),
                },
            );
            route
        };
        let tree = candidate
            .routes
            .get_mut(&route)
            .expect("an existing or newly inserted output route must exist");
        let ordinal =
            u16::try_from(tree.branches.len()).map_err(|_| LegacyAdapterError::IdentityOverflow)?;
        let sink = RoutedSinkId { route, ordinal };
        push_block_once(&mut tree.cells, terminal_at, terminal_state.clone())?;
        tree.branches.push(RealisedRouteBranch {
            sink,
            target: RouteTarget::DeclaredOutput(port),
            root: terminal_at,
            path: vec![terminal_at],
            terminal: TerminalRecord {
                sink,
                at: terminal_at,
                state: terminal_state,
                kind: planner::RouteTerminalKind::DirectedDustIntoSupport,
                repeaters: 0,
                delayed_owner: None,
            },
        });
    }
    Ok(())
}

fn source_stub_blocks(world: &World, pin: Anchor) -> Vec<PlacedBlock> {
    let mut blocks = Vec::new();
    let state = world.get(pin.x, pin.y, pin.z);
    if state.kind != BlockKind::Air {
        blocks.push(PlacedBlock {
            at: pin,
            state: state.clone(),
        });
    }
    if pin.y > 0 {
        let floor_at = Anchor {
            y: pin.y - 1,
            ..pin
        };
        let floor = world.get(floor_at.x, floor_at.y, floor_at.z);
        if floor.kind != BlockKind::Air {
            blocks.push(PlacedBlock {
                at: floor_at,
                state: floor.clone(),
            });
        }
    }
    blocks
}

fn materialise_source_stubs(
    candidate: &mut ExpandedPhysicalCandidate,
    stubs: Vec<(PhysicalEndpointId, Vec<PlacedBlock>)>,
) -> Result<(), LegacyAdapterError> {
    let mut next_route = candidate.routes.keys().next_back().map_or(Ok(0), |route| {
        route
            .0
            .checked_add(1)
            .ok_or(LegacyAdapterError::IdentityOverflow)
    })?;
    for (source, blocks) in stubs {
        let existing_route = candidate
            .routes
            .values()
            .find(|route| route.source == source)
            .map(|route| route.id);
        let route = if let Some(route) = existing_route {
            route
        } else {
            let route = allocate_route(&mut next_route)?;
            candidate.routes.insert(
                route,
                RealisedRouteTree {
                    id: route,
                    source,
                    cells: Vec::new(),
                    floors: Vec::new(),
                    branches: Vec::new(),
                },
            );
            route
        };
        let tree = candidate
            .routes
            .get_mut(&route)
            .expect("an existing or newly inserted source route must exist");
        for block in blocks {
            if is_route_conductor(&block.state) {
                push_block_once(&mut tree.cells, block.at, block.state)?;
            } else if let Some(existing) = tree
                .floors
                .iter_mut()
                .find(|existing| existing.at == block.at)
            {
                // The source stub is sampled from the final world. In
                // particular, a declared-output lamp replaces the temporary
                // stone floor the route planner recorded beneath this pin.
                existing.state = block.state;
            } else {
                tree.floors.push(block);
            }
        }
    }
    Ok(())
}

fn allocate_route(next_route: &mut u32) -> Result<RouteId, LegacyAdapterError> {
    let route = RouteId(*next_route);
    *next_route = next_route
        .checked_add(1)
        .ok_or(LegacyAdapterError::IdentityOverflow)?;
    Ok(route)
}

#[derive(Clone, Copy)]
enum AdapterPhysicalOwner {
    Primitive,
    Route(RouteId),
}

fn refresh_route_repeater_counts(candidate: &mut ExpandedPhysicalCandidate) {
    let mut ledger = BTreeMap::new();
    for placement in candidate.placements.values() {
        for block in &placement.blocks {
            ledger.insert(
                block.at,
                (block.state.kind, AdapterPhysicalOwner::Primitive),
            );
        }
    }
    for (&route, tree) in &candidate.routes {
        for block in tree.cells.iter().chain(&tree.floors) {
            ledger.insert(
                block.at,
                (block.state.kind, AdapterPhysicalOwner::Route(route)),
            );
        }
    }

    for (&route, tree) in &mut candidate.routes {
        for branch in &mut tree.branches {
            branch.terminal.repeaters = branch
                .path
                .iter()
                .filter(|&&at| {
                    let Some((kind, owner)) = ledger.get(&at) else {
                        return false;
                    };
                    if *kind != BlockKind::Repeater {
                        return false;
                    }
                    let belongs_to_path = matches!(owner, AdapterPhysicalOwner::Route(actual) if *actual == route)
                        || matches!(owner, AdapterPhysicalOwner::Primitive);
                    let absorbed_target = at == branch.terminal.at
                        && matches!(
                            branch.terminal.delayed_owner,
                            Some(DelayedOwner::Primitive(_))
                        );
                    let delivery = at == branch.terminal.at
                        && branch.terminal.kind
                            == planner::RouteTerminalKind::OutputTerminalRepeater;
                    belongs_to_path && !absorbed_target && !delivery
                })
                .count() as u64;
        }
    }
}

fn normalise_physical_ownership(
    candidate: &mut ExpandedPhysicalCandidate,
) -> Result<(), LegacyAdapterError> {
    // The legacy recorder keeps the first net that touched a coordinate in
    // `route_anchors`, while the enforcing emitter (and its reservation)
    // keeps the last writer. Reconstruct that final ownership from the
    // ordered branch paths before deduplicating the typed arenas.
    let mut final_route_owner = BTreeMap::new();
    for (&route, tree) in &candidate.routes {
        for branch in &tree.branches {
            for &at in &branch.path {
                final_route_owner.insert(at, route);
            }
        }
    }
    let mut claimed = BTreeMap::<Anchor, BlockState>::new();
    for block in candidate
        .placements
        .values()
        .flat_map(|placement| placement.blocks.iter())
        .chain(
            candidate
                .boundaries
                .values()
                .flat_map(|placement| placement.blocks.iter()),
        )
        .chain(
            candidate
                .junctions
                .values()
                .flat_map(|junction| junction.cells.iter()),
        )
    {
        if let Some(existing) = claimed.insert(block.at, block.state.clone()) {
            if existing != block.state {
                return Err(LegacyAdapterError::MalformedRoute {
                    route: format!("conflicting owner at {:?}", block.at),
                });
            }
        }
    }

    for route in candidate.routes.values_mut() {
        retain_unclaimed(
            &mut route.cells,
            &mut claimed,
            route.id,
            Some(&final_route_owner),
        )?;
        route
            .floors
            .retain(|block| !final_route_owner.contains_key(&block.at));
        retain_unclaimed(&mut route.floors, &mut claimed, route.id, None)?;
    }
    Ok(())
}

fn is_route_conductor(state: &BlockState) -> bool {
    matches!(state.kind, BlockKind::RedstoneWire | BlockKind::Repeater)
}

fn push_block_once(
    blocks: &mut Vec<PlacedBlock>,
    at: Anchor,
    state: BlockState,
) -> Result<(), LegacyAdapterError> {
    if let Some(existing) = blocks.iter().find(|block| block.at == at) {
        if existing.state != state {
            return Err(LegacyAdapterError::MalformedRoute {
                route: format!("conflicting reconstructed block at {at:?}"),
            });
        }
    } else {
        blocks.push(PlacedBlock { at, state });
    }
    Ok(())
}

fn retain_unclaimed(
    blocks: &mut Vec<PlacedBlock>,
    claimed: &mut BTreeMap<Anchor, BlockState>,
    route: RouteId,
    final_route_owner: Option<&BTreeMap<Anchor, RouteId>>,
) -> Result<(), LegacyAdapterError> {
    let mut retained = Vec::with_capacity(blocks.len());
    for block in blocks.drain(..) {
        if final_route_owner
            .and_then(|owners| owners.get(&block.at))
            .is_some_and(|owner| *owner != route)
        {
            continue;
        }
        if let Some(existing) = claimed.get(&block.at) {
            if existing != &block.state {
                return Err(LegacyAdapterError::MalformedRoute {
                    route: format!("{:?} conflicts at {:?}", route, block.at),
                });
            }
            continue;
        }
        claimed.insert(block.at, block.state.clone());
        retained.push(block);
    }
    *blocks = retained;
    Ok(())
}

fn materialise_merge_primitives(
    candidate: &mut ExpandedPhysicalCandidate,
) -> Result<(), LegacyAdapterError> {
    let instances = candidate.instances.instances.clone();
    for instance in instances {
        if !matches!(
            instance.expanded.topology.output,
            OutputSpec::Junction { .. }
        ) {
            continue;
        }
        for primitive in &instance.expanded.topology.primitives {
            let connection = instance
                .expanded
                .topology
                .connections
                .iter()
                .find(|connection| connection.target == ConnectionTarget::Primitive(primitive.id))
                .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                    route: format!("merge primitive {:?}", primitive.id),
                })?;
            let binding = candidate
                .connections
                .get(&connection.id)
                .cloned()
                .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                    route: format!("merge connection {:?}", connection.id),
                })?;
            let route = candidate.routes.get_mut(&binding.route).ok_or_else(|| {
                LegacyAdapterError::MalformedRoute {
                    route: format!("{:?}", binding.route),
                }
            })?;
            let branch = route
                .branches
                .iter_mut()
                .find(|branch| branch.sink == binding.sink)
                .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                    route: format!("{:?}", binding.route),
                })?;
            let at = branch.terminal.at;
            let cell = route
                .cells
                .iter()
                .position(|block| block.at == at)
                .map(|index| route.cells.remove(index))
                .ok_or_else(|| LegacyAdapterError::MissingTerminalState {
                    route: format!("{:?}", binding.route),
                    at,
                })?;
            let floor_at = Anchor { y: at.y - 1, ..at };
            let mut blocks = vec![cell.clone()];
            if let Some(index) = route.floors.iter().position(|block| block.at == floor_at) {
                blocks.push(route.floors.remove(index));
            }
            let facing = cell
                .state
                .facing
                .map(cell_facing)
                .transpose()?
                .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                    route: format!("merge primitive {:?}", primitive.id),
                })?;
            branch.terminal.delayed_owner = Some(DelayedOwner::Primitive(primitive.id));
            // Legacy emission counts the socket repeater in the branch;
            // planner-built candidates already price it as the topology
            // primitive it becomes here. The typed route excludes that
            // absorbed component in either representation.
            branch.terminal.repeaters = branch.terminal.repeaters.saturating_sub(1);
            candidate.placements.insert(
                primitive.id,
                PrimitivePlacement {
                    id: primitive.id,
                    variant: u16::from(facing.index()),
                    facing,
                    anchor: at,
                    delayed: Some(DelayedComponent {
                        at,
                        owner: DelayedOwner::Primitive(primitive.id),
                    }),
                    blocks,
                },
            );
            candidate.observations.insert(
                ObservationId::PrimitiveOutput(primitive.id),
                VerifiedObservation {
                    site: ObservationSite {
                        id: ObservationId::PrimitiveOutput(primitive.id),
                        at,
                        logical_owner: Some(instance.id),
                        display_label: None,
                    },
                    state: cell.state,
                },
            );
        }
    }
    Ok(())
}

fn cell_facing(
    facing: crate::redstone::world::block::Facing,
) -> Result<geometry::CellFacing, LegacyAdapterError> {
    match facing {
        crate::redstone::world::block::Facing::North => Ok(geometry::CellFacing::NORTH),
        crate::redstone::world::block::Facing::South => Ok(geometry::CellFacing::SOUTH),
        crate::redstone::world::block::Facing::East => Ok(geometry::CellFacing::EAST),
        crate::redstone::world::block::Facing::West => Ok(geometry::CellFacing::WEST),
        crate::redstone::world::block::Facing::Up | crate::redstone::world::block::Facing::Down => {
            Err(LegacyAdapterError::MalformedRoute {
                route: "vertical repeater".to_string(),
            })
        }
    }
}

fn materialise_gate(
    gate: &crate::compile::Gate,
    anchor: Anchor,
    facing: geometry::CellFacing,
    size: (i32, i32, i32),
) -> (Vec<PlacedBlock>, Anchor, Anchor) {
    let mut world = World::new(size.0, size.1, size.2);
    let origin = (anchor.x, anchor.y, anchor.z);
    let cell = if gate.is_merge() {
        compile::place_merge_gate(&mut world, origin, gate.inputs.len(), facing)
    } else {
        compile::place_nor_gate(&mut world, origin, gate.inputs.len(), facing)
    };
    let output = Position::new(
        anchor.x + cell.output_offset.0,
        anchor.y + cell.output_offset.1,
        anchor.z + cell.output_offset.2,
    );
    let pin = output.offset(geometry::output_direction(facing));
    compile::ensure_floor(&mut world, pin);
    world.set(pin.x, pin.y, pin.z, compile::dust());
    let observed = if gate.is_merge() {
        anchor
    } else {
        Anchor {
            x: output.x,
            y: output.y,
            z: output.z,
        }
    };
    (
        collect_blocks(&world),
        observed,
        Anchor {
            x: pin.x,
            y: pin.y,
            z: pin.z,
        },
    )
}

fn physical_primitive_blocks(
    primitive: crate::compile::topology::Primitive,
    anchor: Anchor,
    facing: geometry::CellFacing,
    realised: &[PlacedBlock],
) -> Result<Vec<PlacedBlock>, LegacyAdapterError> {
    let variant = physical::variants(primitive)
        .get(usize::from(facing.index()))
        .ok_or_else(|| LegacyAdapterError::MalformedRoute {
            route: format!("primitive {primitive:?} has no variant for facing {facing:?}"),
        })?;
    variant
        .blocks
        .iter()
        .map(|expected| {
            let at = Anchor {
                x: anchor.x + expected.position.x,
                y: anchor.y + expected.position.y,
                z: anchor.z + expected.position.z,
            };
            realised
                .iter()
                .find(|block| {
                    block.at == at
                        && block.state.kind == expected.kind
                        && block.state.facing == expected.facing
                        && block.state.face == expected.face
                })
                .cloned()
                .ok_or_else(|| LegacyAdapterError::MalformedRoute {
                    route: format!(
                        "primitive {primitive:?} is missing its {facing:?} block at {at:?}"
                    ),
                })
        })
        .collect()
}

fn materialise_input(
    anchor: Anchor,
    facing: geometry::CellFacing,
    size: (i32, i32, i32),
) -> Vec<PlacedBlock> {
    let mut world = World::new(size.0, size.1, size.2);
    compile::place_primary_input(
        &mut world,
        Position::new(anchor.x, anchor.y, anchor.z),
        facing,
    );
    collect_blocks(&world)
}

fn materialise_input_terminal(
    anchor: Anchor,
    toward: crate::redstone::world::block::Facing,
    size: (i32, i32, i32),
) -> Vec<PlacedBlock> {
    let mut world = World::new(size.0, size.1, size.2);
    compile::place_input_terminal(
        &mut world,
        Position::new(anchor.x, anchor.y, anchor.z),
        toward,
    );
    collect_blocks(&world)
}

fn collect_blocks(world: &World) -> Vec<PlacedBlock> {
    let mut blocks = Vec::new();
    for flat in 0..world.cells().len() {
        let (x, y, z) = world.decode(flat);
        let state = world.get(x, y, z);
        if state.kind != BlockKind::Air {
            blocks.push(PlacedBlock {
                at: Anchor { x, y, z },
                state: state.clone(),
            });
        }
    }
    blocks
}

fn source_endpoint(
    owner: &str,
    route: RouteId,
    inputs: &BTreeMap<&str, PortId>,
    gates: &BTreeMap<&str, usize>,
    candidate: &ExpandedPhysicalCandidate,
) -> Result<PhysicalEndpointId, LegacyAdapterError> {
    if let Some(&port) = inputs.get(owner) {
        return Ok(PhysicalEndpointId::PrimaryInput(port));
    }
    let &gate = gates
        .get(owner)
        .ok_or_else(|| LegacyAdapterError::UnknownRouteSource {
            route: format!("{:?}", route),
            signal: owner.to_string(),
        })?;
    let instance = &candidate.instances.instances[gate];
    Ok(match &instance.expanded.topology.output {
        OutputSpec::Primitive(primitive) => PhysicalEndpointId::PrimitiveOutput(*primitive),
        OutputSpec::Junction { .. } => PhysicalEndpointId::Junction(instance.id),
    })
}

fn contributor_endpoint(
    contributor: &crate::compile::fragment_synth::topology::ContributorSpec,
) -> PhysicalEndpointId {
    match contributor {
        crate::compile::fragment_synth::topology::ContributorSpec::Landing(connection) => {
            PhysicalEndpointId::Landing(*connection)
        }
        crate::compile::fragment_synth::topology::ContributorSpec::Primitive(primitive) => {
            PhysicalEndpointId::PrimitiveOutput(*primitive)
        }
    }
}

fn observation(
    id: ObservationId,
    block: PlacedBlock,
    logical_owner: Option<InstanceId>,
    display_label: Option<String>,
) -> VerifiedObservation {
    VerifiedObservation {
        site: ObservationSite {
            id,
            at: block.at,
            logical_owner,
            display_label,
        },
        state: block.state,
    }
}
