#![allow(dead_code)] // Task 9 is the first production caller of this Task-8 seam.

//! Independent deterministic sparse-seed construction.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::emission::EmissionError;
use crate::compile::fragment_synth::candidate::{
    endpoint_for_driver, BoundaryPlacement, CandidateError, ConnectionBinding,
    ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedJunction,
    VerifiedObservation,
};
use crate::compile::fragment_synth::certification::{
    CandidateCertificationError, CertifiedCandidate, ExpandedCandidateCertifier,
};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::identity::{
    ConnectionId, InstanceId, ObservationId, ObservationSite, PhysicalEndpointId, PortId,
    PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::instance_graph::{
    InstanceGraph, PhysicalDriver, PhysicalSink, SynthesisError,
};
use crate::compile::fragment_synth::realise::{ExpandedAdapterError, ExpandedCandidateAdapter};
use crate::compile::fragment_synth::services::{SeedEmitter, SeedVerifier};
use crate::compile::fragment_synth::topology::{ConnectionTarget, ContributorSpec, OutputSpec};
use crate::compile::geometry::{self, Anchor, CellFacing};
use crate::compile::physical::{self, PortKind};
use crate::compile::planner::{PortPlacements, PortRole};
use crate::compile::routing::{
    DelayedComponent, DelayedOwner, NonEmptyRouteSinks, PhysicalReservationKind,
    PhysicalReservationOwner, PhysicalReservations, PhysicalRouter, RouteEndpoint, RouteRequest,
    RouteSink, RouterFailure, TerminalContract, TerminalRequirement,
};
use crate::compile::topology::{Library, Primitive};
use crate::compile::verification::ExpandedPhysicalError;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};

const INSTANCE_BASE_X: i32 = 40;
const INSTANCE_BASE_Z: i32 = 40;
const INSTANCE_X_PITCH: i32 = 40;
const INSTANCE_Y_PITCH: i32 = 3;
const INSTANCE_Z_PITCH: i32 = 24;
const PRIMITIVE_X_PITCH: i32 = 8;
const INPUT_STANDOFF: i32 = 10;
const OUTPUT_STANDOFF: i32 = 24;

#[derive(Clone, Copy)]
pub(crate) struct SeedInput<'a> {
    pub lowered: &'a Netlist,
    pub source_provenance: Option<&'a [usize]>,
    pub pins: Option<&'a PortPlacements>,
}

pub(crate) struct SeedServices<'a> {
    pub library: &'a Library,
    pub router: &'a dyn PhysicalRouter,
    pub emitter: &'a dyn SeedEmitter,
    pub verifier: &'a dyn SeedVerifier,
    pub certifier: &'a dyn ExpandedCandidateCertifier,
    pub search_config: &'a SearchConfig,
}

