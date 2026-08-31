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
use crate::compile::planner::{self, PortPlacements};
use crate::compile::topology::Library;
use crate::compile::{self, CompiledCircuit, Netlist};
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
    pub fn adapt(
        netlist: &Netlist,
        compiled: &CompiledCircuit,
    ) -> Result<AdaptedLegacyCandidate, LegacyAdapterError> {
        let seed = planner::seed_from_legacy(netlist, compiled)?;
        let instances = InstanceGraph::one_to_one(netlist, &Library::default_library())?;
        let mut candidate = ExpandedPhysicalCandidate::empty(instances, PortPlacements::default());
        let size = compiled.world.size();

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

        for (index, gate) in netlist.gates.iter().enumerate() {
            let instance = &candidate.instances.instances[index];
            let anchor = seed.anchors()[index];
            let facing = seed.facing_of(index);
            let (mut blocks, output_at, output_pin) = materialise_gate(gate, anchor, facing, size);

            if let Some(output_port) = netlist.outputs.iter().position(|name| name == &gate.output)
            {
                let lamp_at = Anchor {
                    y: output_pin.y - 1,
                    ..output_pin
                };
                blocks.retain(|block| block.at != lamp_at);
                let endpoint = PhysicalEndpointId::DeclaredOutput(PortId(
                    u32::try_from(output_port).map_err(|_| LegacyAdapterError::IdentityOverflow)?,
                ));
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
                    ObservationId::DeclaredOutput(match endpoint {
                        PhysicalEndpointId::DeclaredOutput(port) => port,
                        _ => unreachable!(),
                    }),
                    observation(
                        ObservationId::DeclaredOutput(match endpoint {
                            PhysicalEndpointId::DeclaredOutput(port) => port,
                            _ => unreachable!(),
                        }),
                        lamp,
                        Some(instance.id),
                        Some(gate.output.clone()),
                    ),
                );
            }

            match &instance.expanded.topology.output {
                OutputSpec::Primitive(primitive) => {
                    candidate.placements.insert(
                        *primitive,
                        PrimitivePlacement {
                            id: *primitive,
                            variant: u16::try_from(seed.selected_entry(index))
                                .map_err(|_| LegacyAdapterError::IdentityOverflow)?,
                            facing,
                            anchor,
                            delayed: blocks
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
                            blocks,
                        },
                    );
                    let state = compiled
                        .world
                        .get(output_at.x, output_at.y, output_at.z)
                        .clone();
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
                    let state = compiled.world.get(anchor.x, anchor.y, anchor.z).clone();
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
            let facing = seed.facing_of(netlist.gates.len() + input_index);
            let blocks = materialise_input(anchor, facing, size);
            let endpoint = PhysicalEndpointId::PrimaryInput(port);
            let state = compiled.world.get(anchor.x, anchor.y, anchor.z).clone();
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed: None,
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
                    route: legacy.id().to_string(),
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
            for ((&at, state), floor) in legacy
                .anchors()
                .iter()
                .zip(legacy.realisation())
                .zip(legacy.floors())
            {
                cells.push(PlacedBlock {
                    at,
                    state: state.clone(),
                });
                let floor_at = Anchor { y: at.y - 1, ..at };
                if floor_at.x >= 0
                    && floor_at.y >= 0
                    && floor_at.z >= 0
                    && floor_at.x < size.0
                    && floor_at.y < size.1
                    && floor_at.z < size.2
                {
                    floors.push(PlacedBlock {
                        at: floor_at,
                        state: floor.clone(),
                    });
                }
            }

            let mut branches = Vec::new();
            for (ordinal, terminal) in legacy.terminals().iter().enumerate() {
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
                    root: *legacy.branch_paths()[ordinal].first().ok_or_else(|| {
                        LegacyAdapterError::MalformedRoute {
                            route: legacy.id().to_string(),
                        }
                    })?,
                    path: legacy.branch_paths()[ordinal].clone(),
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
        normalise_physical_ownership(&mut candidate)?;

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
        let terminal_state = candidate_block_state(candidate, terminal_at).ok_or_else(|| {
            LegacyAdapterError::MissingTerminalState {
                route: format!("declared output {port:?}"),
                at: terminal_at,
            }
        })?;
        let route = RouteId(next_route);
        next_route = next_route
            .checked_add(1)
            .ok_or(LegacyAdapterError::IdentityOverflow)?;
        let sink = RoutedSinkId { route, ordinal: 0 };
        candidate.routes.insert(
            route,
            RealisedRouteTree {
                id: route,
                source,
                cells: Vec::new(),
                floors: Vec::new(),
                branches: vec![RealisedRouteBranch {
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
                }],
            },
        );
    }
    Ok(())
}

fn candidate_block_state(candidate: &ExpandedPhysicalCandidate, at: Anchor) -> Option<BlockState> {
    candidate
        .placements
        .values()
        .flat_map(|placement| placement.blocks.iter())
        .chain(
            candidate
                .boundaries
                .values()
                .flat_map(|boundary| boundary.blocks.iter()),
        )
        .chain(
            candidate
                .junctions
                .values()
                .flat_map(|junction| junction.cells.iter()),
        )
        .find(|block| block.at == at)
        .map(|block| block.state.clone())
}

fn normalise_physical_ownership(
    candidate: &mut ExpandedPhysicalCandidate,
) -> Result<(), LegacyAdapterError> {
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
        retain_unclaimed(&mut route.cells, &mut claimed, route.id)?;
        retain_unclaimed(&mut route.floors, &mut claimed, route.id)?;
    }
    Ok(())
}

fn retain_unclaimed(
    blocks: &mut Vec<PlacedBlock>,
    claimed: &mut BTreeMap<Anchor, BlockState>,
    route: RouteId,
) -> Result<(), LegacyAdapterError> {
    let mut retained = Vec::with_capacity(blocks.len());
    for block in blocks.drain(..) {
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
            branch.terminal.repeaters =
                branch.terminal.repeaters.checked_sub(1).ok_or_else(|| {
                    LegacyAdapterError::MalformedRoute {
                        route: format!(
                            "merge primitive {:?} owns an uncharged terminal repeater",
                            primitive.id
                        ),
                    }
                })?;
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
