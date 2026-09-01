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
    ConnectionId, ImplementationKey, InstanceId, ObservationId, ObservationSite,
    PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::fragment_synth::instance_graph::{
    DuplicateRequest, InstanceGraph, PhysicalDriver, PhysicalSink, SynthesisError,
};
use crate::compile::fragment_synth::placement::{
    analyse_instance_dag, SeedPlacementAnalysis, SeedPlacementPlan, SeedPlacementRequest,
    SeedPlacer,
};
use crate::compile::fragment_synth::realise::{ExpandedAdapterError, ExpandedCandidateAdapter};
use crate::compile::fragment_synth::route_schedule::{
    RouteObligation, RouteSchedule, TargetObligation,
};
use crate::compile::fragment_synth::services::{SeedEmitter, SeedVerifier};
use crate::compile::fragment_synth::topology::{
    ConnectionSource, ConnectionTarget, ContributorSpec, OutputSpec,
};
use crate::compile::geometry::{self, Anchor, CellFacing};
use crate::compile::metrics::Fingerprint;
use crate::compile::physical::{self, PortKind};
use crate::compile::planner::{PortPlacements, PortRole};
use crate::compile::routing::{
    DelayedComponent, DelayedOwner, NonEmptyRouteSinks, PhysicalReservationKind,
    PhysicalReservationOwner, PhysicalReservations, PhysicalRouter, RouteEndpoint, RouteRequest,
    RouteSink, RouterFailure, RouterLimitKind, RouterRefusalCategory, TerminalContract,
    TerminalRequirement,
};
use crate::compile::topology::{Library, Primitive};
use crate::compile::verification::ExpandedPhysicalError;
use crate::compile::{self, Netlist};
use crate::redstone::simulator::position::Position;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};

const ORIGIN_WORLD_MARGIN: i32 = 16;

#[derive(Clone, Copy)]
pub(crate) struct SeedInput<'a> {
    pub lowered: &'a Netlist,
    pub source_provenance: Option<&'a [usize]>,
    pub pins: Option<&'a PortPlacements>,
}

#[derive(Clone, Copy)]
pub(crate) struct SeedServices<'a> {
    pub library: &'a Library,
    pub placer: &'a dyn SeedPlacer,
    pub router: &'a dyn PhysicalRouter,
    pub emitter: &'a dyn SeedEmitter,
    pub verifier: &'a dyn SeedVerifier,
    pub certifier: &'a dyn ExpandedCandidateCertifier,
    pub search_config: &'a SearchConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InstancePlacementOverride {
    pub facing: CellFacing,
    pub dx: i32,
    pub dz: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SeedVariant {
    pub implementations: BTreeMap<InstanceId, ImplementationKey>,
    pub placements: BTreeMap<InstanceId, InstancePlacementOverride>,
    pub duplicates: Vec<DuplicateRequest>,
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
    Routing(#[source] SeedRoutingFailure),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeedRoutingFailure {
    pub scheduled_index: usize,
    pub route: RouteId,
    pub source: PhysicalEndpointId,
    pub sink: RoutedSinkId,
    pub category: RouterRefusalCategory,
    pub limit_kind: Option<RouterLimitKind>,
    pub limit: Option<u64>,
    pub work_used: Option<u64>,
    pub plan_fingerprint: Fingerprint,
    pub source_at: Anchor,
    pub sink_at: Anchor,
}

impl std::fmt::Display for SeedRoutingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "scheduled route {} ({:?}) from {:?} at {:?} failed at {:?} at {:?} as {:?}",
            self.scheduled_index,
            self.route,
            self.source,
            self.source_at,
            self.sink,
            self.sink_at,
            self.category
        )
    }
}

impl std::error::Error for SeedRoutingFailure {}

pub(crate) struct SparseSeedBuilder;

pub(crate) fn compile_sparse_seed_with_services(
    input: SeedInput<'_>,
    services: SeedServices<'_>,
) -> Result<CertifiedCandidate, SeedError> {
    SparseSeedBuilder::build(input, services)
}