#[derive(Debug, Error)]
pub(crate) enum SeedError {
    #[error("source provenance has {actual} entries for {expected} lowered gates")]
    ProvenanceWidth { expected: usize, actual: usize },
    #[error("instance graph construction failed: {0}")]
    InstanceGraph(#[from] SynthesisError),
    #[error("candidate construction failed: {0}")]
    Candidate(#[from] CandidateError),
    #[error("invalid pinned IO: {0}")]
    InvalidPins(#[source] crate::compile::planner::PlannerError),
    #[error("physical placement at {at:?} overlaps another seed component")]
    PlacementCollision { at: Anchor },
    #[error(
        "seed placement exhausted at instance {instance:?}, primitive {primitive:?}, radius {radius}"
    )]
    PlacementExhausted {
        instance: InstanceId,
        primitive: PrimitiveId,
        radius: u32,
    },
    #[error("typed route construction failed: {0}")]
    Routing(#[from] RouterFailure),
    #[error("typed route sink set was unexpectedly empty")]
    EmptyRoute,
    #[error("expanded candidate adaptation failed: {0}")]
    Adapter(#[from] ExpandedAdapterError),
    #[error("durable emission failed: {0}")]
    Emission(#[from] EmissionError),
    #[error("durable physical verification failed: {0}")]
    Verification(#[from] ExpandedPhysicalError),
    #[error("complete candidate certification failed: {0}")]
    Certification(#[from] CandidateCertificationError),
    #[error("typed identity width exceeded")]
    IdentityOverflow,
    #[error("seed topology is internally incomplete: {0}")]
    Incomplete(&'static str),
}

pub(crate) struct SparseSeedBuilder;

pub(crate) fn compile_sparse_seed_with_services(
    input: SeedInput<'_>,
    services: SeedServices<'_>,
) -> Result<CertifiedCandidate, SeedError> {
    SparseSeedBuilder::build(input, services)
}

#[derive(Debug, Clone, Copy)]
struct SourceGeometry {
    route_anchor: Anchor,
    allowed_exit: Facing,
}

#[derive(Debug, Clone, Copy)]
struct TargetGeometry {
    terminal: Anchor,
    allowed_entry: Facing,
    support: Anchor,
    requirement: TerminalRequirement,
}

#[derive(Debug)]
struct PlacementSearch {
    max_radius: u32,
    max_backtracks: u64,
    backtracks_used: u64,
}

impl PlacementSearch {
    fn new(config: &SearchConfig) -> Self {
        Self {
            max_radius: config.max_seed_shell_radius,
            max_backtracks: config.max_seed_backtracks,
            backtracks_used: 0,
        }
    }

    fn reject_choice(
        &mut self,
        instance: InstanceId,
        primitive: PrimitiveId,
    ) -> Result<(), SeedError> {
        self.backtracks_used = self.backtracks_used.saturating_add(1);
        if self.backtracks_used >= self.max_backtracks {
            return Err(SeedError::PlacementExhausted {
                instance,
                primitive,
                radius: self.max_radius,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum PendingTarget {
    Connection(ConnectionId, TargetGeometry),
    DeclaredOutput(PortId, TargetGeometry),
}

impl PendingTarget {
    fn key(&self) -> (u8, u32, u16) {
        match self {
            Self::Connection(
                ConnectionId::External {
                    instance,
                    input_index,
                },
                _,
            ) => (0, instance.0, *input_index),
            Self::Connection(
                ConnectionId::Internal {
                    instance,
                    edge_index,
                },
                _,
            ) => (1, instance.0, *edge_index),
            Self::DeclaredOutput(port, _) => (2, port.0, 0),
        }
    }

    fn geometry(&self) -> TargetGeometry {
        match self {
            Self::Connection(_, geometry) | Self::DeclaredOutput(_, geometry) => *geometry,
        }
    }
}

impl SparseSeedBuilder {
    pub(crate) fn build(
        input: SeedInput<'_>,
        services: SeedServices<'_>,
    ) -> Result<CertifiedCandidate, SeedError> {
        if let Some(provenance) = input.source_provenance {
            if provenance.len() != input.lowered.gates.len() {
                return Err(SeedError::ProvenanceWidth {
                    expected: input.lowered.gates.len(),
                    actual: provenance.len(),
                });
            }
        }
        let instances = InstanceGraph::one_to_one(input.lowered, services.library)?;
        if let Some(first) = instances
            .instances
            .iter()
            .flat_map(|instance| {
                instance
                    .expanded
                    .topology
                    .primitives
                    .iter()
                    .map(move |primitive| (instance.id, primitive.id))
            })
            .next()
        {
            if services.search_config.max_seed_shell_radius == 0
                || services.search_config.max_seed_backtracks == 0
            {
                return Err(SeedError::PlacementExhausted {
                    instance: first.0,
                    primitive: first.1,
                    radius: services.search_config.max_seed_shell_radius,
                });
            }
        }

        let mut candidate =
            ExpandedPhysicalCandidate::empty(instances, input.pins.cloned().unwrap_or_default());
        candidate.bind_pin_contracts(input.lowered)?;
        crate::compile::planner::validate_port_placements(input.lowered, &candidate.pins)
            .map_err(SeedError::InvalidPins)?;

        let mut occupied = BTreeSet::new();
        let mut sources = BTreeMap::new();
        let mut targets = BTreeMap::new();
        place_boundaries(
            &mut candidate,
            input.lowered,
            &mut occupied,
            &mut sources,
            &mut targets,
        )?;
        place_instances(
            &mut candidate,
            input.lowered,
            services.search_config,
            &mut occupied,
            &mut sources,
            &mut targets,
        )?;

        let mut reservations = reservations_for_components(&candidate)?;
        reserve_route_endpoints(&mut reservations, &candidate, &sources, &targets);
        route_all(
            &mut candidate,
            services.router,
            services.search_config,
            &sources,
            &targets,
            &mut reservations,
        )?;
        candidate.validate_shape()?;
        candidate.validate_physical_ownership()?;

        let adapter = ExpandedCandidateAdapter::new(&candidate)?;
        let size = adapter.deterministic_world_size()?;
        let emitted = services.emitter.emit(&adapter, size)?;
        services.verifier.verify(&candidate, &emitted)?;

        let certification = CertificationConfig::from_search(services.search_config);
        services
            .certifier
            .certify(candidate, input.lowered, services.library, &certification)
            .map_err(SeedError::from)
    }
}

fn reserve_route_endpoints(
    reservations: &mut PhysicalReservations,
    candidate: &ExpandedPhysicalCandidate,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &BTreeMap<PhysicalSink, TargetGeometry>,
) {
    for (&endpoint, source) in sources {
        if reservations.get(&source.route_anchor).is_none() {
            reservations.reserve(
                source.route_anchor,
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    for (&sink, target) in targets {
        if reservations.get(&target.terminal).is_none() {
            let endpoint = match sink {
                PhysicalSink::InstanceInput {
                    instance,
                    input_index,
                } => PhysicalEndpointId::Landing(ConnectionId::External {
                    instance,
                    input_index,
                }),
                PhysicalSink::DeclaredOutput(port) => PhysicalEndpointId::DeclaredOutput(port),
            };
            reservations.reserve(
                target.terminal,
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    let endpoints = sources
        .values()
        .map(|source| source.route_anchor)
        .chain(targets.values().map(|target| target.terminal))
        .collect::<BTreeSet<_>>();
    for junction in candidate.junctions.values() {
        for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let neighbour = step(junction.at, direction);
            if !endpoints.contains(&neighbour) && reservations.get(&neighbour).is_none() {
                reservations.reserve(
                    neighbour,
                    PhysicalReservationOwner::KeepOut(junction.id.0),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }
}

fn place_boundaries(
    candidate: &mut ExpandedPhysicalCandidate,
    netlist: &Netlist,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    for (index, name) in netlist.inputs.iter().enumerate() {
        let port = PortId(u32::try_from(index).map_err(|_| SeedError::IdentityOverflow)?);
        let endpoint = PhysicalEndpointId::PrimaryInput(port);
        let pin = candidate.pin_contracts.get(&endpoint).copied();
        let (blocks, delayed, observation_at, route_anchor, allowed_exit) = match pin {
            Some(pin) => {
                let handover = pin.handover(PortRole::Input);
                let net = pin.net_cell(PortRole::Input);
                let blocks = vec![
                    PlacedBlock {
                        at: Anchor {
                            y: handover.y - 1,
                            ..handover
                        },
                        state: compile::stone(),
                    },
                    PlacedBlock {
                        at: handover,
                        state: compile::repeater(pin.toward),
                    },
                ];
                (
                    blocks,
                    Some(DelayedComponent {
                        at: handover,
                        owner: DelayedOwner::InputBinding(port),
                    }),
                    pin.at,
                    net,
                    pin.toward,
                )
            }
            None => {
                let home = automatic_input_home(candidate, port, index)?;
                let root = step(home, Facing::East);
                (
                    vec![
                        PlacedBlock {
                            at: Anchor {
                                y: home.y - 1,
                                ..home
                            },
                            state: compile::stone(),
                        },
                        PlacedBlock {
                            at: home,
                            state: compile::lever(false),
                        },
                    ],
                    None,
                    home,
                    root,
                    Facing::East,
                )
            }
        };
        claim_blocks(occupied, &blocks)?;
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed,
                blocks: blocks.clone(),
            },
        );
        candidate.observations.insert(
            ObservationId::PrimaryInput(port),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::PrimaryInput(port),
                    at: observation_at,
                    logical_owner: None,
                    display_label: Some(name.clone()),
                },
                state: block_state_at(&blocks, observation_at),
            },
        );
        sources.insert(
            endpoint,
            SourceGeometry {
                route_anchor,
                allowed_exit,
            },
        );
    }

    for (index, name) in netlist.outputs.iter().enumerate() {
        let port = PortId(u32::try_from(index).map_err(|_| SeedError::IdentityOverflow)?);
        let endpoint = PhysicalEndpointId::DeclaredOutput(port);
        let pin = candidate.pin_contracts.get(&endpoint).copied();
        let (blocks, observation_at, geometry) = match pin {
            Some(pin) => {
                let terminal = pin.handover(PortRole::Output);
                (
                    Vec::new(),
                    pin.at,
                    TargetGeometry {
                        terminal,
                        allowed_entry: pin.toward.opposite(),
                        support: pin.at,
                        requirement: TerminalRequirement::Exact(
                            crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                        ),
                    },
                )
            }
            None => {
                let lamp = automatic_output_home(candidate, port, index)?;
                let terminal = step(lamp, Facing::West);
                let blocks = vec![PlacedBlock {
                    at: lamp,
                    state: compile::lamp(),
                }];
                (
                    blocks,
                    lamp,
                    TargetGeometry {
                        terminal,
                        allowed_entry: Facing::West,
                        support: lamp,
                        requirement: TerminalRequirement::Exact(
                            crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                        ),
                    },
                )
            }
        };
        claim_blocks(occupied, &blocks)?;
        candidate.boundaries.insert(
            endpoint,
            BoundaryPlacement {
                endpoint,
                delayed: None,
                blocks: blocks.clone(),
            },
        );
        candidate.observations.insert(
            ObservationId::DeclaredOutput(port),
            VerifiedObservation {
                site: ObservationSite {
                    id: ObservationId::DeclaredOutput(port),
                    at: observation_at,
                    logical_owner: candidate
                        .instances
                        .assignments
                        .iter()
                        .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
                        .and_then(|assignment| match &assignment.driver {
                            PhysicalDriver::PrimaryInput(_) => None,
                            PhysicalDriver::Instance(driver) => Some(match driver {
                                crate::compile::fragment_synth::instance_graph::InstanceDriver::Primitive { logical_owner, .. }
                                | crate::compile::fragment_synth::instance_graph::InstanceDriver::Junction { logical_owner, .. } => *logical_owner,
                            }),
                        }),
                    display_label: Some(name.clone()),
                },
                state: block_state_at(&blocks, observation_at),
            },
        );
        targets.insert(PhysicalSink::DeclaredOutput(port), geometry);
    }
    Ok(())
}

fn automatic_input_home(
    candidate: &ExpandedPhysicalCandidate,
    port: PortId,
    port_index: usize,
) -> Result<Anchor, SeedError> {
    let order = topological_instance_order(&candidate.instances);
    let rank = order
        .iter()
        .enumerate()
        .map(|(index, &instance)| (instance, index))
        .collect::<BTreeMap<_, _>>();
    let target_rank = candidate
        .instances
        .assignments
        .iter()
        .filter_map(|assignment| match (&assignment.driver, assignment.sink) {
            (
                PhysicalDriver::PrimaryInput(driver),
                PhysicalSink::InstanceInput { instance, .. },
            ) if *driver == port => rank.get(&instance).copied(),
            _ => None,
        })
        .min()
        .unwrap_or(0);
    let target_rank_i32 = i32::try_from(target_rank).map_err(|_| SeedError::IdentityOverflow)?;
    let pin_extent = candidate
        .pins
        .iter()
        .map(|(_, pin)| pin.at.x)
        .max()
        .unwrap_or(0)
        .max(0);
    Ok(Anchor {
        x: pin_extent
            .saturating_add(INSTANCE_BASE_X - INPUT_STANDOFF)
            .saturating_add(target_rank_i32 * INSTANCE_X_PITCH),
        y: instance_layer_y(candidate, target_rank)?,
        z: INSTANCE_BASE_Z
            + target_rank_i32 * INSTANCE_Z_PITCH
            + i32::try_from(port_index % 3).map_err(|_| SeedError::IdentityOverflow)? * 3,
    })
}

fn automatic_output_home(
    candidate: &ExpandedPhysicalCandidate,
    port: PortId,
    port_index: usize,
) -> Result<Anchor, SeedError> {
    let order = topological_instance_order(&candidate.instances);
    let rank = order
        .iter()
        .enumerate()
        .map(|(index, &instance)| (instance, index))
        .collect::<BTreeMap<_, _>>();
    let assignment = candidate
        .instances
        .assignments
        .iter()
        .find(|assignment| assignment.sink == PhysicalSink::DeclaredOutput(port))
        .ok_or(SeedError::Incomplete("automatic output assignment"))?;
    let driver_rank = match &assignment.driver {
        PhysicalDriver::PrimaryInput(_) => 0,
        PhysicalDriver::Instance(driver) => {
            let owner = match driver {
                crate::compile::fragment_synth::instance_graph::InstanceDriver::Primitive {
                    logical_owner,
                    ..
                }
                | crate::compile::fragment_synth::instance_graph::InstanceDriver::Junction {
                    logical_owner,
                    ..
                } => *logical_owner,
            };
            rank.get(&owner).copied().unwrap_or(0)
        }
    };
    let driver_rank = i32::try_from(driver_rank).map_err(|_| SeedError::IdentityOverflow)?;
    let pin_extent = candidate
        .pins
        .iter()
        .map(|(_, pin)| pin.at.x)
        .max()
        .unwrap_or(0)
        .max(0);
    Ok(Anchor {
        x: pin_extent
            .saturating_add(INSTANCE_BASE_X)
            .saturating_add(driver_rank * INSTANCE_X_PITCH)
            .saturating_add(OUTPUT_STANDOFF),
        y: instance_layer_y(
            candidate,
            usize::try_from(driver_rank).map_err(|_| SeedError::IdentityOverflow)?,
        )?,
        z: INSTANCE_BASE_Z
            + driver_rank * INSTANCE_Z_PITCH
            + 8
            + i32::try_from(port_index).map_err(|_| SeedError::IdentityOverflow)? * 4,
    })
}

fn place_instances(
    candidate: &mut ExpandedPhysicalCandidate,
    netlist: &Netlist,
    search_config: &SearchConfig,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let order = topological_instance_order(&candidate.instances);
    let mut placement_search = PlacementSearch::new(search_config);
    let pin_extent = candidate
        .pins
        .iter()
        .map(|(_, pin)| pin.at.x)
        .max()
        .unwrap_or(0)
        .max(0);

    for (ordinal, instance_id) in order.into_iter().enumerate() {
        let instance = candidate
            .instances
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .ok_or(SeedError::Incomplete("topological instance"))?
            .clone();
        let ordinal = i32::try_from(ordinal).map_err(|_| SeedError::IdentityOverflow)?;
        let base = Anchor {
            x: pin_extent
                .saturating_add(INSTANCE_BASE_X)
                .saturating_add(ordinal * INSTANCE_X_PITCH),
            y: instance_layer_y(
                candidate,
                usize::try_from(ordinal).map_err(|_| SeedError::IdentityOverflow)?,
            )?,
            z: INSTANCE_BASE_Z + ordinal * INSTANCE_Z_PITCH,
        };
        let gate = &netlist.gates
            [usize::try_from(instance.logical_gate.0).map_err(|_| SeedError::IdentityOverflow)?];

        match &instance.expanded.topology.output {
            OutputSpec::Junction { contributors, .. } => {
                place_junction_instance(
                    candidate,
                    &instance,
                    gate,
                    contributors,
                    base,
                    occupied,
                    sources,
                    targets,
                )?;
            }
            OutputSpec::Primitive(output) => {
                for specification in &instance.expanded.topology.primitives {
                    let node = i32::from(specification.id.node.0);
                    let anchor = Anchor {
                        x: base.x + node * PRIMITIVE_X_PITCH,
                        ..base
                    };
                    place_primitive_searched(
                        candidate,
                        specification.id,
                        specification.primitive,
                        CellFacing::EAST,
                        anchor,
                        Some(instance.id),
                        instance.id,
                        &mut placement_search,
                        occupied,
                        sources,
                    )?;
                }
                assign_primitive_targets(candidate, &instance, targets)?;
                let observation = candidate
                    .observations
                    .get(&ObservationId::PrimitiveOutput(*output))
                    .cloned()
                    .ok_or(SeedError::Incomplete("instance output primitive"))?;
                candidate.observations.insert(
                    ObservationId::InstanceOutput(instance.id),
                    VerifiedObservation {
                        site: ObservationSite {
                            id: ObservationId::InstanceOutput(instance.id),
                            at: observation.site.at,
                            logical_owner: Some(instance.id),
                            display_label: Some(gate.output.clone()),
                        },
                        state: observation.state,
                    },
                );
            }
        }
    }
    Ok(())
}

fn instance_layer_y(candidate: &ExpandedPhysicalCandidate, rank: usize) -> Result<i32, SeedError> {
    let count = candidate.instances.instances.len();
    let pinned_output_y = candidate
        .pin_contracts
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)).then_some(pin.at.y)
        })
        .min();
    let pinned_input_y = candidate
        .pin_contracts
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)).then_some(pin.at.y)
        })
        .min();
    let rank = i32::try_from(rank).map_err(|_| SeedError::IdentityOverflow)?;
    Ok(match (pinned_input_y, pinned_output_y) {
        (Some(input_y), Some(output_y)) => {
            let denominator = i32::try_from(count.saturating_sub(1).max(1))
                .map_err(|_| SeedError::IdentityOverflow)?;
            input_y
                .saturating_add(output_y.saturating_sub(input_y).saturating_mul(rank) / denominator)
        }
        (_, Some(output_y)) => {
            let reverse = i32::try_from(count.saturating_sub(1))
                .map_err(|_| SeedError::IdentityOverflow)?
                .saturating_sub(rank);
            output_y.saturating_add(reverse * INSTANCE_Y_PITCH)
        }
        (Some(input_y), None) => input_y.saturating_add(rank * INSTANCE_Y_PITCH),
        (None, None) => 4 + rank * INSTANCE_Y_PITCH,
    })
}

#[allow(clippy::too_many_arguments)]
fn place_junction_instance(
    candidate: &mut ExpandedPhysicalCandidate,
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
    gate: &crate::compile::Gate,
    contributors: &[ContributorSpec],
    at: Anchor,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let cells = vec![
        PlacedBlock {
            at: Anchor { y: at.y - 1, ..at },
            state: compile::stone(),
        },
        PlacedBlock {
            at,
            state: compile::dust(),
        },
    ];
    claim_blocks(occupied, &cells)?;
    candidate.junctions.insert(
        instance.id,
        RealisedJunction {
            id: instance.id,
            at,
            facing: CellFacing::NORTH,
            contributors: contributors.iter().map(contributor_endpoint).collect(),
            cells: cells.clone(),
        },
    );
    let junction_state = block_state_at(&cells, at);
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
                    display_label: (id == ObservationId::InstanceOutput(instance.id))
                        .then(|| gate.output.clone()),
                },
                state: junction_state.clone(),
            },
        );
    }
    sources.insert(
        PhysicalEndpointId::Junction(instance.id),
        SourceGeometry {
            route_anchor: step(at, Facing::North),
            allowed_exit: Facing::North,
        },
    );

    let directions = geometry::input_directions(CellFacing::NORTH);
    let mut primitive_slot = 0usize;
    for (input_index, contributor) in contributors.iter().enumerate() {
        let direction = directions[input_index];
        match *contributor {
            ContributorSpec::Landing(connection) => {
                targets.insert(
                    PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index: connection_input_index(connection),
                    },
                    TargetGeometry {
                        terminal: step(at, direction),
                        allowed_entry: direction,
                        support: at,
                        requirement: TerminalRequirement::Exact(
                            crate::compile::routing::RouteTerminalKind::BareMergeDust,
                        ),
                    },
                );
            }
            ContributorSpec::Primitive(primitive) => {
                let primitive_at = step(at, direction);
                let facing = repeater_facing_with_front(direction.opposite())?;
                place_primitive(
                    candidate,
                    primitive,
                    Primitive::Repeater,
                    facing,
                    primitive_at,
                    Some(instance.id),
                    occupied,
                    sources,
                )?;
                let input_index = instance
                    .expanded
                    .topology
                    .connections
                    .iter()
                    .find(|connection| connection.target == ConnectionTarget::Primitive(primitive))
                    .and_then(|connection| match connection.id {
                        ConnectionId::External { input_index, .. } => Some(input_index),
                        ConnectionId::Internal { .. } => None,
                    })
                    .ok_or(SeedError::Incomplete("isolating repeater input"))?;
                let rear = primitive_input_geometry(candidate, primitive, primitive_slot)?;
                primitive_slot += 1;
                targets.insert(
                    PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index,
                    },
                    rear,
                );
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn place_primitive(
    candidate: &mut ExpandedPhysicalCandidate,
    id: PrimitiveId,
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
    logical_owner: Option<InstanceId>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
) -> Result<(), SeedError> {
    let blocks = primitive_blocks(primitive, facing, anchor)?;
    commit_primitive(
        candidate,
        id,
        primitive,
        facing,
        anchor,
        logical_owner,
        occupied,
        sources,
        blocks,
    )
}

#[allow(clippy::too_many_arguments)]
fn place_primitive_searched(
    candidate: &mut ExpandedPhysicalCandidate,
    id: PrimitiveId,
    primitive: Primitive,
    facing: CellFacing,
    preferred: Anchor,
    logical_owner: Option<InstanceId>,
    instance: InstanceId,
    search: &mut PlacementSearch,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
) -> Result<(), SeedError> {
    let (anchor, blocks) =
        find_primitive_placement(primitive, facing, preferred, instance, id, search, occupied)?;
    commit_primitive(
        candidate,
        id,
        primitive,
        facing,
        anchor,
        logical_owner,
        occupied,
        sources,
        blocks,
    )
}

#[allow(clippy::too_many_arguments)]
fn find_primitive_placement(
    primitive: Primitive,
    facing: CellFacing,
    preferred: Anchor,
    instance: InstanceId,
    id: PrimitiveId,
    search: &mut PlacementSearch,
    occupied: &BTreeSet<Anchor>,
) -> Result<(Anchor, Vec<PlacedBlock>), SeedError> {
    for anchor in horizontal_manhattan_shells(preferred, search.max_radius) {
        let blocks = primitive_blocks(primitive, facing, anchor)?;
        if blocks.iter().all(|block| !occupied.contains(&block.at)) {
            return Ok((anchor, blocks));
        }
        search.reject_choice(instance, id)?;
    }
    Err(SeedError::PlacementExhausted {
        instance,
        primitive: id,
        radius: search.max_radius,
    })
}

fn primitive_blocks(
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
) -> Result<Vec<PlacedBlock>, SeedError> {
    let variants = physical::variants(primitive);
    let variant = variants
        .get(usize::from(facing.index()))
        .ok_or(SeedError::Incomplete("physical primitive variant"))?;
    Ok(variant
        .blocks
        .iter()
        .map(|block| PlacedBlock {
            at: translate(anchor, block.position),
            state: state_for_local(block.kind, block.facing, block.face),
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn commit_primitive(
    candidate: &mut ExpandedPhysicalCandidate,
    id: PrimitiveId,
    primitive: Primitive,
    facing: CellFacing,
    anchor: Anchor,
    logical_owner: Option<InstanceId>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    blocks: Vec<PlacedBlock>,
) -> Result<(), SeedError> {
    let variants = physical::variants(primitive);
    let variant = variants
        .get(usize::from(facing.index()))
        .ok_or(SeedError::Incomplete("physical primitive variant"))?;
    claim_blocks(occupied, &blocks)?;
    let delayed = blocks
        .iter()
        .find(|block| {
            matches!(
                block.state.kind,
                BlockKind::WallTorch | BlockKind::Torch | BlockKind::Repeater
            )
        })
        .map(|block| DelayedComponent {
            at: block.at,
            owner: DelayedOwner::Primitive(id),
        });
    candidate.placements.insert(
        id,
        PrimitivePlacement {
            id,
            variant: u16::from(facing.index()),
            facing,
            anchor,
            delayed,
            blocks: blocks.clone(),
        },
    );

    let output_kind = match primitive {
        Primitive::Torch => PortKind::TorchOutput,
        Primitive::Repeater => PortKind::RepeaterFront,
        _ => return Err(SeedError::Incomplete("unsupported seed primitive output")),
    };
    let output = variant.port(output_kind);
    let output_at = translate(anchor, output.position);
    candidate.observations.insert(
        ObservationId::PrimitiveOutput(id),
        VerifiedObservation {
            site: ObservationSite {
                id: ObservationId::PrimitiveOutput(id),
                at: output_at,
                logical_owner,
                display_label: None,
            },
            state: block_state_at(&blocks, output_at),
        },
    );
    sources.insert(
        PhysicalEndpointId::PrimitiveOutput(id),
        SourceGeometry {
            route_anchor: step(output_at, output.direction),
            allowed_exit: output.direction,
        },
    );
    Ok(())
}

fn horizontal_manhattan_shells(origin: Anchor, max_radius: u32) -> Vec<Anchor> {
    let mut anchors = vec![origin];
    for radius in 1..=max_radius {
        let radius = i32::try_from(radius).unwrap_or(i32::MAX);
        for dx in -radius..=radius {
            let dz = radius - dx.abs();
            anchors.push(Anchor {
                x: origin.x.saturating_add(dx),
                y: origin.y,
                z: origin.z.saturating_sub(dz),
            });
            if dz != 0 {
                anchors.push(Anchor {
                    x: origin.x.saturating_add(dx),
                    y: origin.y,
                    z: origin.z.saturating_add(dz),
                });
            }
        }
    }
    anchors
}

fn assign_primitive_targets(
    candidate: &ExpandedPhysicalCandidate,
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let mut ordinal_by_primitive = BTreeMap::<PrimitiveId, usize>::new();
    for connection in &instance.expanded.topology.connections {
        let ConnectionTarget::Primitive(primitive) = connection.target else {
            continue;
        };
        let ordinal = ordinal_by_primitive.entry(primitive).or_default();
        let geometry = primitive_input_geometry(candidate, primitive, *ordinal)?;
        *ordinal += 1;
        match connection.id {
            ConnectionId::External { input_index, .. } => {
                targets.insert(
                    PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index,
                    },
                    geometry,
                );
            }
            ConnectionId::Internal { .. } => {}
        }
    }
    Ok(())
}

fn primitive_input_geometry(
    candidate: &ExpandedPhysicalCandidate,
    primitive: PrimitiveId,
    ordinal: usize,
) -> Result<TargetGeometry, SeedError> {
    let placement = candidate
        .placements
        .get(&primitive)
        .ok_or(SeedError::Incomplete("primitive placement"))?;
    let specification = candidate
        .instances
        .instances
        .iter()
        .flat_map(|instance| &instance.expanded.topology.primitives)
        .find(|specification| specification.id == primitive)
        .ok_or(SeedError::Incomplete("primitive specification"))?;
    let variant = physical::variants(specification.primitive)
        .get(usize::from(placement.variant))
        .ok_or(SeedError::Incomplete("primitive variant"))?;
    match specification.primitive {
        Primitive::Torch => {
            let support = translate(
                placement.anchor,
                variant.port(PortKind::TorchInput).position,
            );
            let directions = geometry::input_directions(placement.facing);
            let direction = *directions
                .get(ordinal)
                .ok_or(SeedError::Incomplete("torch input socket"))?;
            Ok(TargetGeometry {
                terminal: step(support, direction),
                allowed_entry: direction,
                support,
                requirement: TerminalRequirement::Repeater,
            })
        }
        Primitive::Repeater => {
            let rear = variant.port(PortKind::RepeaterRear);
            let support = translate(placement.anchor, rear.position);
            Ok(TargetGeometry {
                terminal: step(support, rear.direction),
                allowed_entry: rear.direction,
                support,
                requirement: TerminalRequirement::DirectedDust,
            })
        }
        _ => Err(SeedError::Incomplete("unsupported primitive input")),
    }
}

fn route_all(
    candidate: &mut ExpandedPhysicalCandidate,
    router: &dyn PhysicalRouter,
    config: &SearchConfig,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &BTreeMap<PhysicalSink, TargetGeometry>,
    reservations: &mut PhysicalReservations,
) -> Result<(), SeedError> {
    let mut grouped = BTreeMap::<PhysicalEndpointId, Vec<PendingTarget>>::new();
    for instance in &candidate.instances.instances {
        for connection in &instance.expanded.topology.connections {
            let source = match connection.source {
                crate::compile::fragment_synth::topology::ConnectionSource::Primitive(id) => {
                    PhysicalEndpointId::PrimitiveOutput(id)
                }
                crate::compile::fragment_synth::topology::ConnectionSource::ExternalInput {
                    input_index,
                } => candidate
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
                    .and_then(|assignment| endpoint_for_driver(&assignment.driver))
                    .ok_or(SeedError::Incomplete("external connection source"))?,
            };
            let geometry = match connection.target {
                ConnectionTarget::Primitive(primitive) => {
                    let ordinal = instance
                        .expanded
                        .topology
                        .connections
                        .iter()
                        .filter(|candidate| {
                            candidate.target == ConnectionTarget::Primitive(primitive)
                        })
                        .take_while(|candidate| candidate.id != connection.id)
                        .count();
                    primitive_input_geometry(candidate, primitive, ordinal)?
                }
                ConnectionTarget::Junction(_) => targets
                    .get(&PhysicalSink::InstanceInput {
                        instance: instance.id,
                        input_index: connection_input_index(connection.id),
                    })
                    .copied()
                    .ok_or(SeedError::Incomplete("junction target"))?,
            };
            grouped
                .entry(source)
                .or_default()
                .push(PendingTarget::Connection(connection.id, geometry));
        }
    }
    for assignment in &candidate.instances.assignments {
        let PhysicalSink::DeclaredOutput(port) = assignment.sink else {
            continue;
        };
        let source = endpoint_for_driver(&assignment.driver)
            .ok_or(SeedError::Incomplete("declared output source"))?;
        let geometry = targets
            .get(&PhysicalSink::DeclaredOutput(port))
            .copied()
            .ok_or(SeedError::Incomplete("declared output target"))?;
        grouped
            .entry(source)
            .or_default()
            .push(PendingTarget::DeclaredOutput(port, geometry));
    }
    for pending in grouped.values().flatten() {
        let (endpoint, geometry) = match pending {
            PendingTarget::Connection(connection, geometry) => {
                (PhysicalEndpointId::Landing(*connection), *geometry)
            }
            PendingTarget::DeclaredOutput(port, geometry) => {
                (PhysicalEndpointId::DeclaredOutput(*port), *geometry)
            }
        };
        if reservations.get(&geometry.terminal).is_none() {
            reservations.reserve(
                geometry.terminal,
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }
    }

    let rank = topological_instance_order(&candidate.instances)
        .into_iter()
        .enumerate()
        .map(|(index, instance)| (instance, index))
        .collect::<BTreeMap<_, _>>();
    let output_rank = rank.len();
    let mut scheduled = grouped.into_iter().collect::<Vec<_>>();
    scheduled.sort_by_key(|(source, pending)| {
        let first_target = pending
            .iter()
            .map(|target| match target {
                PendingTarget::Connection(ConnectionId::External { instance, .. }, _)
                | PendingTarget::Connection(ConnectionId::Internal { instance, .. }, _) => {
                    rank.get(instance).copied().unwrap_or(output_rank)
                }
                PendingTarget::DeclaredOutput(_, _) => output_rank,
            })
            .min()
            .unwrap_or(output_rank);
        (first_target, *source)
    });
    let protected = sources
        .values()
        .map(|source| source.route_anchor)
        .chain(
            scheduled
                .iter()
                .flat_map(|(_, pending)| pending.iter().map(PendingTarget::geometry))
                .map(|geometry| geometry.terminal),
        )
        .collect::<BTreeSet<_>>();

    for (route_index, (source_id, mut pending)) in scheduled.into_iter().enumerate() {
        pending.sort_by_key(PendingTarget::key);
        let route = RouteId(u32::try_from(route_index).map_err(|_| SeedError::IdentityOverflow)?);
        let source = *sources
            .get(&source_id)
            .ok_or(SeedError::Incomplete("route source geometry"))?;
        let sinks = pending
            .iter()
            .enumerate()
            .map(|(ordinal, target)| {
                let id = RoutedSinkId {
                    route,
                    ordinal: u16::try_from(ordinal).map_err(|_| SeedError::IdentityOverflow)?,
                };
                let geometry = target.geometry();
                let (endpoint, route_target) = match target {
                    PendingTarget::Connection(connection, _) => (
                        PhysicalEndpointId::Landing(*connection),
                        crate::compile::routing::RouteTarget::Connection(*connection),
                    ),
                    PendingTarget::DeclaredOutput(port, _) => (
                        PhysicalEndpointId::DeclaredOutput(*port),
                        crate::compile::routing::RouteTarget::DeclaredOutput(*port),
                    ),
                };
                Ok(RouteSink {
                    id,
                    endpoint,
                    anchor: geometry.terminal,
                    allowed_entry: geometry.allowed_entry,
                    terminal: TerminalContract::Sink {
                        target: route_target,
                        support: geometry.support,
                        requirement: geometry.requirement,
                    },
                })
            })
            .collect::<Result<Vec<_>, SeedError>>()?;
        let sinks = NonEmptyRouteSinks::new(sinks).map_err(|_| SeedError::EmptyRoute)?;
        if matches!(source_id, PhysicalEndpointId::Junction(_))
            && !reservations.promote_endpoint_conductor(
                source.route_anchor,
                source_id,
                route,
                compile::repeater(source.allowed_exit),
            )
        {
            return Err(SeedError::Incomplete("junction source refresh reservation"));
        }
        if matches!(source_id, PhysicalEndpointId::Junction(_)) {
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                if direction == source.allowed_exit || direction == source.allowed_exit.opposite() {
                    continue;
                }
                let side = step(source.route_anchor, direction);
                if reservations.get(&side).is_none() {
                    reservations.reserve(
                        side,
                        PhysicalReservationOwner::KeepOut(route.0),
                        PhysicalReservationKind::KeepOut,
                    );
                }
            }
        }
        let mut tree = router.route(RouteRequest {
            id: route,
            source: RouteEndpoint {
                id: source_id,
                anchor: source.route_anchor,
                allowed_exit: source.allowed_exit,
                terminal: TerminalContract::Source {
                    signal_strength: MAX_SIGNAL_STRENGTH,
                },
            },
            sinks: &sinks,
            reservations,
            limits: config.router_limits,
        })?;
        refresh_exact_route_delays(&mut tree);

        for (target, branch) in pending.iter().zip(&tree.branches) {
            if let PendingTarget::Connection(connection, _) = target {
                candidate.connections.insert(
                    *connection,
                    ConnectionBinding {
                        id: *connection,
                        source: source_id,
                        landing: PhysicalEndpointId::Landing(*connection),
                        route,
                        sink: branch.sink,
                    },
                );
            }
        }
        reserve_route(reservations, &tree, &protected);
        candidate.routes.insert(route, tree);
    }
    Ok(())
}

fn refresh_exact_route_delays(tree: &mut crate::compile::routing::RealisedRouteTree) {
    let repeaters = tree
        .cells
        .iter()
        .filter(|block| block.state.kind == BlockKind::Repeater)
        .map(|block| block.at)
        .collect::<BTreeSet<_>>();
    for branch in &mut tree.branches {
        branch.terminal.repeaters = branch
            .path
            .iter()
            .filter(|at| repeaters.contains(at))
            .filter(|at| {
                !(**at == branch.terminal.at
                    && branch.terminal.kind
                        == crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater)
            })
            .count() as u64;
    }
}

fn reservations_for_components(
    candidate: &ExpandedPhysicalCandidate,
) -> Result<PhysicalReservations, SeedError> {
    let mut reservations = PhysicalReservations::new();
    let mut ordinal = 0u32;
    for block in candidate
        .placements
        .values()
        .flat_map(|placement| &placement.blocks)
        .chain(
            candidate
                .boundaries
                .values()
                .flat_map(|boundary| &boundary.blocks),
        )
        .chain(
            candidate
                .junctions
                .values()
                .flat_map(|junction| &junction.cells),
        )
    {
        if reservations.get(&block.at).is_some() {
            return Err(SeedError::PlacementCollision { at: block.at });
        }
        reservations.reserve(
            block.at,
            PhysicalReservationOwner::KeepOut(ordinal),
            PhysicalReservationKind::KeepOut,
        );
        ordinal = ordinal.saturating_add(1);
    }
    Ok(reservations)
}

fn reserve_route(
    reservations: &mut PhysicalReservations,
    tree: &crate::compile::routing::RealisedRouteTree,
    protected: &BTreeSet<Anchor>,
) {
    for block in &tree.cells {
        reservations.reserve(
            block.at,
            PhysicalReservationOwner::Route(tree.id),
            PhysicalReservationKind::Conductor(block.state.clone()),
        );
    }
    for block in &tree.floors {
        reservations.reserve(
            block.at,
            PhysicalReservationOwner::RouteStair(tree.id),
            PhysicalReservationKind::Floor(block.state.clone()),
        );
    }
    for block in &tree.cells {
        for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
            let halo = step(block.at, direction);
            if protected.contains(&halo) || reservations.get(&halo).is_some() {
                continue;
            }
            reservations.reserve(
                halo,
                PhysicalReservationOwner::KeepOut(tree.id.0),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
}

fn topological_instance_order(graph: &InstanceGraph) -> Vec<InstanceId> {
    let ids = graph
        .instances
        .iter()
        .map(|instance| instance.id)
        .collect::<BTreeSet<_>>();
    let mut predecessors = ids
        .iter()
        .copied()
        .map(|id| (id, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    for assignment in &graph.assignments {
        let PhysicalSink::InstanceInput { instance, .. } = assignment.sink else {
            continue;
        };
        if let PhysicalDriver::Instance(driver) = &assignment.driver {
            let owner = match driver {
                crate::compile::fragment_synth::instance_graph::InstanceDriver::Primitive {
                    logical_owner,
                    ..
                }
                | crate::compile::fragment_synth::instance_graph::InstanceDriver::Junction {
                    logical_owner,
                    ..
                } => *logical_owner,
            };
            if owner != instance {
                predecessors.entry(instance).or_default().insert(owner);
            }
        }
    }
    let mut remaining = ids;
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let next = remaining.iter().copied().find(|id| {
            predecessors[id]
                .iter()
                .all(|before| !remaining.contains(before))
        });
        let Some(next) = next else {
            ordered.extend(remaining);
            break;
        };
        remaining.remove(&next);
        ordered.push(next);
    }
    ordered
}

fn repeater_facing_with_front(direction: Facing) -> Result<CellFacing, SeedError> {
    (0..4u8)
        .filter_map(CellFacing::from_index)
        .find(|facing| {
            physical::variants(Primitive::Repeater)[usize::from(facing.index())]
                .port(PortKind::RepeaterFront)
                .direction
                == direction
        })
        .ok_or(SeedError::Incomplete("repeater front direction"))
}

fn contributor_endpoint(contributor: &ContributorSpec) -> PhysicalEndpointId {
    match *contributor {
        ContributorSpec::Landing(connection) => PhysicalEndpointId::Landing(connection),
        ContributorSpec::Primitive(primitive) => PhysicalEndpointId::PrimitiveOutput(primitive),
    }
}

fn connection_input_index(connection: ConnectionId) -> u16 {
    match connection {
        ConnectionId::External { input_index, .. } => input_index,
        ConnectionId::Internal { edge_index, .. } => edge_index,
    }
}

fn claim_blocks(occupied: &mut BTreeSet<Anchor>, blocks: &[PlacedBlock]) -> Result<(), SeedError> {
    for block in blocks {
        if occupied.contains(&block.at) {
            return Err(SeedError::PlacementCollision { at: block.at });
        }
    }
    occupied.extend(blocks.iter().map(|block| block.at));
    Ok(())
}

fn block_state_at(blocks: &[PlacedBlock], at: Anchor) -> BlockState {
    blocks
        .iter()
        .find(|block| block.at == at)
        .map(|block| block.state.clone())
        .unwrap_or_else(BlockState::air)
}

fn translate(anchor: Anchor, local: Position) -> Anchor {
    Anchor {
        x: anchor.x + local.x,
        y: anchor.y + local.y,
        z: anchor.z + local.z,
    }
}

fn state_for_local(
    kind: BlockKind,
    facing: Option<Facing>,
    face: Option<crate::redstone::world::block::Face>,
) -> BlockState {
    let mut state = match kind {
        BlockKind::Solid => compile::stone(),
        BlockKind::Repeater => {
            let mut state = BlockState::air();
            state.kind = BlockKind::Repeater;
            state.name = "minecraft:repeater".to_string();
            state.delay = 1;
            state.lit = true;
            state
        }
        BlockKind::WallTorch => {
            let mut state = BlockState::air();
            state.kind = BlockKind::WallTorch;
            state.name = "minecraft:redstone_wall_torch".to_string();
            state.lit = true;
            state
        }
        BlockKind::Lever => compile::lever(false),
        BlockKind::Lamp => compile::lamp(),
        _ => {
            let mut state = BlockState::air();
            state.kind = kind;
            state
        }
    };
    state.facing = facing;
    state.face = face;
    state
}

fn step(at: Anchor, direction: Facing) -> Anchor {
    match direction {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::emission::{EmittedWorld, PhysicalCandidateView};
    use crate::compile::fragment_synth::certification::CompleteCandidateCertifier;
    use crate::compile::fragment_synth::legacy_adapter::{LegacyCandidateAdapter, LegacyOracle};
    use crate::compile::fragment_synth::services::{DurableSeedEmitter, DurableSeedVerifier};
    use crate::compile::routing::{DurablePhysicalRouter, RealisedRouteTree};

    #[test]
    fn placement_search_visits_stable_manhattan_shells_and_skips_collisions() {
        let preferred = Anchor { x: 8, y: 4, z: 9 };
        let shell = horizontal_manhattan_shells(preferred, 1);
        assert_eq!(
            shell,
            vec![
                preferred,
                Anchor { x: 7, y: 4, z: 9 },
                Anchor { x: 8, y: 4, z: 8 },
                Anchor { x: 8, y: 4, z: 10 },
                Anchor { x: 9, y: 4, z: 9 },
            ]
        );

        let occupied = primitive_blocks(Primitive::Torch, CellFacing::EAST, preferred)
            .unwrap()
            .into_iter()
            .map(|block| block.at)
            .collect::<BTreeSet<_>>();
        let mut search = PlacementSearch {
            max_radius: 4,
            max_backtracks: 100,
            backtracks_used: 0,
        };
        let (selected, blocks) = find_primitive_placement(
            Primitive::Torch,
            CellFacing::EAST,
            preferred,
            InstanceId(3),
            PrimitiveId {
                instance: InstanceId(3),
                node: crate::compile::fragment_synth::identity::TopologyNodeId(2),
            },
            &mut search,
            &occupied,
        )
        .unwrap();

        assert_ne!(selected, preferred);
        assert!(search.backtracks_used > 0);
        assert!(blocks.iter().all(|block| !occupied.contains(&block.at)));
    }

    #[test]
    fn placement_search_honours_the_backtrack_cap() {
        let preferred = Anchor { x: 8, y: 4, z: 9 };
        let occupied = primitive_blocks(Primitive::Torch, CellFacing::EAST, preferred)
            .unwrap()
            .into_iter()
            .map(|block| block.at)
            .collect::<BTreeSet<_>>();
        let primitive = PrimitiveId {
            instance: InstanceId(3),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(2),
        };
        let mut search = PlacementSearch {
            max_radius: 4,
            max_backtracks: 1,
            backtracks_used: 0,
        };

        let error = find_primitive_placement(
            Primitive::Torch,
            CellFacing::EAST,
            preferred,
            InstanceId(3),
            primitive,
            &mut search,
            &occupied,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SeedError::PlacementExhausted {
                instance: InstanceId(3),
                primitive: actual,
                radius: 4,
            } if actual == primitive
        ));
    }

    fn not_netlist() -> Netlist {
        Netlist {
            inputs: vec!["a".to_string()],
            outputs: vec!["y".to_string()],
            gates: vec![crate::compile::Gate {
                name: "not".to_string(),
                inputs: vec!["a".to_string()],
                output: "y".to_string(),
                kind: crate::compile::topology::GateKind::Nor(1),
            }],
        }
    }

    fn build(netlist: &Netlist) -> Result<CertifiedCandidate, SeedError> {
        build_with_pins(netlist, None)
    }

    fn build_with_pins(
        netlist: &Netlist,
        pins: Option<&PortPlacements>,
    ) -> Result<CertifiedCandidate, SeedError> {
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        compile_sparse_seed_with_services(
            SeedInput {
                lowered: netlist,
                source_provenance: None,
                pins,
            },
            SeedServices {
                library: &library,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
    }

    #[derive(Default)]
    struct CountingRouter {
        calls: Cell<u32>,
    }

    impl PhysicalRouter for CountingRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.calls.set(self.calls.get() + 1);
            DurablePhysicalRouter.route(request)
        }
    }

    #[derive(Default)]
    struct CountingEmitter {
        calls: Cell<u32>,
    }

    impl SeedEmitter for CountingEmitter {
        fn emit(
            &self,
            candidate: &dyn PhysicalCandidateView,
            size: (i32, i32, i32),
        ) -> Result<EmittedWorld, EmissionError> {
            self.calls.set(self.calls.get() + 1);
            DurableSeedEmitter.emit(candidate, size)
        }
    }

    #[derive(Default)]
    struct CountingVerifier {
        calls: Cell<u32>,
    }

    impl SeedVerifier for CountingVerifier {
        fn verify(
            &self,
            candidate: &ExpandedPhysicalCandidate,
            emitted: &EmittedWorld,
        ) -> Result<(), ExpandedPhysicalError> {
            self.calls.set(self.calls.get() + 1);
            DurableSeedVerifier.verify(candidate, emitted)
        }
    }

    #[derive(Default)]
    struct CountingCertifier {
        calls: Cell<u32>,
    }

    impl ExpandedCandidateCertifier for CountingCertifier {
        fn certify(
            &self,
            candidate: ExpandedPhysicalCandidate,
            lowered: &Netlist,
            library: &Library,
            config: &CertificationConfig,
        ) -> Result<CertifiedCandidate, CandidateCertificationError> {
            self.calls.set(self.calls.get() + 1);
            CompleteCandidateCertifier.certify(candidate, lowered, library, config)
        }
    }

    #[derive(Default)]
    struct CountingLegacyOracle {
        calls: Cell<u32>,
        compile_legacy_calls: Cell<u32>,
        compile_planned_calls: Cell<u32>,
        compile_grown_calls: Cell<u32>,
        seed_from_legacy_calls: Cell<u32>,
        plan_from_netlist_calls: Cell<u32>,
    }

    impl LegacyOracle for CountingLegacyOracle {
        fn compile_legacy(
            &self,
            netlist: &Netlist,
        ) -> Result<crate::compile::CompiledCircuit, crate::compile::CompileError> {
            self.calls.set(self.calls.get() + 1);
            self.compile_legacy_calls
                .set(self.compile_legacy_calls.get() + 1);
            crate::compile::compile_legacy(netlist)
        }
    }

    fn gate(
        name: &str,
        inputs: &[&str],
        output: &str,
        kind: crate::compile::topology::GateKind,
    ) -> crate::compile::Gate {
        crate::compile::Gate {
            name: name.to_string(),
            inputs: inputs.iter().map(|input| (*input).to_string()).collect(),
            output: output.to_string(),
            kind,
        }
    }

    #[test]
    fn not_seed_is_fully_certified_and_deterministic() {
        let netlist = not_netlist();
        let first = build(&netlist).unwrap();
        let second = build(&netlist).unwrap();

        assert_eq!(
            first.metrics().candidate_fingerprint,
            second.metrics().candidate_fingerprint
        );
        assert_eq!(
            first.metrics().emitted_world_fingerprint,
            second.metrics().emitted_world_fingerprint
        );
    }

    #[test]
    fn production_seed_uses_every_durable_service_and_no_legacy_entrypoint() {
        let (netlist, _) = build_and4_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
        let emitter = CountingEmitter::default();
        let verifier = CountingVerifier::default();
        let certifier = CountingCertifier::default();
        let legacy = CountingLegacyOracle::default();

        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                router: &router,
                emitter: &emitter,
                verifier: &verifier,
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap();

        assert_eq!(
            certified.candidate().instances.instances.len(),
            netlist.gates.len()
        );
        assert!(router.calls.get() > 0);
        assert!(emitter.calls.get() > 0);
        assert!(verifier.calls.get() > 0);
        assert!(certifier.calls.get() > 0);
        assert_eq!(legacy.calls.get(), 0);
        assert_eq!(legacy.compile_legacy_calls.get(), 0);
        assert_eq!(legacy.compile_planned_calls.get(), 0);
        assert_eq!(legacy.compile_grown_calls.get(), 0);
        assert_eq!(legacy.seed_from_legacy_calls.get(), 0);
        assert_eq!(legacy.plan_from_netlist_calls.get(), 0);

        LegacyCandidateAdapter::adapt_from_oracle(&netlist, &legacy).unwrap();
        assert_eq!(legacy.calls.get(), 1);
        assert_eq!(legacy.compile_legacy_calls.get(), 1);
        assert_eq!(legacy.compile_planned_calls.get(), 0);
        assert_eq!(legacy.compile_grown_calls.get(), 0);
        assert_eq!(legacy.seed_from_legacy_calls.get(), 0);
        assert_eq!(legacy.plan_from_netlist_calls.get(), 0);
    }

    #[test]
    fn and4_seed_is_fully_certified_and_deterministic() {
        let (netlist, _) = build_and4_netlist();
        let first = build(&netlist).unwrap();
        let second = build(&netlist).unwrap();

        assert_eq!(
            first.candidate().instances.instances.len(),
            netlist.gates.len()
        );
        assert_eq!(
            first.metrics().candidate_fingerprint,
            second.metrics().candidate_fingerprint
        );
        assert_eq!(
            first.metrics().emitted_world_fingerprint,
            second.metrics().emitted_world_fingerprint
        );
    }

    #[test]
    fn pinned_and4_preserves_caller_cells_and_handover_directions() {
        let (netlist, _) = build_and4_netlist();
        let input_at = Anchor { x: 21, y: 1, z: 62 };
        let output_at = Anchor { x: 53, y: 1, z: 10 };
        let output_name = netlist.outputs[0].clone();
        let mut pins = PortPlacements::default();
        pins.pin("a", input_at, Facing::North)
            .pin(output_name.clone(), output_at, Facing::North);

        let certified = build_with_pins(&netlist, Some(&pins)).unwrap();
        let repeated = build_with_pins(&netlist, Some(&pins)).unwrap();
        let candidate = certified.candidate();
        assert_eq!(
            certified.metrics().candidate_fingerprint,
            repeated.metrics().candidate_fingerprint
        );
        assert_eq!(
            certified.metrics().emitted_world_fingerprint,
            repeated.metrics().emitted_world_fingerprint
        );
        assert_eq!(candidate.pins.get("a").unwrap().at, input_at);
        assert_eq!(candidate.pins.get(&output_name).unwrap().at, output_at);
        assert_eq!(
            candidate.observations[&ObservationId::PrimaryInput(PortId(0))]
                .state
                .kind,
            BlockKind::Air
        );
        assert_eq!(
            candidate.observations[&ObservationId::DeclaredOutput(PortId(0))]
                .state
                .kind,
            BlockKind::Air
        );
    }

    #[test]
    fn fanout_buf_and_merge_seed_shapes_are_fully_certified() {
        use crate::compile::topology::GateKind;

        let fixtures = [
            Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into(), "z".into()],
                gates: vec![
                    gate("source", &["a"], "n", GateKind::Nor(1)),
                    gate("left", &["n"], "y", GateKind::Nor(1)),
                    gate("right", &["n"], "z", GateKind::Nor(1)),
                ],
            },
            Netlist {
                inputs: vec!["a".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("buf", &["a"], "y", GateKind::Buf)],
            },
            Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into()],
                gates: vec![gate("bare", &["a", "b"], "y", GateKind::Or(2))],
            },
            Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into(), "z".into()],
                gates: vec![
                    gate("mixed", &["a", "b"], "y", GateKind::Or(2)),
                    gate("fanout", &["a"], "z", GateKind::Nor(1)),
                ],
            },
            Netlist {
                inputs: vec!["a".into(), "b".into()],
                outputs: vec!["y".into(), "u".into(), "v".into()],
                gates: vec![
                    gate("isolated", &["a", "b"], "y", GateKind::Or(2)),
                    gate("left", &["a"], "u", GateKind::Nor(1)),
                    gate("right", &["b"], "v", GateKind::Nor(1)),
                ],
            },
        ];

        for (fixture_index, fixture) in fixtures.into_iter().enumerate() {
            let certified = build(&fixture)
                .unwrap_or_else(|error| panic!("fixture {fixture_index} failed: {error:?}"));
            let repeated = build(&fixture)
                .unwrap_or_else(|error| panic!("fixture {fixture_index} repeat failed: {error:?}"));
            assert_eq!(
                certified.candidate().instances.instances.len(),
                fixture.gates.len()
            );
            assert_eq!(
                certified.metrics().candidate_fingerprint,
                repeated.metrics().candidate_fingerprint
            );
            assert_eq!(
                certified.metrics().emitted_world_fingerprint,
                repeated.metrics().emitted_world_fingerprint
            );
        }
    }

    #[test]
    fn stateful_topology_is_rejected_before_any_physical_service() {
        let netlist = Netlist {
            inputs: vec!["d".into(), "clk".into()],
            outputs: vec!["q".into()],
            gates: vec![gate(
                "ff",
                &["d", "clk"],
                "q",
                crate::compile::topology::GateKind::DffPosedge,
            )],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
        let emitter = CountingEmitter::default();
        let verifier = CountingVerifier::default();
        let certifier = CountingCertifier::default();

        let error = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                router: &router,
                emitter: &emitter,
                verifier: &verifier,
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SeedError::InstanceGraph(SynthesisError::UnsupportedStatefulTopology { .. })
        ));
        assert_eq!(router.calls.get(), 0);
        assert_eq!(emitter.calls.get(), 0);
        assert_eq!(verifier.calls.get(), 0);
        assert_eq!(certifier.calls.get(), 0);
    }

    #[test]
    fn invalid_pin_is_rejected_before_any_physical_service() {
        let netlist = not_netlist();
        let mut pins = PortPlacements::default();
        pins.pin("a", Anchor { x: 4, y: 1, z: 4 }, Facing::Up);
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
        let emitter = CountingEmitter::default();
        let verifier = CountingVerifier::default();
        let certifier = CountingCertifier::default();

        let error = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: Some(&pins),
            },
            SeedServices {
                library: &library,
                router: &router,
                emitter: &emitter,
                verifier: &verifier,
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap_err();

        assert!(matches!(error, SeedError::InvalidPins(_)));
        assert_eq!(router.calls.get(), 0);
        assert_eq!(emitter.calls.get(), 0);
        assert_eq!(verifier.calls.get(), 0);
        assert_eq!(certifier.calls.get(), 0);
    }

    #[test]
    fn zero_seed_radius_is_a_named_bounded_refusal() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let mut config = SearchConfig::checked_defaults();
        config.max_seed_shell_radius = 0;
        let error = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SeedError::PlacementExhausted {
                instance: InstanceId(0),
                radius: 0,
                ..
            }
        ));
    }
}