pub(crate) fn compile_sparse_seed_variant_with_services(
    input: SeedInput<'_>,
    services: SeedServices<'_>,
    variant: &SeedVariant,
) -> Result<CertifiedCandidate, SeedError> {
    SparseSeedBuilder::build_variant(input, services, variant)
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

#[derive(Debug, Clone, Copy, Default)]
struct PlanTranslation {
    dx: i32,
    dz: i32,
}

impl PlanTranslation {
    fn for_unpinned(plan: &SeedPlacementPlan, has_pins: bool) -> Self {
        if has_pins {
            return Self::default();
        }
        let anchors = plan
            .instances
            .values()
            .map(|pose| pose.preferred_origin)
            .chain(plan.automatic_inputs.values().copied())
            .chain(plan.automatic_outputs.values().copied())
            .collect::<Vec<_>>();
        let min_x = anchors.iter().map(|anchor| anchor.x).min().unwrap_or(0);
        let min_z = anchors.iter().map(|anchor| anchor.z).min().unwrap_or(0);
        Self {
            dx: ORIGIN_WORLD_MARGIN.saturating_sub(min_x).max(0),
            dz: ORIGIN_WORLD_MARGIN.saturating_sub(min_z).max(0),
        }
    }

    fn apply(self, anchor: Anchor) -> Anchor {
        Anchor {
            x: anchor.x.saturating_add(self.dx),
            z: anchor.z.saturating_add(self.dz),
            ..anchor
        }
    }
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
        Self::build_variant(input, services, &SeedVariant::default())
    }

    fn build_variant(
        input: SeedInput<'_>,
        services: SeedServices<'_>,
        variant: &SeedVariant,
    ) -> Result<CertifiedCandidate, SeedError> {
        if let Some(provenance) = input.source_provenance {
            if provenance.len() != input.lowered.gates.len() {
                return Err(SeedError::ProvenanceWidth {
                    expected: input.lowered.gates.len(),
                    actual: provenance.len(),
                });
            }
        }
        let instances = InstanceGraph::with_variants(
            input.lowered,
            services.library,
            &variant.implementations,
            &variant.duplicates,
        )?;
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
        let placement_analysis = analyse_instance_dag(&candidate.instances)
            .map_err(|_| SeedError::Incomplete("seed placement analysis"))?;
        let placement_plan = services
            .placer
            .plan(SeedPlacementRequest {
                graph: &candidate.instances,
                analysis: &placement_analysis,
                pins: &candidate.pin_contracts,
            })
            .map_err(|_| SeedError::Incomplete("seed placement plan"))?;
        let plan_translation =
            PlanTranslation::for_unpinned(&placement_plan, !candidate.pin_contracts.is_empty());

        let mut occupied = BTreeSet::new();
        let mut sources = BTreeMap::new();
        let mut targets = BTreeMap::new();
        place_boundaries(
            &mut candidate,
            input.lowered,
            &placement_plan,
            plan_translation,
            &mut occupied,
            &mut sources,
            &mut targets,
        )?;
        place_instances(
            &mut candidate,
            input.lowered,
            services.search_config,
            &placement_plan,
            plan_translation,
            &variant.placements,
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
            &placement_analysis,
            &placement_plan.fingerprint,
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
    for (&primitive, placement) in &candidate.placements {
        let Some(specification) = candidate
            .instances
            .instances
            .iter()
            .flat_map(|instance| &instance.expanded.topology.primitives)
            .find(|specification| specification.id == primitive)
        else {
            continue;
        };
        if specification.primitive != Primitive::Torch {
            continue;
        }
        let variant = &physical::variants(specification.primitive)[usize::from(placement.variant)];
        let support = translate(
            placement.anchor,
            variant.port(PortKind::TorchInput).position,
        );
        for direction in geometry::input_directions(placement.facing) {
            let socket = step(support, direction);
            if !endpoints.contains(&socket) && reservations.get(&socket).is_none() {
                reservations.reserve(
                    socket,
                    PhysicalReservationOwner::KeepOut(
                        primitive.instance.0 ^ u32::from(primitive.node.0),
                    ),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }
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
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
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
                let home = plan
                    .automatic_inputs
                    .get(&port)
                    .copied()
                    .ok_or(SeedError::Incomplete("automatic input placement"))?;
                let toward = automatic_boundary_direction(candidate);
                let home = plan_translation.apply(home);
                let root = step(home, toward);
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
                    toward,
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
                let lamp = plan
                    .automatic_outputs
                    .get(&port)
                    .copied()
                    .ok_or(SeedError::Incomplete("automatic output placement"))?;
                let toward = automatic_boundary_direction(candidate);
                let lamp = plan_translation.apply(lamp);
                let terminal = step(lamp, toward.opposite());
                let blocks = vec![PlacedBlock {
                    at: lamp,
                    state: compile::lamp(),
                }];
                (
                    blocks,
                    lamp,
                    TargetGeometry {
                        terminal,
                        allowed_entry: toward.opposite(),
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

fn automatic_boundary_direction(candidate: &ExpandedPhysicalCandidate) -> Facing {
    let inputs = candidate
        .pin_contracts
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::PrimaryInput(_)).then_some(*pin)
        })
        .collect::<Vec<_>>();
    let outputs = candidate
        .pin_contracts
        .iter()
        .filter_map(|(endpoint, pin)| {
            matches!(endpoint, PhysicalEndpointId::DeclaredOutput(_)).then_some(*pin)
        })
        .collect::<Vec<_>>();
    if !inputs.is_empty() && !outputs.is_empty() {
        let from = median_anchor(inputs.iter().map(|pin| pin.net_cell(PortRole::Input)));
        let to = median_anchor(outputs.iter().map(|pin| pin.net_cell(PortRole::Output)));
        dominant_horizontal_direction(from, to)
    } else if !inputs.is_empty() {
        majority_direction(inputs.iter().map(|pin| pin.toward))
    } else if !outputs.is_empty() {
        majority_direction(outputs.iter().map(|pin| pin.toward)).opposite()
    } else {
        Facing::East
    }
}

fn median_anchor(anchors: impl Iterator<Item = Anchor>) -> Anchor {
    let anchors = anchors.collect::<Vec<_>>();
    let mut xs = anchors.iter().map(|anchor| anchor.x).collect::<Vec<_>>();
    let mut ys = anchors.iter().map(|anchor| anchor.y).collect::<Vec<_>>();
    let mut zs = anchors.iter().map(|anchor| anchor.z).collect::<Vec<_>>();
    xs.sort();
    ys.sort();
    zs.sort();
    Anchor {
        x: xs[xs.len() / 2],
        y: ys[ys.len() / 2],
        z: zs[zs.len() / 2],
    }
}

fn majority_direction(directions: impl Iterator<Item = Facing>) -> Facing {
    let order = [Facing::North, Facing::East, Facing::South, Facing::West];
    let directions = directions.collect::<Vec<_>>();
    order
        .into_iter()
        .max_by_key(|candidate| {
            (
                directions
                    .iter()
                    .filter(|direction| *direction == candidate)
                    .count(),
                std::cmp::Reverse(
                    order
                        .iter()
                        .position(|direction| direction == candidate)
                        .unwrap(),
                ),
            )
        })
        .unwrap_or(Facing::East)
}

fn dominant_horizontal_direction(from: Anchor, to: Anchor) -> Facing {
    let dx = i64::from(to.x) - i64::from(from.x);
    let dz = i64::from(to.z) - i64::from(from.z);
    if dz.abs() >= dx.abs() && dz != 0 {
        if dz < 0 {
            Facing::North
        } else {
            Facing::South
        }
    } else if dx < 0 {
        Facing::West
    } else {
        Facing::East
    }
}

fn place_instances(
    candidate: &mut ExpandedPhysicalCandidate,
    netlist: &Netlist,
    search_config: &SearchConfig,
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
    placement_overrides: &BTreeMap<InstanceId, InstancePlacementOverride>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    let order = topological_instance_order(&candidate.instances);
    let mut placement_search = PlacementSearch::new(search_config);

    for instance_id in order {
        let instance = candidate
            .instances
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .ok_or(SeedError::Incomplete("topological instance"))?
            .clone();
        let planned = plan
            .instances
            .get(&instance.id)
            .copied()
            .ok_or(SeedError::Incomplete("planned instance pose"))?;
        let placement_override = placement_overrides.get(&instance.id).copied();
        let base = plan_translation.apply(Anchor {
            x: planned
                .preferred_origin
                .x
                .saturating_add(placement_override.map_or(0, |choice| choice.dx)),
            z: planned
                .preferred_origin
                .z
                .saturating_add(placement_override.map_or(0, |choice| choice.dz)),
            ..planned.preferred_origin
        });
        let gate = &netlist.gates
            [usize::try_from(instance.logical_gate.0).map_err(|_| SeedError::IdentityOverflow)?];

        match &instance.expanded.topology.output {
            OutputSpec::Junction { contributors, .. } => {
                let facing = placement_override
                    .map(|choice| choice.facing)
                    .unwrap_or(planned.facing);
                place_junction_instance(
                    candidate,
                    &instance,
                    gate,
                    contributors,
                    base,
                    facing,
                    occupied,
                    sources,
                    targets,
                )?;
            }
            OutputSpec::Primitive(output) => {
                let facing = placement_override
                    .map(|choice| choice.facing)
                    .unwrap_or(planned.facing);
                let positions = topology_primitive_positions(&instance);
                for specification in &instance.expanded.topology.primitives {
                    let local = positions[&specification.id];
                    let (dx, dy, dz) = geometry::rotate((local.x, local.y, local.z), facing);
                    let anchor = Anchor {
                        x: base.x.saturating_add(dx),
                        y: base.y.saturating_add(dy),
                        z: base.z.saturating_add(dz),
                    };
                    place_primitive_searched(
                        candidate,
                        specification.id,
                        specification.primitive,
                        facing,
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

fn topology_primitive_positions(
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
) -> BTreeMap<PrimitiveId, Position> {
    let topology = &instance.expanded.topology;
    let mut levels = BTreeMap::<PrimitiveId, i32>::new();
    while levels.len() < topology.primitives.len() {
        let before = levels.len();
        for primitive in &topology.primitives {
            if levels.contains_key(&primitive.id) {
                continue;
            }
            let incoming = topology
                .connections
                .iter()
                .filter(|edge| edge.target == ConnectionTarget::Primitive(primitive.id))
                .filter_map(|edge| match edge.source {
                    ConnectionSource::Primitive(source) => Some(source),
                    ConnectionSource::ExternalInput { .. } => None,
                })
                .collect::<Vec<_>>();
            if incoming.iter().all(|source| levels.contains_key(source)) {
                levels.insert(
                    primitive.id,
                    incoming
                        .iter()
                        .map(|source| levels[source] + 1)
                        .max()
                        .unwrap_or(0),
                );
            }
        }
        if levels.len() == before {
            break;
        }
    }
    let mut per_level = BTreeMap::<i32, i32>::new();
    topology
        .primitives
        .iter()
        .map(|primitive| {
            let level = levels.get(&primitive.id).copied().unwrap_or(0);
            let lane = per_level.entry(level).or_insert(0);
            let position = Position::new(level * 4, 0, *lane * 4);
            *lane += 1;
            (primitive.id, position)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn place_junction_instance(
    candidate: &mut ExpandedPhysicalCandidate,
    instance: &crate::compile::fragment_synth::instance_graph::Instance,
    gate: &crate::compile::Gate,
    contributors: &[ContributorSpec],
    at: Anchor,
    facing: CellFacing,
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
            facing,
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
            route_anchor: step(at, geometry::output_direction(facing)),
            allowed_exit: geometry::output_direction(facing),
        },
    );

    let directions = geometry::input_directions(facing);
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

fn route_source_instance(source: PhysicalEndpointId) -> Option<InstanceId> {
    match source {
        PhysicalEndpointId::PrimitiveOutput(primitive) => Some(primitive.instance),
        PhysicalEndpointId::Junction(instance) => Some(instance),
        PhysicalEndpointId::PrimaryInput(_)
        | PhysicalEndpointId::DeclaredOutput(_)
        | PhysicalEndpointId::Landing(_) => None,
    }
}

fn route_target_instance(target: &PendingTarget) -> Option<InstanceId> {
    match target {
        PendingTarget::Connection(ConnectionId::External { instance, .. }, _)
        | PendingTarget::Connection(ConnectionId::Internal { instance, .. }, _) => Some(*instance),
        PendingTarget::DeclaredOutput(_, _) => None,
    }
}

fn route_source_level(source: PhysicalEndpointId, analysis: &SeedPlacementAnalysis) -> u64 {
    route_source_instance(source)
        .and_then(|instance| analysis.nodes.get(&instance))
        .map(|facts| facts.forward_level)
        .unwrap_or(0)
}

fn route_target_level(
    target: &PendingTarget,
    analysis: &SeedPlacementAnalysis,
    output_level: u64,
) -> u64 {
    route_target_instance(target)
        .and_then(|instance| analysis.nodes.get(&instance))
        .map(|facts| facts.forward_level)
        .unwrap_or(output_level)
}

fn input_boundary_slack(instance: InstanceId, analysis: &SeedPlacementAnalysis) -> u64 {
    analysis.nodes.get(&instance).map_or(0, |facts| {
        analysis
            .critical_delay_ticks
            .saturating_sub(facts.tail_ticks)
    })
}

fn output_boundary_slack(instance: InstanceId, analysis: &SeedPlacementAnalysis) -> u64 {
    analysis.nodes.get(&instance).map_or(0, |facts| {
        analysis
            .critical_delay_ticks
            .saturating_sub(facts.head_ticks)
    })
}

fn route_target_slack(
    source: PhysicalEndpointId,
    target: &PendingTarget,
    analysis: &SeedPlacementAnalysis,
) -> u64 {
    let source_instance = route_source_instance(source);
    let target_instance = route_target_instance(target);
    match (source_instance, target_instance) {
        (Some(source), Some(target)) if source == target => 0,
        (Some(source), Some(target)) => analysis
            .edges
            .iter()
            .find(|edge| edge.source == source && edge.sink == target)
            .map(|edge| edge.structural_slack_ticks)
            .unwrap_or(0),
        (None, Some(target)) => input_boundary_slack(target, analysis),
        (Some(source), None) => output_boundary_slack(source, analysis),
        (None, None) => 0,
    }
}

fn route_all(
    candidate: &mut ExpandedPhysicalCandidate,
    router: &dyn PhysicalRouter,
    config: &SearchConfig,
    analysis: &SeedPlacementAnalysis,
    plan_fingerprint: &Fingerprint,
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

    let output_level = analysis
        .nodes
        .values()
        .map(|facts| facts.forward_level)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let obligations = grouped
        .into_iter()
        .map(|(source, pending)| {
            let source_level = route_source_level(source, analysis);
            let targets = pending
                .into_iter()
                .map(|target| {
                    let target_level = route_target_level(&target, analysis, output_level);
                    TargetObligation {
                        structural_slack_ticks: route_target_slack(source, &target, analysis),
                        forward_distance: target_level.saturating_sub(source_level),
                        key: target.key(),
                        target,
                    }
                })
                .collect::<Vec<_>>();
            let structural_slack_ticks = targets
                .iter()
                .map(|target| target.structural_slack_ticks)
                .min()
                .unwrap_or(0);
            let level_span = targets
                .iter()
                .map(|target| target.forward_distance)
                .max()
                .unwrap_or(0);
            RouteObligation {
                source,
                pinned_boundary_escape: matches!(source, PhysicalEndpointId::PrimaryInput(_))
                    && candidate.pin_contracts.contains_key(&source),
                structural_slack_ticks,
                fanout: targets.len(),
                level_span,
                targets,
            }
        })
        .collect();
    let schedule = RouteSchedule::build(obligations);
    let protected = sources
        .values()
        .map(|source| source.route_anchor)
        .chain(
            schedule
                .routes
                .iter()
                .flat_map(|route| route.targets.iter().map(PendingTarget::geometry))
                .map(|geometry| geometry.terminal),
        )
        .collect::<BTreeSet<_>>();

    for (route_index, scheduled_route) in schedule.routes.into_iter().enumerate() {
        let source_id = scheduled_route.source;
        let pending = scheduled_route.targets;
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
        let mut attempt_reservations = reservations.clone();
        if matches!(source_id, PhysicalEndpointId::Junction(_))
            && !attempt_reservations.promote_endpoint_conductor(
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
                if attempt_reservations.get(&side).is_none() {
                    attempt_reservations.reserve(
                        side,
                        PhysicalReservationOwner::KeepOut(route.0),
                        PhysicalReservationKind::KeepOut,
                    );
                }
            }
        }
        let mut tree = match router.route(RouteRequest {
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
            reservations: &attempt_reservations,
            limits: config.router_limits,
        }) {
            Ok(tree) => tree,
            Err(failure) => {
                return Err(SeedError::Routing(seed_routing_failure(
                    route_index,
                    route,
                    source_id,
                    source.route_anchor,
                    &sinks,
                    &failure,
                    plan_fingerprint,
                )))
            }
        };
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
        reserve_route(&mut attempt_reservations, &tree, &protected);
        *reservations = attempt_reservations;
        candidate.routes.insert(route, tree);
    }
    Ok(())
}

fn seed_routing_failure(
    scheduled_index: usize,
    route: RouteId,
    source: PhysicalEndpointId,
    source_at: Anchor,
    sinks: &NonEmptyRouteSinks,
    failure: &RouterFailure,
    plan_fingerprint: &Fingerprint,
) -> SeedRoutingFailure {
    let explicit_sink = match failure {
        RouterFailure::RouterLimitExceeded { sink, .. }
        | RouterFailure::NoLocalRoute { sink, .. }
        | RouterFailure::RingClosure { sink, .. } => Some(*sink),
        RouterFailure::InvalidRequest { sink, .. } | RouterFailure::Refused { sink, .. } => *sink,
        RouterFailure::WrongRepeaterAxis { connection, .. } => sinks
            .as_slice()
            .iter()
            .find(|sink| {
                sink.terminal.target()
                    == Some(crate::compile::routing::RouteTarget::Connection(
                        *connection,
                    ))
            })
            .map(|sink| sink.id),
    };
    let fallback = &sinks.as_slice()[0];
    let sink = explicit_sink.unwrap_or(fallback.id);
    let sink_at = sinks
        .as_slice()
        .iter()
        .find(|candidate| candidate.id == sink)
        .map(|candidate| candidate.anchor)
        .unwrap_or(fallback.anchor);
    let (limit_kind, limit, work_used) = match failure {
        RouterFailure::RouterLimitExceeded {
            kind,
            limit,
            work_used,
            ..
        } => (Some(*kind), Some(*limit), Some(*work_used)),
        _ => (None, None, None),
    };
    SeedRoutingFailure {
        scheduled_index,
        route,
        source,
        sink,
        category: failure.category(),
        limit_kind,
        limit,
        work_used,
        plan_fingerprint: plan_fingerprint.clone(),
        source_at,
        sink_at,
    }
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

fn step_many(at: Anchor, direction: Facing, distance: i32) -> Anchor {
    match direction {
        Facing::North => Anchor {
            z: at.z.saturating_sub(distance),
            ..at
        },
        Facing::South => Anchor {
            z: at.z.saturating_add(distance),
            ..at
        },
        Facing::East => Anchor {
            x: at.x.saturating_add(distance),
            ..at
        },
        Facing::West => Anchor {
            x: at.x.saturating_sub(distance),
            ..at
        },
        Facing::Up => Anchor {
            y: at.y.saturating_add(distance),
            ..at
        },
        Facing::Down => Anchor {
            y: at.y.saturating_sub(distance),
            ..at
        },
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
    use crate::compile::fragment_synth::placement::{
        PreferredInstancePose, SeedPlacementError, SeedPlacementPlan, SeedPlacementRequest,
        SeedPlacer, TopologyAwareSeedPlacer,
    };
    use crate::compile::fragment_synth::services::{DurableSeedEmitter, DurableSeedVerifier};
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::routing::{DurablePhysicalRouter, RealisedRouteTree};
    use crate::compile::Gate;

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

    fn one_typed_sink(route: RouteId) -> NonEmptyRouteSinks {
        NonEmptyRouteSinks::new(vec![RouteSink {
            id: RoutedSinkId { route, ordinal: 0 },
            endpoint: PhysicalEndpointId::DeclaredOutput(PortId(0)),
            anchor: Anchor { x: 9, y: 2, z: 7 },
            allowed_entry: Facing::West,
            terminal: TerminalContract::Sink {
                target: crate::compile::routing::RouteTarget::DeclaredOutput(PortId(0)),
                support: Anchor { x: 10, y: 2, z: 7 },
                requirement: TerminalRequirement::Exact(
                    crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                ),
            },
        }])
        .unwrap()
    }

    #[test]
    fn boundary_route_slack_counts_instance_delay_once() {
        let instance = InstanceId(0);
        let analysis = SeedPlacementAnalysis {
            order: vec![instance],
            nodes: BTreeMap::from([(
                instance,
                crate::compile::fragment_synth::placement::NodeFacts {
                    predecessors: Vec::new(),
                    successors: Vec::new(),
                    forward_level: 0,
                    reverse_level: 0,
                    head_ticks: 4,
                    tail_ticks: 8,
                },
            )]),
            edges: Vec::new(),
            critical_delay_ticks: 10,
        };
        let geometry = TargetGeometry {
            terminal: Anchor { x: 4, y: 1, z: 4 },
            allowed_entry: Facing::West,
            support: Anchor { x: 5, y: 1, z: 4 },
            requirement: TerminalRequirement::DirectedDust,
        };
        let input_target = PendingTarget::Connection(
            ConnectionId::External {
                instance,
                input_index: 0,
            },
            geometry,
        );
        let output_target = PendingTarget::DeclaredOutput(PortId(0), geometry);
        let output_source = PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
            instance,
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        });

        assert_eq!(
            route_target_slack(
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                &input_target,
                &analysis,
            ),
            2,
        );
        assert_eq!(
            route_target_slack(output_source, &output_target, &analysis),
            6,
        );
    }

    #[test]
    fn router_limit_failure_keeps_exact_schedule_geometry_and_cap_work() {
        let route = RouteId(3);
        let source = PhysicalEndpointId::PrimaryInput(PortId(2));
        let source_at = Anchor { x: 2, y: 2, z: 7 };
        let sinks = one_typed_sink(route);
        let plan_fingerprint = canonical_fingerprint(b"typed-limit-plan");
        let failure = RouterFailure::RouterLimitExceeded {
            route,
            source,
            sink: RoutedSinkId { route, ordinal: 0 },
            kind: crate::compile::routing::RouterLimitKind::QueueEntries,
            limit: 262_144,
            work_used: 262_145,
        };

        let evidence = seed_routing_failure(
            7,
            route,
            source,
            source_at,
            &sinks,
            &failure,
            &plan_fingerprint,
        );

        assert_eq!(evidence.scheduled_index, 7);
        assert_eq!(evidence.route, route);
        assert_eq!(evidence.source, source);
        assert_eq!(evidence.sink, RoutedSinkId { route, ordinal: 0 });
        assert_eq!(
            evidence.category,
            crate::compile::routing::RouterRefusalCategory::InvalidRequest
        );
        assert_eq!(evidence.limit, Some(262_144));
        assert_eq!(evidence.work_used, Some(262_145));
        assert_eq!(
            evidence.limit_kind,
            Some(crate::compile::routing::RouterLimitKind::QueueEntries)
        );
        assert_eq!(evidence.plan_fingerprint, plan_fingerprint);
        assert_eq!(evidence.source_at, source_at);
        assert_eq!(evidence.sink_at, Anchor { x: 9, y: 2, z: 7 });
    }

    #[test]
    fn ring_closure_failure_has_physical_category_without_cap_work() {
        let route = RouteId(4);
        let source = PhysicalEndpointId::Junction(InstanceId(6));
        let source_at = Anchor { x: 3, y: 1, z: 5 };
        let sinks = one_typed_sink(route);
        let plan_fingerprint = canonical_fingerprint(b"typed-ring-plan");
        let failure = RouterFailure::RingClosure {
            route,
            source,
            sink: RoutedSinkId { route, ordinal: 0 },
            repeater: Anchor { x: 7, y: 1, z: 5 },
            charged: vec![Anchor { x: 8, y: 1, z: 5 }],
        };

        let evidence = seed_routing_failure(
            2,
            route,
            source,
            source_at,
            &sinks,
            &failure,
            &plan_fingerprint,
        );

        assert_eq!(
            evidence.category,
            crate::compile::routing::RouterRefusalCategory::PhysicalInvariant
        );
        assert_eq!(evidence.limit, None);
        assert_eq!(evidence.work_used, None);
        assert_eq!(evidence.sink_at, Anchor { x: 9, y: 2, z: 7 });
        assert_eq!(evidence.plan_fingerprint, plan_fingerprint);
    }

    struct LiteralSeedPlacer {
        calls: Cell<u32>,
    }

    impl SeedPlacer for LiteralSeedPlacer {
        fn plan(
            &self,
            _request: SeedPlacementRequest<'_>,
        ) -> Result<SeedPlacementPlan, SeedPlacementError> {
            self.calls.set(self.calls.get() + 1);
            Ok(SeedPlacementPlan {
                instances: BTreeMap::from([(
                    InstanceId(0),
                    PreferredInstancePose {
                        preferred_origin: Anchor { x: 91, y: 7, z: 83 },
                        facing: CellFacing::NORTH,
                    },
                )]),
                automatic_inputs: BTreeMap::from([(PortId(0), Anchor { x: 71, y: 7, z: 83 })]),
                automatic_outputs: BTreeMap::from([(
                    PortId(0),
                    Anchor {
                        x: 111,
                        y: 7,
                        z: 83,
                    },
                )]),
                fingerprint: canonical_fingerprint(b"literal-seed-plan"),
            })
        }
    }

    #[test]
    fn injected_seed_plan_controls_materialised_pose_and_automatic_boundaries_once() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let placer = LiteralSeedPlacer {
            calls: Cell::new(0),
        };

        let certified = compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &placer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();

        let candidate = certified.candidate();
        assert_eq!(placer.calls.get(), 1);
        let views = candidate.compatibility_views(&netlist).unwrap();
        assert_eq!(
            views.input_positions,
            BTreeMap::from([("a".to_string(), (71, 7, 83))])
        );
        assert_eq!(
            views.output_positions,
            BTreeMap::from([("y".to_string(), (111, 7, 83))])
        );
        let primitive = PrimitiveId {
            instance: InstanceId(0),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        };
        assert_eq!(candidate.placements[&primitive].facing, CellFacing::NORTH);
        assert_eq!(
            candidate.placements[&primitive].anchor,
            Anchor { x: 91, y: 7, z: 83 }
        );
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
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
    }

    #[test]
    fn one_instance_variant_rebuilds_and_certifies_the_requested_facing_and_offset() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let baseline = build(&netlist).unwrap();
        let variant = compile_sparse_seed_variant_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
            &SeedVariant {
                placements: BTreeMap::from([(
                    InstanceId(0),
                    InstancePlacementOverride {
                        facing: CellFacing::NORTH,
                        dx: 4,
                        dz: -3,
                    },
                )]),
                ..SeedVariant::default()
            },
        )
        .unwrap();
        let primitive = PrimitiveId {
            instance: InstanceId(0),
            node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
        };
        let original = &baseline.candidate().placements[&primitive];
        let changed = &variant.candidate().placements[&primitive];

        assert_eq!(changed.facing, CellFacing::NORTH);
        assert_eq!(changed.anchor.x, original.anchor.x + 4);
        assert_eq!(changed.anchor.z, original.anchor.z - 3);
        assert_ne!(
            variant.metrics().candidate_fingerprint,
            baseline.metrics().candidate_fingerprint
        );
    }

    #[test]
    fn a_combinational_duplicate_is_independently_placed_routed_and_certified() {
        let netlist = Netlist {
            inputs: vec!["a".into()],
            outputs: vec!["left".into(), "right".into()],
            gates: vec![
                Gate::nor("shared", &["a"]),
                Gate::nor("left", &["shared"]),
                Gate::nor("right", &["shared"]),
            ],
        };
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let duplicate_sink =
            crate::compile::fragment_synth::instance_graph::PhysicalSink::InstanceInput {
                instance: InstanceId(2),
                input_index: 0,
            };
        let certified = compile_sparse_seed_variant_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &DurablePhysicalRouter,
                emitter: &DurableSeedEmitter,
                verifier: &DurableSeedVerifier,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
            &SeedVariant {
                duplicates: vec![
                    crate::compile::fragment_synth::instance_graph::DuplicateRequest {
                        canonical: InstanceId(0),
                        ordinal: 1,
                        sinks: BTreeSet::from([duplicate_sink]),
                    },
                ],
                ..SeedVariant::default()
            },
        )
        .unwrap();

        assert_eq!(certified.candidate().instances.instances.len(), 4);
        assert!(certified
            .candidate()
            .instances
            .instances
            .iter()
            .any(|instance| {
                instance.role
                    == crate::compile::fragment_synth::instance_graph::InstanceRole::Duplicate {
                        ordinal: 1,
                    }
            }));
        certified.candidate().validate_shape().unwrap();
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
                placer: &TopologyAwareSeedPlacer,
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
                placer: &TopologyAwareSeedPlacer,
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
                placer: &TopologyAwareSeedPlacer,
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
                placer: &TopologyAwareSeedPlacer,
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
