#![allow(dead_code)] // Task 9 is the first production caller of this Task-8 seam.

//! Independent deterministic sparse-seed construction.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::emission::EmissionError;
use crate::compile::fragment_synth::blocks::CompiledBlock;
use crate::compile::fragment_synth::candidate::{
    endpoint_for_driver, BoundaryPlacement, CandidateError, ConnectionBinding,
    ExpandedPhysicalCandidate, PlacedBlock, PrimitivePlacement, RealisedJunction,
    VerifiedObservation,
};
use crate::compile::fragment_synth::certification::{
    CandidateCertificationError, CertifiedCandidate, ExpandedCandidateCertifier,
};
use crate::compile::fragment_synth::channel_layout::{
    plan_channel_layout, ChannelLayout, ChannelLayoutError, NetGeometry,
};
use crate::compile::fragment_synth::config::{CertificationConfig, SearchConfig};
use crate::compile::fragment_synth::identity::{
    ConnectionId, ImplementationKey, InstanceId, ObservationId, ObservationSite,
    PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId, TopologyNodeId,
};
use crate::compile::fragment_synth::instance_graph::{
    DuplicateRequest, InstanceGraph, PhysicalDriver, PhysicalSink, SynthesisError,
};
use crate::compile::fragment_synth::placement::{
    analyse_instance_dag, BlockFacts, SeedPlacementAnalysis, SeedPlacementPlan,
    SeedPlacementRequest, SeedPlacer,
};
use crate::compile::fragment_synth::placement::{LateralWindow, LayoutRepair, PlacementFrame};
use crate::compile::fragment_synth::realise::{
    CertificationError as PhysicalCertificationError, ExpandedAdapterError,
};
use crate::compile::fragment_synth::relocate::Offset;
use crate::compile::fragment_synth::route_schedule::{
    RouteObligation, RouteSchedule, TargetObligation,
};
use crate::compile::fragment_synth::topology::{
    ConnectionSource, ConnectionTarget, ContributorSpec, OutputSpec,
};
use crate::compile::geometry::{self, Anchor, CellFacing};
use crate::compile::metrics::Fingerprint;
use crate::compile::physical::{self, PortKind};
use crate::compile::planner::{IoFootprint, PortPlacements, PortRole};
use crate::compile::routing::{
    DelayedComponent, DelayedOwner, NonEmptyRouteSinks, PhysicalReservationKind,
    PhysicalReservationOwner, PhysicalReservations, PhysicalRouter, ReservationStore,
    RouteEndpoint, RouteSink, RouterFailure, RouterLimitKind, RouterRefusalCategory,
    TerminalContract, TerminalRequirement, TransactionalRouteRequest,
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
    pub certifier: &'a dyn ExpandedCandidateCertifier,
    pub search_config: &'a SearchConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InstancePlacementOverride {
    pub facing: CellFacing,
    pub dx: i32,
    pub dz: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockPlacementOffset {
    pub(crate) dx: i32,
    pub(crate) dz: i32,
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
    #[error("channel routing plan failed: {0}")]
    ChannelLayout(#[from] ChannelLayoutError),
    #[error("seed repair exhausted after {attempts_used} attempts: {refusal}")]
    SeedExhausted {
        attempts_used: u64,
        refusal: Box<SeedError>,
    },
    #[error("physical placement at {at:?} overlaps another seed component")]
    PlacementCollision { at: Anchor },
    /// A post-plan move walked a body off the board a complete pin set
    /// drew.  Only a site with no search of its own reports this: where
    /// shells exist, an off-board anchor is skipped like an occupied one
    /// and the existing [`SeedError::PlacementExhausted`] ends the search.
    #[error("physical placement at {at:?} stands off the pinned IO footprint")]
    PlacementOutsideIoFootprint { at: Anchor },
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
    // `#[source]` rather than `#[from]`: the conversion is hand-written below
    // so the certifier's physical half lands in the outer variants above.
    #[error("complete candidate certification failed: {0}")]
    Certification(#[source] CandidateCertificationError),
    #[error("typed identity width exceeded")]
    IdentityOverflow,
    #[error("seed topology is internally incomplete: {0}")]
    Incomplete(&'static str),
    #[error(
        "block instance {block:?} names compiled block {index}, which the parent did not supply"
    )]
    UnknownBlock { block: InstanceId, index: u32 },
    #[error("the parent placing block {block:?} did not settle on the direct east frame")]
    BlockFrameTurned { block: InstanceId },
    #[error("block {block:?} has a footprint too large to measure in placement coordinates")]
    BlockTooWide { block: InstanceId },
    #[error("flat union failed: {0}")]
    Union(String),
}

impl From<CandidateCertificationError> for SeedError {
    /// The certifier owns the only adapter/emission/physical-verification
    /// transaction, so its physical half is unwrapped back into the same outer
    /// variants the seed used to raise itself.  Callers -- including the
    /// proposal stream's terminal classification -- keep seeing the exact
    /// typed payload and category they saw when the seed ran that transaction
    /// a second time.  Every other certification failure stays a certification
    /// failure.
    fn from(error: CandidateCertificationError) -> Self {
        match error {
            CandidateCertificationError::Physical(PhysicalCertificationError::Adapter(adapter)) => {
                Self::Adapter(adapter)
            }
            CandidateCertificationError::Physical(PhysicalCertificationError::Emission(
                emission,
            )) => Self::Emission(emission.0),
            CandidateCertificationError::Physical(PhysicalCertificationError::Physical(
                physical,
            )) => Self::Verification(physical),
            other => Self::Certification(other),
        }
    }
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

/// The compiled blocks a parent may stamp, indexed exactly the way
/// [`BlockInstance::block`](crate::compile::fragment_synth::instance_graph::BlockInstance)
/// indexes them.
#[derive(Clone, Copy)]
pub(crate) struct ParentBlocks<'a> {
    pub compiled: &'a [CompiledBlock],
}

impl ParentBlocks<'static> {
    /// A parent that stamps nothing -- what every flat design passes.
    pub(crate) const fn none() -> Self {
        Self { compiled: &[] }
    }
}

/// A parent design planned as far as its routes, with the block bodies in
/// place as opaque reservations. Emission, verification and certification
/// have deliberately not run: stamping each block's real contents into the
/// candidate is a later step, and until it has, the candidate's block
/// bodies are placeholder placements rather than owned topology.
pub(crate) struct PlannedParent {
    /// The parent's own gates, boundaries and routes, plus one placeholder
    /// placement per block holding that block's translated body.
    pub candidate: ExpandedPhysicalCandidate,
    /// Block instance -> the translation from block-local to parent
    /// coordinates that [`relocate::translate`](crate::compile::fragment_synth::relocate::translate)
    /// must apply to stamp it.
    pub block_offsets: BTreeMap<InstanceId, Offset>,
    /// The planning netlist the parent was planned from.
    pub lowered: Netlist,
}

/// Everything the parent's planning stages need to know about the blocks in
/// its graph, derived in ONE walk over `graph.blocks`.
///
/// The placer's footprint facts, the level analysis's delays and the
/// geometry `place_blocks` stamps all come out of this single resolution,
/// so no two of them can disagree about which compiled block a
/// `BlockInstance` names, or about how wide it is.
struct ResolvedBlocks<'a> {
    compiled: BTreeMap<InstanceId, &'a CompiledBlock>,
    facts: BTreeMap<InstanceId, BlockFacts>,
    delays: BTreeMap<InstanceId, u64>,
}

impl<'a> ResolvedBlocks<'a> {
    fn resolve(graph: &InstanceGraph, blocks: ParentBlocks<'a>) -> Result<Self, SeedError> {
        let mut resolved = Self {
            compiled: BTreeMap::new(),
            facts: BTreeMap::new(),
            delays: BTreeMap::new(),
        };
        for instance in &graph.blocks {
            let index = usize::try_from(instance.block).map_err(|_| SeedError::IdentityOverflow)?;
            let compiled = blocks
                .compiled
                .get(index)
                .ok_or_else(|| SeedError::UnknownBlock {
                    block: instance.id,
                    index: instance.block,
                })?;
            let span = |low: i32, high: i32| {
                high.checked_sub(low)
                    .and_then(|span| span.checked_add(1))
                    .filter(|span| *span > 0)
            };
            let (Some(width), Some(depth), Some(height)) = (
                span(compiled.bounds.min.x, compiled.bounds.max.x),
                span(compiled.bounds.min.z, compiled.bounds.max.z),
                span(compiled.bounds.min.y, compiled.bounds.max.y),
            ) else {
                return Err(SeedError::BlockTooWide { block: instance.id });
            };
            resolved.facts.insert(
                instance.id,
                BlockFacts {
                    width,
                    depth,
                    height,
                    delay_ticks: compiled.delay.0,
                },
            );
            resolved.delays.insert(instance.id, compiled.delay.0);
            resolved.compiled.insert(instance.id, compiled);
        }
        Ok(resolved)
    }

    fn compiled(&self, block: InstanceId) -> Result<&'a CompiledBlock, SeedError> {
        self.compiled
            .get(&block)
            .copied()
            .ok_or(SeedError::Incomplete("resolved compiled block"))
    }
}

/// Plans a parent design that stamps compiled blocks: places the parent's
/// own gates and boundaries, reserves each block's body where the placer
/// put it, and routes the parent's wires to the blocks' port cells.
///
/// This is [`SparseSeedBuilder::build_variant`] stopped after routing --
/// the same channel-widening repair loop, the same planning half -- because
/// a parent's candidate is not finished until its blocks' real contents
/// have been stamped into it.
pub(crate) fn plan_parent_with_services(
    input: SeedInput<'_>,
    services: SeedServices<'_>,
    graph: InstanceGraph,
    blocks: ParentBlocks<'_>,
    placements: &BTreeMap<InstanceId, InstancePlacementOverride>,
    block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>,
) -> Result<PlannedParent, SeedError> {
    if let Some(provenance) = input.source_provenance {
        if provenance.len() != input.lowered.gates.len() {
            return Err(SeedError::ProvenanceWidth {
                expected: input.lowered.gates.len(),
                actual: provenance.len(),
            });
        }
    }
    let (candidate, block_offsets) = with_channel_widening(services.search_config, |repairs| {
        SparseSeedBuilder::plan_attempt(
            &input,
            &services,
            placements,
            block_placements,
            graph.clone(),
            blocks,
            repairs,
        )
    })?;
    Ok(PlannedParent {
        candidate,
        block_offsets,
        lowered: input.lowered.clone(),
    })
}

/// The finishing half on its own: shape and ownership validation, emission,
/// verification and certification, over a candidate somebody else built.
///
/// [`plan_parent_with_services`] deliberately stops before this, because a
/// parent's candidate is not a finished design until its blocks have been
/// dissolved into it (`union::union_candidate`). Once they have, the result
/// is an ordinary flat candidate and goes through exactly the same finish
/// as a flat compile -- no branch here knows a block ever existed.
pub(crate) fn certify_planned(
    candidate: ExpandedPhysicalCandidate,
    lowered: &Netlist,
    services: SeedServices<'_>,
) -> Result<CertifiedCandidate, SeedError> {
    let input = SeedInput {
        lowered,
        source_provenance: None,
        pins: None,
    };
    SparseSeedBuilder::finish_attempt(candidate, &input, &services)
}

/// Bounded fresh-candidate repair: every attempt rebuilds the candidate
/// from scratch with the accumulated canonical repairs.  The only repair
/// the channel plan asks for is a wider channel, and the requested width
/// grows strictly, so the loop ends by the cap.
fn with_channel_widening<T>(
    config: &SearchConfig,
    mut attempt: impl FnMut(&[LayoutRepair]) -> Result<T, SeedError>,
) -> Result<T, SeedError> {
    let cap = config.max_seed_backtracks;
    let mut repairs = Vec::<LayoutRepair>::new();
    let mut attempts = 0u64;
    let mut widenings = 0u64;
    loop {
        attempts += 1;
        match attempt(&repairs) {
            Ok(built) => return Ok(built),
            Err(SeedError::ChannelLayout(ChannelLayoutError::ChannelTooNarrow {
                level,
                needed,
                available,
                ..
            })) => {
                // The placer's width for this channel and the free span the
                // plan measured differ by the endpoint cells beside the
                // macros; a repeated repair grows by the measured shortfall
                // so the loop is monotone.
                let previous = repairs.iter().find_map(|repair| match repair {
                    LayoutRepair::WidenChannel {
                        level: known,
                        width,
                    } if *known == level => Some(*width),
                    _ => None,
                });
                let width = match previous {
                    Some(previous) => previous + (needed - available).max(1),
                    None => needed + CHANNEL_ENDPOINT_CELLS,
                };
                let already = previous.is_some_and(|previous| width <= previous);
                // A channel that keeps asking for more room after this
                // many widenings is not short of lanes; something else
                // is wrong with the layout.
                widenings += 1;
                if already || attempts >= cap || widenings > MAX_CHANNEL_WIDENINGS {
                    return Err(SeedError::SeedExhausted {
                        attempts_used: attempts,
                        refusal: Box::new(SeedError::ChannelLayout(
                            ChannelLayoutError::ChannelTooNarrow {
                                channel: 0,
                                level,
                                available: 0,
                                lanes: 0,
                                needed,
                            },
                        )),
                    });
                }
                repairs.retain(|repair| {
                    !matches!(repair, LayoutRepair::WidenChannel { level: known, .. } if *known == level)
                });
                repairs.push(LayoutRepair::WidenChannel { level, width });
                repairs.sort();
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceGeometry {
    pub(crate) route_anchor: Anchor,
    pub(crate) allowed_exit: Facing,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TargetGeometry {
    pub(crate) terminal: Anchor,
    pub(crate) allowed_entry: Facing,
    pub(crate) support: Anchor,
    pub(crate) requirement: TerminalRequirement,
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
pub(crate) enum PendingTarget {
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

    pub(crate) fn geometry(&self) -> TargetGeometry {
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

        with_channel_widening(services.search_config, |repairs| {
            Self::build_attempt(&input, &services, variant, instances.clone(), repairs)
        })
    }

    /// The whole pipeline: plan the layout and routes, then finish the
    /// candidate off.  Exactly the two halves composed, and nothing else.
    fn build_attempt(
        input: &SeedInput<'_>,
        services: &SeedServices<'_>,
        variant: &SeedVariant,
        instances: InstanceGraph,
        repairs: &[LayoutRepair],
    ) -> Result<CertifiedCandidate, SeedError> {
        let (candidate, _) = Self::plan_attempt(
            input,
            services,
            &variant.placements,
            &BTreeMap::new(),
            instances,
            ParentBlocks::none(),
            repairs,
        )?;
        Self::finish_attempt(candidate, input, services)
    }

    /// The planning half: everything through `route_all`.  Returns the
    /// routed candidate and where each block landed.
    fn plan_attempt(
        input: &SeedInput<'_>,
        services: &SeedServices<'_>,
        placements: &BTreeMap<InstanceId, InstancePlacementOverride>,
        block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>,
        instances: InstanceGraph,
        blocks: ParentBlocks<'_>,
        repairs: &[LayoutRepair],
    ) -> Result<(ExpandedPhysicalCandidate, BTreeMap<InstanceId, Offset>), SeedError> {
        let timing = std::env::var_os("REDA_PHASE_TIMING").is_some();
        let mut phase_started = std::time::Instant::now();
        let phase = |name: &str, started: &mut std::time::Instant| {
            if timing {
                eprintln!("PHASE {name} {}", started.elapsed().as_millis());
            }
            *started = std::time::Instant::now();
        };
        let mut candidate =
            ExpandedPhysicalCandidate::empty(instances, input.pins.cloned().unwrap_or_default());
        candidate.bind_pin_contracts(input.lowered)?;
        crate::compile::planner::validate_port_placements(input.lowered, &candidate.pins)
            .map_err(SeedError::InvalidPins)?;
        // One resolution of the parent's blocks feeds both the level
        // analysis (delays) and the placer (footprints), so the two views
        // of a block can never drift apart.
        let resolved = ResolvedBlocks::resolve(&candidate.instances, blocks)?;
        let placement_analysis = analyse_instance_dag(&candidate.instances, &resolved.delays)
            .map_err(|_| SeedError::Incomplete("seed placement analysis"))?;
        let placement_plan = services
            .placer
            .plan_with_repairs(
                SeedPlacementRequest {
                    graph: &candidate.instances,
                    analysis: &placement_analysis,
                    pins: &candidate.pin_contracts,
                    block_facts: &resolved.facts,
                },
                repairs,
            )
            .map_err(|_| SeedError::Incomplete("seed placement plan"))?;
        phase("placement", &mut phase_started);
        // A block is a fixed east-facing layout: its inputs sit on its west
        // face and its outputs on its east one.  A parent that settled on a
        // turned frame would have to route backwards into every block, so
        // refuse rather than plan something unroutable.
        if let Some(block) = candidate.instances.blocks.first() {
            if placement_plan.frame.forward != Facing::East {
                return Err(SeedError::BlockFrameTurned { block: block.id });
            }
        }
        // Every later stage measures levels in the plan's folded analysis.
        let placement_analysis = placement_plan.analysis.clone();
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
        let block_offsets = place_blocks(
            &mut candidate,
            &resolved,
            &placement_plan,
            plan_translation,
            block_placements,
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
            placements,
            &mut occupied,
            &mut sources,
            &mut targets,
        )?;

        let sockets = assign_torch_sockets(
            &candidate,
            &placement_analysis,
            placement_plan.frame,
            &sources,
        )?;
        refresh_primitive_targets(&candidate, &sockets, &mut targets)?;
        let mut reservations = reservations_for_components(&candidate)?;
        // Every macro is standing by now and no route has started, so this is
        // the one moment a terminal's tunnel can be both checked against what
        // was placed and closed against what is about to be routed.
        reserve_terminal_tunnels(&candidate, input.lowered, &mut reservations)?;
        reserve_route_endpoints(&mut reservations, &candidate, &sources, &targets);
        let route_started = std::time::Instant::now();
        let route_result = route_all(
            &mut candidate,
            services.router,
            services.search_config,
            &placement_analysis,
            placement_plan.frame,
            placement_plan.window,
            &placement_plan.fingerprint,
            &sources,
            &targets,
            &sockets,
            &mut reservations,
        );
        if timing {
            eprintln!(
                "PHASE route_nets {:.3}",
                route_started.elapsed().as_secs_f64() * 1_000.0
            );
        }
        route_result?;
        phase("layout+routing", &mut phase_started);
        Ok((candidate, block_offsets))
    }

    /// The finishing half: shape and ownership validation, then the one
    /// certification transaction -- the only place adaptation, emission and
    /// durable physical verification happen.
    fn finish_attempt(
        candidate: ExpandedPhysicalCandidate,
        input: &SeedInput<'_>,
        services: &SeedServices<'_>,
    ) -> Result<CertifiedCandidate, SeedError> {
        let timing = std::env::var_os("REDA_PHASE_TIMING").is_some();
        let mut phase_started = std::time::Instant::now();
        let phase = |name: &str, started: &mut std::time::Instant| {
            if timing {
                eprintln!("PHASE {name} {}", started.elapsed().as_millis());
            }
            *started = std::time::Instant::now();
        };
        candidate.validate_shape()?;
        candidate.validate_physical_ownership()?;
        // The board, once, after the candidate is known to be well shaped
        // and singly owned and before anything is emitted from it.  Every
        // design passes here -- a flat compile through `build_attempt` and
        // a hierarchical one through `certify_planned`, once its blocks
        // have been dissolved in -- so there is exactly one place a cell
        // standing off a completely pinned board is caught.
        candidate.validate_io_footprint(input.lowered)?;

        let certification = CertificationConfig::from_search(services.search_config);
        let certified = services
            .certifier
            .certify(candidate, input.lowered, services.library, &certification)
            .map_err(SeedError::from);
        phase("certify", &mut phase_started);
        certified
    }
}

/// Holds every complete pin's terminal tunnel against the board that was just
/// placed, then closes what is left of it before anything is routed.
///
/// The tunnel is the promise a complete pin set makes: two cells deep along
/// the signal, one cell of halo around both, and nothing REDA owns inside it
/// but the terminal's own hardware. `place_boundaries`, `place_blocks` and
/// `place_instances` have all run, so a macro that took one of those cells is
/// visible here and is refused under the port's own name -- the same
/// [`PinRefusal::ClearanceConflict`](crate::compile::planner::PinRefusal) the
/// planner's door raises when two pins want one cell, because it is the same
/// promise being broken, only by a body instead of by another pin.
///
/// The exemptions are exactly the hardware the terminal *is*: the handover
/// and the cell it stands on. An input's boundary has already built both; an
/// output's route still has to, so they are left unreserved rather than
/// merely unchecked. The caller's own cell is not exempt -- it ships as air --
/// and the first internal net cell is a step further in, outside the tunnel,
/// so the net can still leave.
///
/// A partial or unpinned set draws no board at all
/// ([`IoFootprint::from_complete`]), so this is a no-op for it and its
/// terminals keep the older five-neighbour, signal-only clearance exactly.
fn reserve_terminal_tunnels(
    candidate: &ExpandedPhysicalCandidate,
    netlist: &Netlist,
    reservations: &mut PhysicalReservations,
) -> Result<(), SeedError> {
    let Some(footprint) = IoFootprint::from_complete(
        netlist.inputs.len() + netlist.outputs.len(),
        candidate.pin_contracts.values().map(|pin| pin.at),
    ) else {
        return Ok(());
    };

    // The same three collections `reservations_for_components` reserves from:
    // everything REDA has put on the board so far. Routes have not started,
    // so there is nothing else to ask about.
    let bodies = candidate
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
        .map(|block| block.at)
        .collect::<BTreeSet<_>>();

    for (&endpoint, pin) in &candidate.pin_contracts {
        let role = match endpoint {
            PhysicalEndpointId::PrimaryInput(_) => PortRole::Input,
            PhysicalEndpointId::DeclaredOutput(_) => PortRole::Output,
            // `bind_pin_contracts` writes no other key and
            // `validate_pin_contracts` refuses one that appeared anyway; say
            // so here rather than guess a role for it.
            _ => return Err(SeedError::Incomplete("pinned terminal role")),
        };
        let handover = pin.handover(role);
        let required = [
            handover,
            Anchor {
                y: handover.y - 1,
                ..handover
            },
        ];
        // Clipped, not raw: what lies off the board or under the world is the
        // caller's, and the planner's door compared the same cells.
        let tunnel = crate::compile::planner::effective_tunnel(*pin, role, footprint);
        for &cell in &tunnel {
            if required.contains(&cell) {
                continue;
            }
            if bodies.contains(&cell) {
                let port = candidate
                    .pin_name_bindings
                    .iter()
                    .find(|(_, bound)| **bound == endpoint)
                    .map(|(name, _)| name.clone())
                    .ok_or(SeedError::Incomplete("pinned port name"))?;
                return Err(SeedError::InvalidPins(
                    crate::compile::planner::PlannerError::InvalidPortPin {
                        port,
                        at: pin.at,
                        refusal: crate::compile::planner::PinRefusal::ClearanceConflict {
                            other_port_cell: cell,
                        },
                    },
                ));
            }
            // Keep-out stops a route standing *in* the cell, and that is
            // enough wherever the cell above is kept out too. Where it is
            // not -- the roof of the tunnel, whose upper neighbour is
            // ordinary space -- a conductor up there would read this cell as
            // its floor, fail to claim one, and leave dust on nothing. Air
            // says the cell is empty, which refuses that conductor outright.
            let sealed_above = match cell.y.checked_add(1) {
                None => true,
                Some(y) => {
                    let above = Anchor { y, ..cell };
                    tunnel.contains(&above) && !required.contains(&above)
                }
            };
            reservations.reserve(
                cell,
                PhysicalReservationOwner::Endpoint(endpoint),
                if sealed_above {
                    PhysicalReservationKind::KeepOut
                } else {
                    PhysicalReservationKind::MandatoryAir
                },
            );
        }
    }
    Ok(())
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
            reservations.reserve(
                target.terminal,
                PhysicalReservationOwner::Endpoint(sink_endpoint(sink)),
                PhysicalReservationKind::KeepOut,
            );
        }
    }
    // Nothing may stand directly above a terminal, its access cell, or a sink
    // support: dust up there needs a floor exactly where the terminal goes,
    // and a powered support would drive it straight back into the terminal's
    // own input.
    for (&endpoint, source) in sources {
        for cell in [
            source.route_anchor,
            step(source.route_anchor, source.allowed_exit),
        ] {
            reserve_above(reservations, cell, endpoint);
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

/// Reservation owner tag for the closed channel layers.
const CHANNEL_LAYER_OWNER: u32 = u32::MAX;
/// Channel widenings the repair loop grants before it gives up.
const MAX_CHANNEL_WIDENINGS: u64 = 16;
/// Forward cells of a channel taken by the source anchor and the sink
/// terminal beside their macros; the placer's width includes them, the
/// channel plan's free span does not.
const CHANNEL_ENDPOINT_CELLS: i32 = 2;

fn sink_endpoint(sink: PhysicalSink) -> PhysicalEndpointId {
    match sink {
        PhysicalSink::InstanceInput {
            instance,
            input_index,
        } => PhysicalEndpointId::Landing(ConnectionId::External {
            instance,
            input_index,
        }),
        PhysicalSink::DeclaredOutput(port) => PhysicalEndpointId::DeclaredOutput(port),
    }
}

fn reserve_above(
    reservations: &mut PhysicalReservations,
    cell: Anchor,
    endpoint: PhysicalEndpointId,
) {
    let above = Anchor {
        y: cell.y + 1,
        ..cell
    };
    if reservations.get(&above).is_none() {
        reservations.reserve(
            above,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );
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

/// Reserves each stamped block's body where the placer put it, and
/// registers its ports as parent route endpoints.
///
/// A block is opaque to the parent: nothing in the parent's own pipeline
/// understands its topology, and the parent must simply not route through
/// it.  The body therefore enters the candidate as ONE placeholder
/// `PrimitivePlacement` per block, holding every cell the compiled block
/// owns, translated.  That is the shape both `channel_layout`'s occupancy
/// and `reservations_for_components` already read, so the block's footprint
/// closes the channel columns it stands in and closes the cell above every
/// one of its own blocks, with no change to either.
///
/// Two cells per port are deliberately NOT in the placeholder:
///   * an input's lever cell, which the parent's route terminal claims for
///     the repeater that drives the block's own root dust one cell east of
///     it (the stone under the lever stays: it is the repeater's floor);
///   * an output's lamp cell, which the parent's route root claims, driven
///     from the west by the block's own output repeater.
fn place_blocks(
    candidate: &mut ExpandedPhysicalCandidate,
    resolved: &ResolvedBlocks<'_>,
    plan: &SeedPlacementPlan,
    plan_translation: PlanTranslation,
    block_placements: &BTreeMap<InstanceId, BlockPlacementOffset>,
    occupied: &mut BTreeSet<Anchor>,
    sources: &mut BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<BTreeMap<InstanceId, Offset>, SeedError> {
    let mut offsets = BTreeMap::new();
    let block_ids = candidate
        .instances
        .blocks
        .iter()
        .map(|block| block.id)
        .collect::<Vec<_>>();
    for block in block_ids {
        let compiled = resolved.compiled(block)?;
        let pose = plan
            .instances
            .get(&block)
            .copied()
            .ok_or(SeedError::Incomplete("planned block pose"))?;
        let block_offset = block_placements.get(&block).copied();
        let origin = plan_translation.apply(Anchor {
            x: pose
                .preferred_origin
                .x
                .saturating_add(block_offset.map_or(0, |offset| offset.dx)),
            z: pose
                .preferred_origin
                .z
                .saturating_add(block_offset.map_or(0, |offset| offset.dz)),
            ..pose.preferred_origin
        });
        // The placer's origin is the block's own minimum corner, and the
        // parent's ground row sits one above the floor row every unpinned
        // layout (a block's included) puts its supports on.
        let offset = Offset {
            dx: origin.x - compiled.bounds.min.x,
            dy: (plan.frame.origin.y - 1) - compiled.bounds.min.y,
            dz: origin.z - compiled.bounds.min.z,
        };
        offsets.insert(block, offset);

        let mut ports = BTreeSet::new();
        for (index, name) in compiled.lowered.outputs.iter().enumerate() {
            let port = compiled
                .outputs
                .get(name)
                .ok_or(SeedError::Incomplete("compiled block output port"))?;
            let lamp = shift(port.cell, offset);
            ports.insert(lamp);
            sources.insert(
                PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                    instance: block,
                    node: TopologyNodeId(
                        u16::try_from(index).map_err(|_| SeedError::IdentityOverflow)?,
                    ),
                }),
                SourceGeometry {
                    route_anchor: lamp,
                    allowed_exit: Facing::East,
                },
            );
        }
        for (index, name) in compiled.lowered.inputs.iter().enumerate() {
            let port = compiled
                .inputs
                .get(name)
                .ok_or(SeedError::Incomplete("compiled block input port"))?;
            let lever = shift(port.cell, offset);
            ports.insert(lever);
            targets.insert(
                PhysicalSink::InstanceInput {
                    instance: block,
                    input_index: u16::try_from(index).map_err(|_| SeedError::IdentityOverflow)?,
                },
                TargetGeometry {
                    terminal: lever,
                    allowed_entry: Facing::West,
                    support: step(lever, Facing::East),
                    requirement: TerminalRequirement::Exact(
                        crate::compile::routing::RouteTerminalKind::OutputTerminalRepeater,
                    ),
                },
            );
        }

        // The body, deduplicated by cell: a block's own route floors may
        // restate a cell another of its routes already floors, and the
        // parent only needs each cell claimed once.  First writer wins, in
        // the fixed order below, so the placeholder is deterministic.
        let mut body = BTreeMap::<Anchor, BlockState>::new();
        for cell in compiled_block_cells(&compiled.candidate) {
            let at = shift(cell.at, offset);
            if ports.contains(&at) {
                continue;
            }
            body.entry(at).or_insert_with(|| cell.state.clone());
        }
        let blocks = body
            .into_iter()
            .map(|(at, state)| PlacedBlock { at, state })
            .collect::<Vec<_>>();
        // The offset is a horizontal optimisation control over a body that
        // is already built: the exact cells above are what must stand on
        // the board, and a block has no shell search to move it.
        if let Some(at) = escaped_cell(&blocks, plan.io_footprint) {
            return Err(SeedError::PlacementOutsideIoFootprint { at });
        }
        claim_blocks(occupied, &blocks)?;
        let id = PrimitiveId {
            instance: block,
            node: TopologyNodeId(0),
        };
        candidate.placements.insert(
            id,
            PrimitivePlacement {
                id,
                variant: 0,
                facing: CellFacing::EAST,
                anchor: origin,
                delayed: None,
                blocks,
            },
        );
    }
    Ok(offsets)
}

/// Every physical cell a compiled block owns, in a fixed order.
fn compiled_block_cells(
    candidate: &ExpandedPhysicalCandidate,
) -> impl Iterator<Item = &PlacedBlock> {
    candidate
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
        .chain(
            candidate
                .routes
                .values()
                .flat_map(|route| route.cells.iter().chain(route.floors.iter())),
        )
}

fn shift(at: Anchor, offset: Offset) -> Anchor {
    Anchor {
        x: at.x + offset.dx,
        y: at.y + offset.dy,
        z: at.z + offset.dz,
    }
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
                    plan.io_footprint,
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
                        plan.io_footprint,
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
    footprint: Option<IoFootprint>,
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
    // A junction stands where the plan (and any override) put it, with no
    // search to move it: off the board is a refusal.
    if let Some(at) = escaped_cell(&cells, footprint) {
        return Err(SeedError::PlacementOutsideIoFootprint { at });
    }
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
                    footprint,
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
    footprint: Option<IoFootprint>,
) -> Result<(), SeedError> {
    let blocks = primitive_blocks(primitive, facing, anchor)?;
    // A junction's own hardware has no shell search behind it: its anchor
    // is fixed by the junction cell it hangs off, so an off-board body is
    // refused here rather than moved.
    if let Some(at) = escaped_cell(&blocks, footprint) {
        return Err(SeedError::PlacementOutsideIoFootprint { at });
    }
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
    footprint: Option<IoFootprint>,
) -> Result<(), SeedError> {
    let (anchor, blocks) = find_primitive_placement(
        primitive, facing, preferred, instance, id, search, occupied, footprint,
    )?;
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
    footprint: Option<IoFootprint>,
) -> Result<(Anchor, Vec<PlacedBlock>), SeedError> {
    for anchor in horizontal_manhattan_shells(preferred, search.max_radius) {
        let blocks = primitive_blocks(primitive, facing, anchor)?;
        // A shell candidate standing off the board is refused exactly like
        // an occupied one: the search moves on, and only a search with no
        // candidate left at all reports `PlacementExhausted`.
        if blocks_fit_footprint(&blocks, footprint)
            && blocks.iter().all(|block| !occupied.contains(&block.at))
        {
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

/// The socket a connection lands in; declaration order is the fallback for
/// every primitive that is not a torch.
fn socket_ordinal(sockets: &BTreeMap<ConnectionId, usize>, connection: ConnectionId) -> usize {
    sockets.get(&connection).copied().unwrap_or(0)
}

/// Assigns every torch input connection to one of the torch's three sockets
/// so that the total distance from each driver's route anchor to the socket
/// access cell is minimal.  Declaration order breaks ties, so the choice is a
/// pure function of the placed geometry.
fn assign_torch_sockets(
    candidate: &ExpandedPhysicalCandidate,
    analysis: &SeedPlacementAnalysis,
    frame: PlacementFrame,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
) -> Result<BTreeMap<ConnectionId, usize>, SeedError> {
    use crate::compile::fragment_synth::placement::project_horizontal;
    let lateral_of = |at: Anchor| project_horizontal(at.x, at.z, frame.lateral);
    let along_forward =
        |direction: Facing| direction == frame.forward || direction == frame.forward.opposite();
    // Every source's entry row, keyed by the source endpoint: a sink row of
    // another net that equals or touches one of them would put two ground
    // lines side by side in the same channel.
    let level_of = |endpoint: PhysicalEndpointId| -> i64 {
        let instance = match endpoint {
            PhysicalEndpointId::PrimitiveOutput(primitive) => primitive.instance,
            PhysicalEndpointId::Junction(instance) => instance,
            _ => return -1,
        };
        analysis
            .nodes
            .get(&instance)
            .map_or(-1, |facts| facts.forward_level as i64)
    };
    let source_rows = sources
        .iter()
        .map(|(&endpoint, source)| {
            let row_cell = if along_forward(source.allowed_exit) {
                source.route_anchor
            } else {
                step_many(source.route_anchor, source.allowed_exit, 3)
            };
            (endpoint, (level_of(endpoint), lateral_of(row_cell)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut sockets = BTreeMap::new();
    // Corridor cells already claimed by sockets chosen for earlier torches;
    // a later torch pays for every cell of a candidate socket's entry that
    // touches them, so facing sockets of stacked torches are avoided when
    // the torch has a spare socket.
    let mut claimed = BTreeSet::<Anchor>::new();
    // Sink rows already claimed, with the sink's level and driver.
    let mut claimed_rows = Vec::<(i64, i32, PhysicalEndpointId)>::new();
    for instance in &candidate.instances.instances {
        let sink_level = analysis
            .nodes
            .get(&instance.id)
            .map_or(0, |facts| facts.forward_level as i64);
        let mut by_primitive =
            BTreeMap::<PrimitiveId, Vec<(ConnectionId, Anchor, PhysicalEndpointId)>>::new();
        for connection in &instance.expanded.topology.connections {
            let ConnectionTarget::Primitive(primitive) = connection.target else {
                continue;
            };
            let is_torch = instance
                .expanded
                .topology
                .primitives
                .iter()
                .any(|spec| spec.id == primitive && spec.primitive == Primitive::Torch);
            if !is_torch {
                continue;
            }
            let driver = match connection.source {
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
                    .ok_or(SeedError::Incomplete("torch socket driver"))?,
            };
            let anchor = sources
                .get(&driver)
                .ok_or(SeedError::Incomplete("torch socket driver geometry"))?
                .route_anchor;
            by_primitive
                .entry(primitive)
                .or_default()
                .push((connection.id, anchor, driver));
        }
        for (primitive, inputs) in by_primitive {
            let socket_count = geometry::input_directions(CellFacing::NORTH).len();
            if inputs.len() > socket_count {
                return Err(SeedError::Incomplete("torch socket arity"));
            }
            let placement = candidate
                .placements
                .get(&primitive)
                .ok_or(SeedError::Incomplete("torch socket placement"))?;
            let output = geometry::output_direction(placement.facing);
            let access_cells = (0..socket_count)
                .map(|ordinal| {
                    let geometry = primitive_input_geometry(candidate, primitive, ordinal)?;
                    // The socket behind the torch faces the previous level;
                    // when distances tie it is the one a straight channel
                    // entry can reach without turning across a neighbour.
                    let behind = geometry.allowed_entry == output.opposite();
                    let entry = socket_entry_cells(geometry.terminal, geometry.allowed_entry);
                    let conflicts =
                        entry.iter().filter(|cell| claimed.contains(cell)).count() as u64;
                    let row_cell = if along_forward(geometry.allowed_entry) {
                        geometry.terminal
                    } else {
                        step_many(geometry.terminal, geometry.allowed_entry, 3)
                    };
                    Ok(SocketOption {
                        access: step(geometry.terminal, geometry.allowed_entry),
                        behind,
                        conflicts,
                        entry,
                        row: lateral_of(row_cell),
                    })
                })
                .collect::<Result<Vec<_>, SeedError>>()?;
            // Row conflicts depend on which driver lands in the socket: the
            // driver's own row is the ideal straight line, every other row
            // within one cell is a collision.
            // Only the channel this sink faces matters: sources one level
            // back and sinks of the same level share its rows.
            let row_conflicts = inputs
                .iter()
                .map(|(_, _, driver)| {
                    access_cells
                        .iter()
                        .map(|option| {
                            let own_row = source_rows.get(driver).map(|(_, row)| *row);
                            let against_sources = source_rows
                                .iter()
                                .filter(|(endpoint, (level, _))| {
                                    *endpoint != driver && *level == sink_level - 1
                                })
                                .filter(|(_, (_, row))| (option.row - *row).abs() <= 1)
                                .count();
                            let against_own = usize::from(
                                own_row.is_some_and(|row| (option.row - row).abs() == 1),
                            );
                            let against_sinks = claimed_rows
                                .iter()
                                .filter(|(level, row, _)| {
                                    *level == sink_level && (option.row - *row).abs() <= 1
                                })
                                .count();
                            (against_sources + against_own + against_sinks) as u64
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let mut best: Option<(u64, Vec<usize>)> = None;
            let mut choice = vec![0usize; inputs.len()];
            assign_sockets_recursive(
                &inputs,
                &access_cells,
                &row_conflicts,
                0,
                &mut choice,
                &mut best,
            );
            let (_, ordinals) = best.ok_or(SeedError::Incomplete("torch socket assignment"))?;
            for ((connection, _, driver), ordinal) in inputs.iter().zip(ordinals) {
                sockets.insert(*connection, ordinal);
                claimed.extend(access_cells[ordinal].entry.iter().copied());
                claimed_rows.push((sink_level, access_cells[ordinal].row, *driver));
            }
        }
    }
    Ok(sockets)
}

struct SocketOption {
    access: Anchor,
    behind: bool,
    conflicts: u64,
    entry: Vec<Anchor>,
    row: i32,
}

/// One socket's straight entry: access cell, runway, and their sides.
fn socket_entry_cells(terminal: Anchor, direction: Facing) -> Vec<Anchor> {
    let access = step(terminal, direction);
    let runway = step(access, direction);
    let (left, right) = match direction {
        Facing::North | Facing::South => (Facing::West, Facing::East),
        Facing::East | Facing::West => (Facing::North, Facing::South),
        Facing::Up | Facing::Down => return vec![access],
    };
    vec![
        access,
        runway,
        step(access, left),
        step(access, right),
        step(runway, left),
        step(runway, right),
        step(runway, direction),
    ]
}

/// Cost of one conflicting entry cell; it outweighs any distance the
/// corpus can produce so a spare socket is always preferred to a collision.
const SOCKET_CONFLICT_COST: u64 = 1 << 20;

fn assign_sockets_recursive(
    inputs: &[(ConnectionId, Anchor, PhysicalEndpointId)],
    access_cells: &[SocketOption],
    row_conflicts: &[Vec<u64>],
    index: usize,
    choice: &mut Vec<usize>,
    best: &mut Option<(u64, Vec<usize>)>,
) {
    if index == inputs.len() {
        let cost = inputs
            .iter()
            .zip(choice.iter())
            .enumerate()
            .map(|(input, ((_, anchor, _), &ordinal))| {
                let option = &access_cells[ordinal];
                manhattan(*anchor, option.access) * 2
                    + u64::from(!option.behind)
                    + (option.conflicts + row_conflicts[input][ordinal]) * SOCKET_CONFLICT_COST
            })
            .sum::<u64>();
        let better = match best {
            None => true,
            Some((best_cost, best_choice)) => {
                cost < *best_cost || (cost == *best_cost && *choice < *best_choice)
            }
        };
        if better {
            *best = Some((cost, choice.clone()));
        }
        return;
    }
    for ordinal in 0..access_cells.len() {
        if choice[..index].contains(&ordinal) {
            continue;
        }
        choice[index] = ordinal;
        assign_sockets_recursive(inputs, access_cells, row_conflicts, index + 1, choice, best);
    }
}

fn manhattan(from: Anchor, to: Anchor) -> u64 {
    u64::from(from.x.abs_diff(to.x))
        + u64::from(from.y.abs_diff(to.y))
        + u64::from(from.z.abs_diff(to.z))
}

/// Re-derives the external primitive-input terminals from the chosen sockets
/// so reservations and route obligations agree on every terminal cell.
fn refresh_primitive_targets(
    candidate: &ExpandedPhysicalCandidate,
    sockets: &BTreeMap<ConnectionId, usize>,
    targets: &mut BTreeMap<PhysicalSink, TargetGeometry>,
) -> Result<(), SeedError> {
    for instance in &candidate.instances.instances {
        for connection in &instance.expanded.topology.connections {
            let ConnectionTarget::Primitive(primitive) = connection.target else {
                continue;
            };
            let ConnectionId::External { input_index, .. } = connection.id else {
                continue;
            };
            let geometry = primitive_input_geometry(
                candidate,
                primitive,
                socket_ordinal(sockets, connection.id),
            )?;
            targets.insert(
                PhysicalSink::InstanceInput {
                    instance: instance.id,
                    input_index,
                },
                geometry,
            );
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
    frame: PlacementFrame,
    window: LateralWindow,
    plan_fingerprint: &Fingerprint,
    sources: &BTreeMap<PhysicalEndpointId, SourceGeometry>,
    targets: &BTreeMap<PhysicalSink, TargetGeometry>,
    sockets: &BTreeMap<ConnectionId, usize>,
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
                    let ordinal = socket_ordinal(sockets, connection.id);
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
    // A block has no topology to walk, so its inputs are grouped straight
    // off the graph's assignments.  Its OUTPUTS need nothing here: they are
    // already registered in `sources`, and every consumer of one reaches it
    // through `endpoint_for_driver` like any other instance output.
    for block in &candidate.instances.blocks {
        for input_index in 0..block.inputs.len() {
            let input_index =
                u16::try_from(input_index).map_err(|_| SeedError::IdentityOverflow)?;
            let sink = PhysicalSink::InstanceInput {
                instance: block.id,
                input_index,
            };
            let source = candidate
                .instances
                .assignments
                .iter()
                .find(|assignment| assignment.sink == sink)
                .and_then(|assignment| endpoint_for_driver(&assignment.driver))
                .ok_or(SeedError::Incomplete("block input source"))?;
            let geometry = targets
                .get(&sink)
                .copied()
                .ok_or(SeedError::Incomplete("block input target"))?;
            grouped
                .entry(source)
                .or_default()
                .push(PendingTarget::Connection(
                    ConnectionId::External {
                        instance: block.id,
                        input_index,
                    },
                    geometry,
                ));
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
        // Nothing may stand directly above a terminal, its access cell, or
        // its support: dust up there needs a floor exactly where the terminal
        // goes, and a powered support would drive it back into the input.
        for cell in [
            geometry.terminal,
            step(geometry.terminal, geometry.allowed_entry),
            geometry.support,
        ] {
            reserve_above(reservations, cell, endpoint);
        }
    }

    let output_level = analysis
        .nodes
        .values()
        .map(|facts| facts.forward_level)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let lateral_of = |at: Anchor| {
        crate::compile::fragment_synth::placement::project_horizontal(at.x, at.z, frame.lateral)
    };
    let obligations = grouped
        .into_iter()
        .map(|(source, pending)| {
            let source_level = route_source_level(source, analysis);
            let targets = pending
                .into_iter()
                .map(|target| {
                    let target_level = route_target_level(&target, analysis, output_level);
                    TargetObligation {
                        promoted: false,
                        structural_slack_ticks: route_target_slack(source, &target, analysis),
                        forward_distance: target_level.saturating_sub(source_level),
                        lateral_distance: sources.get(&source).map_or(0, |geometry| {
                            u64::from(
                                lateral_of(geometry.route_anchor)
                                    .abs_diff(lateral_of(target.geometry().terminal)),
                            )
                        }),
                        physical_distance: sources.get(&source).map_or(0, |geometry| {
                            manhattan(geometry.route_anchor, target.geometry().terminal)
                        }),
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

    // The channel routing plan: every net gets its own lane, climb, descent,
    // and ground rows; everything else in the channels is closed.
    let nets = schedule
        .routes
        .iter()
        .map(|scheduled_route| {
            let source_geometry = *sources
                .get(&scheduled_route.source)
                .ok_or(SeedError::Incomplete("route source geometry"))?;
            let sinks = scheduled_route
                .targets
                .iter()
                .map(|target| {
                    let endpoint = match target {
                        PendingTarget::Connection(connection, _) => {
                            PhysicalEndpointId::Landing(*connection)
                        }
                        PendingTarget::DeclaredOutput(port, _) => {
                            PhysicalEndpointId::DeclaredOutput(*port)
                        }
                    };
                    (endpoint, target.geometry())
                })
                .collect();
            Ok(NetGeometry {
                source: scheduled_route.source,
                source_geometry,
                sinks,
            })
        })
        .collect::<Result<Vec<_>, SeedError>>()?;
    let layout = plan_channel_layout(
        candidate,
        analysis,
        frame,
        window,
        &nets,
        router,
        reservations,
        config.router_limits,
    )?;
    // Box stub staircases belong to their routes before any route runs,
    // ahead of the closed layers.
    for (route_index, net) in nets.iter().enumerate() {
        let Some(floors) = layout.floors.get(&net.source) else {
            continue;
        };
        let route = RouteId(u32::try_from(route_index).map_err(|_| SeedError::IdentityOverflow)?);
        for floor in floors {
            if reservations.get(&floor.at).is_none() {
                reservations.reserve(
                    floor.at,
                    PhysicalReservationOwner::RouteStair(route),
                    PhysicalReservationKind::Floor(floor.state.clone()),
                );
            }
        }
    }
    for &cell in &layout.closed {
        if reservations.get(&cell).is_none() {
            reservations.reserve(
                cell,
                PhysicalReservationOwner::KeepOut(CHANNEL_LAYER_OWNER),
                PhysicalReservationKind::KeepOut,
            );
        }
    }

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
        let routed = {
            let mut attempt = reservations.transaction();
            reserve_foreign_private_cells(&mut attempt, &layout, source_id, &protected);
            reserve_source_refresh(&mut attempt, source_id, &source, route)?;
            router.route_transactional(TransactionalRouteRequest {
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
                reservations: &mut attempt,
                limits: config.router_limits,
                no_refresh: layout.departures.get(&source_id),
            })
        };
        let mut tree = match routed {
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
                )));
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
        // Commit only what the route really laid: the attempt-local corridor
        // keep-outs of later routes must not leak into the shared map.
        reserve_source_refresh(reservations, source_id, &source, route)?;
        reserve_route(reservations, &tree, &protected);
        candidate.routes.insert(route, tree);
    }
    Ok(())
}

/// Closes every other net's private plan cells for this attempt.  The
/// owner's own cells stay free, and pre-reserved terminals are untouched.
fn reserve_foreign_private_cells(
    attempt: &mut impl ReservationStore,
    layout: &ChannelLayout,
    owner: PhysicalEndpointId,
    protected: &BTreeSet<Anchor>,
) {
    let own = layout.private.get(&owner);
    for (&net, cells) in &layout.private {
        if net == owner {
            continue;
        }
        for &cell in cells {
            if protected.contains(&cell) || own.is_some_and(|own| own.contains(&cell)) {
                continue;
            }
            if attempt.get(&cell).is_none() {
                attempt.reserve(
                    cell,
                    PhysicalReservationOwner::Endpoint(net),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
    }
}

/// A junction source is refreshed by a repeater at its route anchor, so the
/// anchor is promoted to an exact route conductor and its two side cells are
/// kept clear of foreign dust.
fn reserve_source_refresh(
    reservations: &mut impl ReservationStore,
    source_id: PhysicalEndpointId,
    source: &SourceGeometry,
    route: RouteId,
) -> Result<(), SeedError> {
    if !matches!(source_id, PhysicalEndpointId::Junction(_)) {
        return Ok(());
    }
    if !reservations.promote_endpoint_conductor(
        source.route_anchor,
        source_id,
        route,
        compile::repeater(source.allowed_exit),
    ) {
        return Err(SeedError::Incomplete("junction source refresh reservation"));
    }
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

pub(crate) fn refresh_exact_route_delays(tree: &mut crate::compile::routing::RealisedRouteTree) {
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
    // A route cell directly above a component would need a floor exactly
    // where the component stands, so the cell above every component block is
    // closed as well.
    let component_cells = candidate
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
        .map(|block| (block.at, block.state.kind))
        .collect::<Vec<_>>();
    for (at, _) in component_cells {
        // Mandatory air rather than a keep-out: it also refuses a route floor
        // there, so nothing can stand on top of a torch, lever, or support
        // that the component may power.
        let above = Anchor { y: at.y + 1, ..at };
        if reservations.get(&above).is_none() {
            reservations.reserve(
                above,
                PhysicalReservationOwner::KeepOut(ordinal),
                PhysicalReservationKind::MandatoryAir,
            );
            ordinal = ordinal.saturating_add(1);
        }
    }
    Ok(reservations)
}

pub(crate) fn reserve_route(
    reservations: &mut impl ReservationStore,
    tree: &crate::compile::routing::RealisedRouteTree,
    protected: &BTreeSet<Anchor>,
) {
    // Terminals and the source anchor were protected as endpoint keep-outs
    // before routing; now that the route really occupies them they become
    // exact conductors so later routes keep their halo distance from them.
    let mut endpoint_at = BTreeMap::new();
    for branch in &tree.branches {
        endpoint_at.insert(branch.root, tree.source);
        let endpoint = match branch.target {
            crate::compile::routing::RouteTarget::Connection(connection) => {
                PhysicalEndpointId::Landing(connection)
            }
            crate::compile::routing::RouteTarget::DeclaredOutput(port) => {
                PhysicalEndpointId::DeclaredOutput(port)
            }
        };
        endpoint_at.insert(branch.terminal.at, endpoint);
    }
    for block in &tree.cells {
        if reservations
            .reserve(
                block.at,
                PhysicalReservationOwner::Route(tree.id),
                PhysicalReservationKind::Conductor(block.state.clone()),
            )
            .is_none()
        {
            if let Some(&endpoint) = endpoint_at.get(&block.at) {
                reservations.promote_endpoint_conductor(
                    block.at,
                    endpoint,
                    tree.id,
                    block.state.clone(),
                );
            }
        }
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

/// The first cell of `blocks` standing off the board a complete pin set
/// drew, in the list's own fixed order -- so two runs name the same cell.
///
/// `None` for the footprint is a partial or unpinned set: it draws no
/// board, nothing can leave one, and every post-plan move stays exactly
/// what it was before the caller's cells bounded anything.
fn escaped_cell(blocks: &[PlacedBlock], footprint: Option<IoFootprint>) -> Option<Anchor> {
    let footprint = footprint?;
    blocks
        .iter()
        .map(|block| block.at)
        .find(|at| !footprint.contains_xz(*at))
}

/// Whether every cell of `blocks` stands on that board.  The one
/// containment question every post-plan movement site asks, over the exact
/// `PlacedBlock` list that site already built -- no envelope is rebuilt and
/// no second walk exists.
fn blocks_fit_footprint(blocks: &[PlacedBlock], footprint: Option<IoFootprint>) -> bool {
    escaped_cell(blocks, footprint).is_none()
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

pub(crate) fn step(at: Anchor, direction: Facing) -> Anchor {
    match direction {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

pub(crate) fn step_many(at: Anchor, direction: Facing, distance: i32) -> Anchor {
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
// `pub(crate)` only so `circuits::hierarchical_builder`'s equivalence tests
// can reach `extra_circuits`'s flat reference circuits below (see that
// module's own comment) -- nothing in here is otherwise touched or
// re-exported outside `#[cfg(test)]` builds.
pub(crate) mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::circuits::and4::build_and4_netlist;
    use crate::compile::fragment_synth::certification::CompleteCandidateCertifier;
    use crate::compile::fragment_synth::legacy_adapter::{LegacyCandidateAdapter, LegacyOracle};
    use crate::compile::fragment_synth::placement::{
        DeckId, DeckPlan, FloorplanMetrics, PreferredInstancePose, SeedPlacementError,
        SeedPlacementPlan, SeedPlacementRequest, SeedPlacer, TopologyAwareSeedPlacer,
    };
    use crate::compile::metrics::canonical_fingerprint;
    use crate::compile::planner::{PinRefusal, PortPin};
    use crate::compile::routing::{
        GuardedPhysicalRouter, PhysicalReservation, RealisedRouteTree, RouteRequest,
    };
    use crate::compile::topology::GateKind;
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
            None,
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
            None,
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

    /// A plan literal carrying exactly the board and poses a test wants,
    /// so the post-plan movement sites can be driven on their own -- the
    /// placer's own bounding is `placement.rs`'s business, not theirs.
    fn bounded_plan(
        analysis: &SeedPlacementAnalysis,
        instances: BTreeMap<InstanceId, PreferredInstancePose>,
        footprint: Option<IoFootprint>,
    ) -> SeedPlacementPlan {
        SeedPlacementPlan {
            frame: crate::compile::fragment_synth::placement::derive_frame(&BTreeMap::new()),
            analysis: analysis.clone(),
            window: LateralWindow::default(),
            io_footprint: footprint,
            instances,
            automatic_inputs: BTreeMap::new(),
            automatic_outputs: BTreeMap::new(),
            fingerprint: canonical_fingerprint(b"bounded-movement-plan"),
            decks: BTreeMap::from([(
                DeckId(0),
                DeckPlan {
                    ground: 1,
                    min_y: 0,
                    max_y: 4,
                },
            )]),
            floorplan: FloorplanMetrics {
                macro_volume: 0,
                union_volume: 0,
                deck_count: 1,
                cross_deck_nets: 0,
                vertical_trunk_lanes: 0,
            },
        }
    }

    /// The board a complete pin set drew bounds the shell search itself: an
    /// anchor whose body would stand off it is skipped exactly like an
    /// occupied one, and when no shell is on it the search ends in the
    /// existing `PlacementExhausted`.
    #[test]
    fn bounded_post_plan_moves_never_cross_the_io_footprint_at_a_shell_candidate() {
        let preferred = Anchor { x: 8, y: 4, z: 9 };
        let instance = InstanceId(3);
        let primitive = PrimitiveId {
            instance,
            node: crate::compile::fragment_synth::identity::TopologyNodeId(2),
        };
        let search = || PlacementSearch {
            max_radius: 4,
            max_backtracks: 100,
            backtracks_used: 0,
        };
        let place = |footprint, search: &mut PlacementSearch| {
            find_primitive_placement(
                Primitive::Torch,
                CellFacing::EAST,
                preferred,
                instance,
                primitive,
                search,
                &BTreeSet::new(),
                footprint,
            )
        };

        // One cell of the torch's body at `preferred` stands past `max_x`,
        // and nothing at all is occupied: only the board can move it.
        let body = primitive_blocks(Primitive::Torch, CellFacing::EAST, preferred).unwrap();
        let east_most = body.iter().map(|block| block.at.x).max().unwrap();
        let board = IoFootprint {
            min_x: east_most - 64,
            max_x: east_most - 1,
            min_z: preferred.z - 64,
            max_z: preferred.z + 64,
        };
        let (anchor, blocks) = place(Some(board), &mut search()).unwrap();
        assert_ne!(anchor, preferred);
        assert!(blocks_fit_footprint(&blocks, Some(board)));

        // The same anchor with no board -- a partial or unpinned pin set --
        // is still the legacy first answer.
        assert_eq!(place(None, &mut search()).unwrap().0, preferred);

        // A board no shell reaches is the existing exhaustion refusal, not
        // a new one.
        let far = IoFootprint {
            min_x: 1_000,
            max_x: 1_040,
            min_z: 1_000,
            max_z: 1_040,
        };
        assert!(matches!(
            place(Some(far), &mut search()).unwrap_err(),
            SeedError::PlacementExhausted {
                instance: InstanceId(3),
                primitive: actual,
                radius: 4,
            } if actual == primitive
        ));
    }

    /// An `InstancePlacementOverride` is a horizontal optimisation control,
    /// and the board bounds it like every other post-plan move: an override
    /// that parks the instance further off the board than any shell reaches
    /// back ends in `PlacementExhausted`, while the same override with no
    /// board is honoured to the cell.
    #[test]
    fn bounded_post_plan_moves_never_cross_the_io_footprint_at_an_instance_override() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let graph =
            InstanceGraph::with_variants(&netlist, &library, &BTreeMap::new(), &[]).unwrap();
        let analysis = analyse_instance_dag(&graph, &BTreeMap::new()).unwrap();
        let origin = Anchor { x: 20, y: 1, z: 20 };
        let board = IoFootprint {
            min_x: 0,
            max_x: 40,
            min_z: 0,
            max_z: 40,
        };
        let place = |footprint, dx| {
            let plan = bounded_plan(
                &analysis,
                BTreeMap::from([(
                    InstanceId(0),
                    PreferredInstancePose {
                        preferred_origin: origin,
                        facing: CellFacing::EAST,
                    },
                )]),
                footprint,
            );
            let mut candidate =
                ExpandedPhysicalCandidate::empty(graph.clone(), PortPlacements::default());
            let mut occupied = BTreeSet::new();
            let mut sources = BTreeMap::new();
            let mut targets = BTreeMap::new();
            place_instances(
                &mut candidate,
                &netlist,
                &config,
                &plan,
                PlanTranslation::default(),
                &BTreeMap::from([(
                    InstanceId(0),
                    InstancePlacementOverride {
                        facing: CellFacing::EAST,
                        dx,
                        dz: 0,
                    },
                )]),
                &mut occupied,
                &mut sources,
                &mut targets,
            )
            .map(|()| candidate)
        };
        let anchor = |candidate: &ExpandedPhysicalCandidate| {
            candidate
                .placements
                .values()
                .next()
                .expect("the one instance is placed")
                .anchor
        };

        assert!(matches!(
            place(Some(board), 200).unwrap_err(),
            SeedError::PlacementExhausted {
                instance: InstanceId(0),
                ..
            }
        ));
        // A partial pin set draws no board at all, and the legacy move
        // stands exactly where it asked to.
        assert_eq!(
            anchor(&place(IoFootprint::from_complete(2, [origin]), 200).unwrap()),
            Anchor { x: 220, ..origin }
        );
        // Inside the board the override is honoured, not clamped.
        assert_eq!(
            anchor(&place(Some(board), 10).unwrap()),
            Anchor { x: 30, ..origin }
        );
    }

    /// A `BlockPlacementOffset` moves a whole stamped body, and the board
    /// bounds that body cell by cell: there is no search to fall back on
    /// here, so an offset that walks it off the board is the bounded
    /// placement refusal, naming the first cell that left.
    #[test]
    fn bounded_post_plan_moves_never_cross_the_io_footprint_at_a_block_offset() {
        let (library, config) = default_services_parts();
        let lowered = crate::compile::lowering::lower_optimised(&not_netlist()).unwrap();
        let compiled = crate::compile::fragment_synth::blocks::compile_block(
            "not",
            &lowered,
            services(&library, &config),
        )
        .expect("the not gate compiles as a block");
        let blocks = [compiled];
        let inputs = vec!["a".to_string()];
        let outputs = vec!["y".to_string()];
        let planning = Netlist {
            inputs: inputs.clone(),
            outputs: outputs.clone(),
            gates: vec![Gate {
                name: "u0.0".into(),
                inputs: inputs.clone(),
                output: outputs[0].clone(),
                kind: GateKind::Buf,
            }],
        };
        let specs = [crate::compile::fragment_synth::instance_graph::BlockSpec {
            name: "u0",
            block: 0,
            inputs: &inputs,
            outputs: &outputs,
        }];
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs).expect("parent graph");
        let block_id = graph.blocks[0].id;
        let origin = Anchor { x: 20, y: 1, z: 20 };
        let board = IoFootprint {
            min_x: 0,
            max_x: 60,
            min_z: 0,
            max_z: 60,
        };
        let place = |footprint, dx| {
            let mut candidate =
                ExpandedPhysicalCandidate::empty(graph.clone(), PortPlacements::default());
            let resolved =
                ResolvedBlocks::resolve(&candidate.instances, ParentBlocks { compiled: &blocks })
                    .expect("the block resolves");
            let analysis =
                analyse_instance_dag(&candidate.instances, &resolved.delays).expect("analysis");
            let plan = bounded_plan(
                &analysis,
                BTreeMap::from([(
                    block_id,
                    PreferredInstancePose {
                        preferred_origin: origin,
                        facing: CellFacing::EAST,
                    },
                )]),
                footprint,
            );
            let mut occupied = BTreeSet::new();
            let mut sources = BTreeMap::new();
            let mut targets = BTreeMap::new();
            place_blocks(
                &mut candidate,
                &resolved,
                &plan,
                PlanTranslation::default(),
                &BTreeMap::from([(block_id, BlockPlacementOffset { dx, dz: 0 })]),
                &mut occupied,
                &mut sources,
                &mut targets,
            )
            .map(|_| candidate)
        };
        let body = |candidate: &ExpandedPhysicalCandidate| {
            candidate.placements[&PrimitiveId {
                instance: block_id,
                node: TopologyNodeId(0),
            }]
                .blocks
                .clone()
        };

        // On the board, the body stands where the offset put it.
        let inside = place(Some(board), 0).expect("a body on the board stands");
        assert!(blocks_fit_footprint(&body(&inside), Some(board)));

        // A hundred cells east of a 61-cell board, every one of its cells
        // has left, and the refusal names the first of them in the body's
        // own fixed order.
        let first = body(&inside)[0].at;
        assert!(matches!(
            place(Some(board), 100).unwrap_err(),
            SeedError::PlacementOutsideIoFootprint { at } if at == Anchor { x: first.x + 100, ..first }
        ));
        // With no board that same offset is the legacy move.
        let legacy = place(None, 100).expect("an unbounded body moves as it always did");
        assert_eq!(
            body(&legacy)[0].at,
            Anchor {
                x: first.x + 100,
                ..first
            }
        );
    }

    /// The block half of `macro_envelopes_keep_their_real_vertical_bounds`:
    /// a block's height comes from the same certified bounds its width and
    /// depth do, and an inverted Y span is refused exactly where an
    /// inverted X or Z span already is.
    #[test]
    fn macro_envelopes_keep_their_real_vertical_bounds_for_resolved_blocks() {
        let (library, config) = default_services_parts();
        let lowered = crate::compile::lowering::lower_optimised(&not_netlist()).unwrap();
        let compiled = crate::compile::fragment_synth::blocks::compile_block(
            "not",
            &lowered,
            services(&library, &config),
        )
        .expect("the not gate compiles as a block");
        let inputs = vec!["a".to_string()];
        let outputs = vec!["y".to_string()];
        let planning = Netlist {
            inputs: inputs.clone(),
            outputs: outputs.clone(),
            gates: vec![Gate {
                name: "u0.0".into(),
                inputs: inputs.clone(),
                output: outputs[0].clone(),
                kind: GateKind::Buf,
            }],
        };
        let specs = [crate::compile::fragment_synth::instance_graph::BlockSpec {
            name: "u0",
            block: 0,
            inputs: &inputs,
            outputs: &outputs,
        }];
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs).expect("parent graph");
        let block_id = graph.blocks[0].id;

        let blocks = [compiled.clone()];
        let resolved = ResolvedBlocks::resolve(&graph, ParentBlocks { compiled: &blocks })
            .expect("the block resolves");
        let facts = resolved.facts[&block_id];
        assert_eq!(facts.height, compiled.bounds.max.y - compiled.bounds.min.y + 1);
        assert_eq!(facts.width, compiled.bounds.max.x - compiled.bounds.min.x + 1);

        // An inverted Y span is no box at all, and the existing
        // block-resolution refusal is where that is said.
        let mut inverted = compiled.clone();
        inverted.bounds.max.y = inverted.bounds.min.y - 1;
        let blocks = [inverted];
        assert!(matches!(
            ResolvedBlocks::resolve(&graph, ParentBlocks { compiled: &blocks }),
            Err(SeedError::BlockTooWide { block }) if block == block_id
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
                    deck: DeckId(0),
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
                frame: crate::compile::fragment_synth::placement::derive_frame(_request.pins),
                analysis: _request.analysis.clone(),
                window: LateralWindow::default(),
                io_footprint: None,
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
                decks: BTreeMap::from([(
                    DeckId(0),
                    DeckPlan {
                        ground: 7,
                        min_y: 6,
                        max_y: 10,
                    },
                )]),
                floorplan: FloorplanMetrics {
                    macro_volume: 0,
                    union_volume: 0,
                    deck_count: 1,
                    cross_deck_nets: 0,
                    vertical_trunk_lanes: 0,
                },
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
                router: &GuardedPhysicalRouter,
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

    /// A real certified candidate with more than one instance and a
    /// non-trivial route tree, for `relocate.rs`'s translation and
    /// renumbering tests -- `not_netlist()`'s one-gate candidate is too
    /// small to catch a walker that misses a field.
    pub(crate) fn certified_full_adder() -> CertifiedCandidate {
        let (netlist, _) = crate::circuits::full_adder::build_full_adder_netlist();
        let lowered =
            crate::compile::lowering::lower_optimised(&netlist).expect("full adder netlist lowers");
        build(&lowered).expect("full adder seed certifies")
    }

    /// The owning pair behind [`services`] -- split out because
    /// `SeedServices` only borrows, so its `library`/`search_config` fields
    /// need somewhere outside the call that builds them to live.
    pub(crate) fn default_services_parts() -> (Library, SearchConfig) {
        (Library::default_library(), SearchConfig::checked_defaults())
    }

    /// The same `SeedServices` wiring `api.rs`'s
    /// `compile_fragment_synth_with_case_fingerprint` uses in production
    /// (`api.rs:131-139`), for callers outside this module (e.g.
    /// `blocks.rs`) that need a real seed run without duplicating that
    /// wiring themselves.
    pub(crate) fn services<'a>(library: &'a Library, config: &'a SearchConfig) -> SeedServices<'a> {
        SeedServices {
            library,
            placer: &TopologyAwareSeedPlacer,
            router: &GuardedPhysicalRouter,
            certifier: &CompleteCandidateCertifier,
            search_config: config,
        }
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
                router: &GuardedPhysicalRouter,
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
                router: &GuardedPhysicalRouter,
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
                router: &GuardedPhysicalRouter,
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
            GuardedPhysicalRouter.route(request)
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
    fn production_seed_has_one_finishing_authority() {
        let (netlist, _) = build_and4_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CountingRouter::default();
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
                certifier: &certifier,
                search_config: &config,
            },
        )
        .expect("fixture certifies");

        assert_eq!(
            certified.candidate().instances.instances.len(),
            netlist.gates.len()
        );
        assert!(router.calls.get() > 0);
        assert_eq!(certifier.calls.get(), 1);
        assert_eq!(
            certified.metrics().candidate_fingerprint,
            certified.candidate().fingerprint(),
        );
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
                certifier: &certifier,
                search_config: &config,
            },
        )
        .unwrap_err();

        assert!(matches!(error, SeedError::InvalidPins(_)));
        assert_eq!(router.calls.get(), 0);
        assert_eq!(certifier.calls.get(), 0);
    }

    /// A complete pin set gives every terminal one exact tunnel, and the only
    /// hardware allowed inside it is that terminal's own: the handover and
    /// the cell it stands on.  Anything else REDA placed there is refused by
    /// port name before a single route runs, and every remaining cell is
    /// reserved -- as mandatory air wherever a route could otherwise stand on
    /// it, so no route floor can appear inside the tunnel either.
    ///
    /// A partial pin set draws no board, so none of this happens to it: its
    /// terminals keep the older five-neighbour, signal-only promise and the
    /// same geometry reserves nothing.
    #[test]
    fn a_complete_pin_tunnel_rejects_macro_and_route_floor_cells() {
        let netlist = not_netlist();
        let library = Library::default_library();
        let graph =
            InstanceGraph::with_variants(&netlist, &library, &BTreeMap::new(), &[]).unwrap();
        let input = PortPin {
            at: Anchor { x: 10, y: 1, z: 10 },
            toward: Facing::East,
        };
        let output = PortPin {
            at: Anchor { x: 30, y: 1, z: 20 },
            toward: Facing::East,
        };
        // The board these two draw is x 10..=30 by z 10..=20, so the input's
        // own tunnel already reaches one cell past its southern edge.
        let mut complete = PortPlacements::default();
        complete.pin("a", input.at, input.toward);
        complete.pin("y", output.at, output.toward);
        let mut partial = PortPlacements::default();
        partial.pin("a", input.at, input.toward);

        let in_handover = input.handover(PortRole::Input);
        let out_handover = output.handover(PortRole::Output);
        let primitive = PrimitiveId {
            instance: InstanceId(0),
            node: TopologyNodeId(0),
        };
        let build = |pins: &PortPlacements, intruder: Option<Anchor>| {
            let mut candidate = ExpandedPhysicalCandidate::empty(graph.clone(), pins.clone());
            candidate
                .bind_pin_contracts(&netlist)
                .expect("both pins name declared ports");
            // Exactly the hardware `place_boundaries` builds for a pinned
            // input: the handover repeater and the support under it.
            let endpoint = PhysicalEndpointId::PrimaryInput(PortId(0));
            candidate.boundaries.insert(
                endpoint,
                BoundaryPlacement {
                    endpoint,
                    delayed: None,
                    blocks: vec![
                        PlacedBlock {
                            at: Anchor {
                                y: in_handover.y - 1,
                                ..in_handover
                            },
                            state: compile::stone(),
                        },
                        PlacedBlock {
                            at: in_handover,
                            state: compile::repeater(input.toward),
                        },
                    ],
                },
            );
            if let Some(at) = intruder {
                candidate.placements.insert(
                    primitive,
                    PrimitivePlacement {
                        id: primitive,
                        variant: 0,
                        facing: CellFacing::EAST,
                        anchor: at,
                        delayed: None,
                        blocks: vec![PlacedBlock {
                            at,
                            state: compile::stone(),
                        }],
                    },
                );
            }
            candidate
        };
        let reserve = |candidate: &ExpandedPhysicalCandidate| {
            let mut reservations = reservations_for_components(candidate)?;
            reserve_terminal_tunnels(candidate, &netlist, &mut reservations)?;
            Ok::<_, SeedError>(reservations)
        };

        // The terminal's own hardware is not an encroachment, and the tunnel
        // around it is reserved.
        let mut reservations = reserve(&build(&complete, None)).expect("a clear tunnel reserves");
        let claim = |at: Anchor| reservations.get(&at).cloned();
        // Caller cell and the rest of the middle of the tunnel: keep-out, so
        // no route conductor may enter.
        assert_eq!(
            claim(input.at),
            Some(PhysicalReservation {
                owner: PhysicalReservationOwner::Endpoint(PhysicalEndpointId::PrimaryInput(
                    PortId(0)
                )),
                kind: PhysicalReservationKind::KeepOut,
            })
        );
        // The roof of the tunnel is mandatory air: a route one cell higher
        // would otherwise stand on it as if it were floor.
        for at in [Anchor { x: 10, y: 2, z: 10 }, Anchor { x: 30, y: 2, z: 20 }] {
            assert_eq!(
                claim(at).map(|reservation| reservation.kind),
                Some(PhysicalReservationKind::MandatoryAir),
                "the cell above {at:?} must be mandatory air",
            );
        }
        // The handover and its support are the terminal's required hardware:
        // the input's stay the boundary's own claims, and the output's -- which
        // its route still has to build -- stay free.
        assert!(matches!(
            claim(in_handover),
            Some(PhysicalReservation {
                owner: PhysicalReservationOwner::KeepOut(_),
                ..
            })
        ));
        assert_eq!(claim(out_handover), None);
        assert_eq!(
            claim(Anchor {
                y: out_handover.y - 1,
                ..out_handover
            }),
            None
        );
        // One cell south of the input's caller cell is off the board, so the
        // tunnel never reached it and REDA reserves nothing there.
        assert_eq!(claim(Anchor { x: 10, y: 1, z: 9 }), None);

        // A route floor cannot land on a reserved tunnel cell, whichever kind
        // holds it.
        for at in [Anchor { x: 10, y: 2, z: 10 }, input.at] {
            let held = reservations
                .get(&at)
                .cloned()
                .expect("the tunnel holds this cell");
            reservations.reserve(
                at,
                PhysicalReservationOwner::Route(RouteId(0)),
                PhysicalReservationKind::Floor(compile::stone()),
            );
            assert_eq!(reservations.get(&at), Some(&held));
        }

        // A macro body in a non-exempt tunnel cell is refused by port name,
        // carrying the cell it took.
        let intruder = Anchor { x: 10, y: 1, z: 11 };
        let error = reserve(&build(&complete, Some(intruder))).unwrap_err();
        let SeedError::InvalidPins(crate::compile::planner::PlannerError::InvalidPortPin {
            port,
            at,
            refusal,
        }) = error
        else {
            panic!("a macro in a terminal tunnel is an invalid pin: {error:?}");
        };
        assert_eq!(port, "a");
        assert_eq!(at, input.at);
        assert_eq!(
            refusal,
            PinRefusal::ClearanceConflict {
                other_port_cell: intruder,
            }
        );

        // The same geometry with one port unpinned draws no board: the body
        // stands, and nothing around the terminal is reserved beyond what the
        // components themselves already claimed.
        let legacy = reserve(&build(&partial, Some(intruder))).expect("a partial pin set stands");
        assert!(matches!(
            legacy.get(&intruder),
            Some(PhysicalReservation {
                owner: PhysicalReservationOwner::KeepOut(_),
                ..
            })
        ));
        assert_eq!(legacy.get(&input.at), None);
        assert_eq!(legacy.get(&Anchor { x: 10, y: 2, z: 10 }), None);
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
                router: &GuardedPhysicalRouter,
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedRouteRequest {
        id: RouteId,
        source: PhysicalEndpointId,
        source_anchor: Anchor,
        sinks: Vec<(PhysicalEndpointId, Anchor)>,
        limits: crate::compile::routing::RouterLimits,
    }

    /// Forwards every request to the production router unchanged while keeping
    /// the evidence: request identity, typed endpoints in caller order, limits,
    /// and a snapshot of the first request's reservations.
    #[derive(Default)]
    struct RecordingRouter {
        requests: RefCell<Vec<RecordedRouteRequest>>,
        first_reservations: RefCell<Option<PhysicalReservations>>,
    }

    impl PhysicalRouter for RecordingRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.first_reservations
                .borrow_mut()
                .get_or_insert_with(|| request.reservations.clone());
            self.requests.borrow_mut().push(RecordedRouteRequest {
                id: request.id,
                source: request.source.id,
                source_anchor: request.source.anchor,
                sinks: request
                    .sinks
                    .as_slice()
                    .iter()
                    .map(|sink| (sink.endpoint, sink.anchor))
                    .collect(),
                limits: request.limits,
            });
            GuardedPhysicalRouter.route(request)
        }
    }

    #[test]
    fn and4_schedule_reaches_the_router_in_order_over_pre_reserved_terminals() {
        let (netlist, _) = build_and4_netlist();
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = RecordingRouter::default();

        compile_sparse_seed_with_services(
            SeedInput {
                lowered: &netlist,
                source_provenance: None,
                pins: None,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &router,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        )
        .unwrap();

        let requests = router.requests.borrow();
        let observed = requests
            .iter()
            .map(|request| {
                (
                    request.source,
                    request
                        .sinks
                        .iter()
                        .map(|(endpoint, _)| *endpoint)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        fn primitive_output(instance: u32) -> PhysicalEndpointId {
            PhysicalEndpointId::PrimitiveOutput(PrimitiveId {
                instance: InstanceId(instance),
                node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
            })
        }
        fn external_landing(instance: u32, input_index: u16) -> PhysicalEndpointId {
            PhysicalEndpointId::Landing(ConnectionId::External {
                instance: InstanceId(instance),
                input_index,
            })
        }
        let expected = vec![
            (primitive_output(0), vec![external_landing(3, 0)]),
            (primitive_output(1), vec![external_landing(3, 1)]),
            (primitive_output(2), vec![external_landing(3, 2)]),
            (primitive_output(3), vec![external_landing(4, 0)]),
            (primitive_output(4), vec![external_landing(6, 0)]),
            (
                primitive_output(6),
                vec![PhysicalEndpointId::DeclaredOutput(PortId(0))],
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(0)),
                vec![external_landing(0, 0)],
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(1)),
                vec![external_landing(1, 0)],
            ),
            (
                PhysicalEndpointId::PrimaryInput(PortId(2)),
                vec![external_landing(2, 0)],
            ),
            (primitive_output(5), vec![external_landing(6, 1)]),
            (
                PhysicalEndpointId::PrimaryInput(PortId(3)),
                vec![external_landing(5, 0)],
            ),
        ];
        assert_eq!(observed, expected);

        for (scheduled_index, request) in requests.iter().enumerate() {
            assert_eq!(
                request.id,
                RouteId(u32::try_from(scheduled_index).unwrap()),
                "scheduled request {scheduled_index} carries a non-sequential RouteId"
            );
            assert_eq!(
                request.limits, config.router_limits,
                "scheduled request {scheduled_index} altered the configured router limits"
            );
        }

        let first_reservations = router.first_reservations.borrow();
        let first_reservations = first_reservations
            .as_ref()
            .expect("the certifying build must issue at least one route request");
        for request in requests.iter() {
            assert!(
                first_reservations.get(&request.source_anchor).is_some(),
                "source terminal {:?} of {:?} was not reserved before routing began",
                request.source_anchor,
                request.source,
            );
            for (endpoint, anchor) in &request.sinks {
                assert!(
                    first_reservations.get(anchor).is_some(),
                    "sink terminal {anchor:?} of {endpoint:?} was not reserved before routing began",
                );
            }
        }
    }
    /// Records every access-corridor violation the production seed hands to
    /// the router: a source exit cell or sink approach cell that a foreign
    /// route already occupies, or that a foreign conductor hugs.
    #[derive(Default)]
    struct CorridorCheckingRouter {
        violations: RefCell<Vec<String>>,
    }

    impl CorridorCheckingRouter {
        fn check_cell(&self, request: &RouteRequest<'_>, label: &str, cell: Anchor) {
            let own_endpoints = std::iter::once(request.source.id)
                .chain(request.sinks.as_slice().iter().map(|sink| sink.endpoint))
                .collect::<BTreeSet<_>>();
            if let Some(claim) = request.reservations.get(&cell) {
                let own = match claim.owner {
                    PhysicalReservationOwner::Endpoint(endpoint) => {
                        own_endpoints.contains(&endpoint)
                    }
                    PhysicalReservationOwner::Route(route)
                    | PhysicalReservationOwner::RouteStair(route) => route == request.id,
                    PhysicalReservationOwner::Sink(sink) => sink.route == request.id,
                    PhysicalReservationOwner::KeepOut(_) => false,
                };
                if !own {
                    self.violations.borrow_mut().push(format!(
                        "route {:?} {label} {cell:?} is reserved by {:?}",
                        request.id, claim.owner
                    ));
                }
            }
            for direction in [Facing::North, Facing::South, Facing::East, Facing::West] {
                let neighbour = step(cell, direction);
                if let Some(claim) = request.reservations.get(&neighbour) {
                    let foreign_conductor =
                        matches!(claim.kind, PhysicalReservationKind::Conductor(_))
                            && !matches!(
                                claim.owner,
                                PhysicalReservationOwner::Route(route) if route == request.id
                            );
                    if foreign_conductor {
                        self.violations.borrow_mut().push(format!(
                            "route {:?} {label} {cell:?} is hugged by {:?} at {neighbour:?}",
                            request.id, claim.owner
                        ));
                    }
                }
            }
        }
    }

    impl PhysicalRouter for CorridorCheckingRouter {
        fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
            self.check_cell(
                &request,
                "source exit",
                step(request.source.anchor, request.source.allowed_exit),
            );
            for sink in request.sinks.as_slice() {
                self.check_cell(
                    &request,
                    "sink approach",
                    step(sink.anchor, sink.allowed_entry),
                );
            }
            GuardedPhysicalRouter.route(request)
        }
    }

    fn build_checking_corridors(
        netlist: &Netlist,
        pins: Option<&PortPlacements>,
    ) -> (Result<CertifiedCandidate, SeedError>, Vec<String>) {
        let library = Library::default_library();
        let config = SearchConfig::checked_defaults();
        let router = CorridorCheckingRouter::default();
        let result = compile_sparse_seed_with_services(
            SeedInput {
                lowered: netlist,
                source_provenance: None,
                pins,
            },
            SeedServices {
                library: &library,
                placer: &TopologyAwareSeedPlacer,
                router: &router,
                certifier: &CompleteCandidateCertifier,
                search_config: &config,
            },
        );
        let violations = router.violations.into_inner();
        (result, violations)
    }

    #[test]
    fn pinned_and4_routes_never_occupy_or_hug_a_later_access_corridor() {
        let (netlist, _) = build_and4_netlist();
        let output_name = netlist.outputs[0].clone();
        let mut pins = PortPlacements::default();
        pins.pin("a", Anchor { x: 21, y: 1, z: 62 }, Facing::North)
            .pin(output_name, Anchor { x: 53, y: 1, z: 10 }, Facing::North);

        let (result, violations) = build_checking_corridors(&netlist, Some(&pins));

        assert_eq!(violations, Vec::<String>::new());
        result.expect("pinned and4 must route and certify once corridors are protected");
    }

    #[test]
    fn pinned_output_facing_away_from_its_channel_is_joined_by_a_router_stub() {
        // The output lamp faces south, so its wire has to enter from the
        // north, on the far side from the channel that serves it: the layout
        // must plan a stub around the pin instead of a straight line into
        // the lamp.
        let (netlist, _) = build_and4_netlist();
        let output_name = netlist.outputs[0].clone();
        let mut pins = PortPlacements::default();
        pins.pin("a", Anchor { x: 21, y: 1, z: 62 }, Facing::North)
            .pin(output_name, Anchor { x: 53, y: 1, z: 10 }, Facing::South);

        let (result, violations) = build_checking_corridors(&netlist, Some(&pins));

        assert_eq!(violations, Vec::<String>::new());
        result.expect("a pinned output facing away from its channel must route and certify");
    }

    /// Circuits beyond the six acceptance cases, built with the same
    /// netlist builder the reference circuits use.  Release-only: the
    /// seven-segment slices take a quarter of a minute each in release.
    // `pub(crate)`: see the `pub(crate) mod tests` comment above -- widened
    // only so `ripple_adder`/`alu4_full`/`multiplier4` below are reachable
    // as the flat comparison netlists for the hierarchical equivalence
    // tests. Logic is untouched.
    pub(crate) mod extra_circuits {
        use crate::circuits::netlist_builder::NetlistBuilder;
        use crate::compile::fragment_synth::api::{
            compile_fragment_synth, SynthesisInput, SynthesisResult,
        };
        use crate::compile::fragment_synth::benchmark::canonical_world_fingerprint;
        use crate::compile::fragment_synth::certification::with_certification_threads;
        use crate::compile::fragment_synth::search::SynthesisBudget;
        use crate::compile::{HierarchicalNetlist, Netlist};

        fn xor(b: &mut NetlistBuilder, x: &str, y: &str) -> String {
            let nx = b.not(x);
            let ny = b.not(y);
            let left = b.and_reduce(vec![x.to_string(), ny]);
            let right = b.and_reduce(vec![nx, y.to_string()]);
            b.or_reduce(vec![left, right])
        }

        fn full_adder(b: &mut NetlistBuilder, x: &str, y: &str, cin: &str) -> (String, String) {
            let ab = b.and_reduce(vec![x.to_string(), y.to_string()]);
            let bc = b.and_reduce(vec![y.to_string(), cin.to_string()]);
            let ac = b.and_reduce(vec![x.to_string(), cin.to_string()]);
            let cout = b.or_reduce(vec![ab, bc, ac]);
            let s1 = xor(b, x, y);
            let sum = xor(b, &s1, cin);
            (sum, cout)
        }

        pub(crate) fn ripple_adder(bits: usize) -> Netlist {
            let mut b = NetlistBuilder::new();
            let mut inputs = Vec::new();
            for i in 0..bits {
                inputs.push(format!("a{i}"));
            }
            for i in 0..bits {
                inputs.push(format!("b{i}"));
            }
            inputs.push("cin".to_string());
            let mut carry = "cin".to_string();
            let mut outputs = Vec::new();
            for i in 0..bits {
                let (sum, cout) = full_adder(&mut b, &format!("a{i}"), &format!("b{i}"), &carry);
                outputs.push(sum);
                carry = cout;
            }
            outputs.push(carry);
            Netlist {
                inputs,
                outputs,
                gates: b.into_gates(),
            }
        }

        fn decoder_2_to_4() -> Netlist {
            let mut b = NetlistBuilder::new();
            let n0 = b.not("s0");
            let n1 = b.not("s1");
            let outputs = vec![
                b.and_reduce(vec![n1.clone(), n0.clone()]),
                b.and_reduce(vec![n1, "s0".to_string()]),
                b.and_reduce(vec!["s1".to_string(), n0]),
                b.and_reduce(vec!["s1".to_string(), "s0".to_string()]),
            ];
            Netlist {
                inputs: vec!["s1".into(), "s0".into()],
                outputs,
                gates: b.into_gates(),
            }
        }

        fn mux_4_to_1() -> Netlist {
            let mut b = NetlistBuilder::new();
            let n0 = b.not("s0");
            let n1 = b.not("s1");
            let t0 = b.and_reduce(vec!["d0".to_string(), n1.clone(), n0.clone()]);
            let t1 = b.and_reduce(vec!["d1".to_string(), n1, "s0".to_string()]);
            let t2 = b.and_reduce(vec!["d2".to_string(), "s1".to_string(), n0]);
            let t3 = b.and_reduce(vec!["d3".to_string(), "s1".to_string(), "s0".to_string()]);
            let y = b.or_reduce(vec![t0, t1, t2, t3]);
            Netlist {
                inputs: vec![
                    "d0".into(),
                    "d1".into(),
                    "d2".into(),
                    "d3".into(),
                    "s1".into(),
                    "s0".into(),
                ],
                outputs: vec![y],
                gates: b.into_gates(),
            }
        }

        fn majority3() -> Netlist {
            let mut b = NetlistBuilder::new();
            let ab = b.and_reduce(vec!["a".into(), "b".into()]);
            let bc = b.and_reduce(vec!["b".into(), "c".into()]);
            let ac = b.and_reduce(vec!["a".into(), "c".into()]);
            let y = b.or_reduce(vec![ab, bc, ac]);
            Netlist {
                inputs: vec!["a".into(), "b".into(), "c".into()],
                outputs: vec![y],
                gates: b.into_gates(),
            }
        }

        fn parity4() -> Netlist {
            let mut b = NetlistBuilder::new();
            let p1 = xor(&mut b, "a", "b");
            let p2 = xor(&mut b, &p1, "c");
            let p3 = xor(&mut b, &p2, "d");
            Netlist {
                inputs: vec!["a".into(), "b".into(), "c".into(), "d".into()],
                outputs: vec![p3],
                gates: b.into_gates(),
            }
        }

        fn equal4() -> Netlist {
            let mut b = NetlistBuilder::new();
            let mut same = Vec::new();
            for i in 0..4 {
                let x = xor(&mut b, &format!("a{i}"), &format!("b{i}"));
                same.push(b.not(&x));
            }
            let y = b.and_reduce(same);
            let mut inputs = Vec::new();
            for i in 0..4 {
                inputs.push(format!("a{i}"));
            }
            for i in 0..4 {
                inputs.push(format!("b{i}"));
            }
            Netlist {
                inputs,
                outputs: vec![y],
                gates: b.into_gates(),
            }
        }

        fn wide_gate(kind: &str, width: usize) -> Netlist {
            let mut b = NetlistBuilder::new();
            let inputs = (0..width).map(|i| format!("i{i}")).collect::<Vec<_>>();
            let y = if kind == "and" {
                b.and_reduce(inputs.clone())
            } else {
                b.or_reduce(inputs.clone())
            };
            Netlist {
                inputs,
                outputs: vec![y],
                gates: b.into_gates(),
            }
        }

        fn half_adder_chain(bits: usize) -> Netlist {
            // Incrementer: carry chain of half adders.
            let mut b = NetlistBuilder::new();
            let inputs = (0..bits).map(|i| format!("a{i}")).collect::<Vec<_>>();
            let mut carry = "a0".to_string();
            let mut outputs = vec![b.not("a0")];
            for i in 1..bits {
                let sum = xor(&mut b, &format!("a{i}"), &carry);
                outputs.push(sum);
                carry = b.and_reduce(vec![format!("a{i}"), carry]);
            }
            outputs.push(carry);
            Netlist {
                inputs,
                outputs,
                gates: b.into_gates(),
            }
        }

        /// A 4-bit ALU: opcode `s1 s0` selects AND, OR, XOR or ADD of `a`
        /// and `b`; `cout` is the adder carry.
        fn alu4() -> Netlist {
            let mut b = NetlistBuilder::new();
            let ns0 = b.not("s0");
            let ns1 = b.not("s1");
            let sel = [
                b.and_reduce(vec![ns1.clone(), ns0.clone()]),
                b.and_reduce(vec![ns1.clone(), "s0".to_string()]),
                b.and_reduce(vec!["s1".to_string(), ns0.clone()]),
                b.and_reduce(vec!["s1".to_string(), "s0".to_string()]),
            ];
            let mut carry = "cin".to_string();
            let mut outputs = Vec::new();
            for i in 0..4 {
                let (x, y) = (format!("a{i}"), format!("b{i}"));
                let and = b.and_reduce(vec![x.clone(), y.clone()]);
                let or = b.or_reduce(vec![x.clone(), y.clone()]);
                let xo = xor(&mut b, &x, &y);
                let (sum, cout) = full_adder(&mut b, &x, &y, &carry);
                carry = cout;
                let picks = vec![
                    b.and_reduce(vec![and, sel[0].clone()]),
                    b.and_reduce(vec![or, sel[1].clone()]),
                    b.and_reduce(vec![xo, sel[2].clone()]),
                    b.and_reduce(vec![sum, sel[3].clone()]),
                ];
                outputs.push(b.or_reduce(picks));
            }
            outputs.push(carry);
            let mut inputs = Vec::new();
            for i in 0..4 {
                inputs.push(format!("a{i}"));
            }
            for i in 0..4 {
                inputs.push(format!("b{i}"));
            }
            inputs.extend(["cin".to_string(), "s1".to_string(), "s0".to_string()]);
            Netlist {
                inputs,
                outputs,
                gates: b.into_gates(),
            }
        }

        /// A 4x4 array multiplier: partial products summed with ripple adders.
        pub(crate) fn multiplier4() -> Netlist {
            let mut b = NetlistBuilder::new();
            let pp = |b: &mut NetlistBuilder, i: usize, j: usize| {
                b.and_reduce(vec![format!("a{i}"), format!("b{j}")])
            };
            let mut acc: Vec<String> = (0..4).map(|i| pp(&mut b, i, 0)).collect();
            let mut outputs = vec![acc[0].clone()];
            for j in 1..4 {
                let row: Vec<String> = (0..4).map(|i| pp(&mut b, i, j)).collect();
                let mut carry: Option<String> = None;
                let mut next = Vec::new();
                for i in 0..4 {
                    let x = if i + 1 < acc.len() {
                        Some(acc[i + 1].clone())
                    } else {
                        None
                    };
                    let y = row[i].clone();
                    let (sum, cout) = match (x, carry.take()) {
                        (Some(x), Some(c)) => full_adder(&mut b, &x, &y, &c),
                        (Some(x), None) => {
                            let sum = xor(&mut b, &x, &y);
                            let cout = b.and_reduce(vec![x, y]);
                            (sum, cout)
                        }
                        (None, Some(c)) => {
                            let sum = xor(&mut b, &y, &c);
                            let cout = b.and_reduce(vec![y, c]);
                            (sum, cout)
                        }
                        (None, None) => (y, String::new()),
                    };
                    next.push(sum);
                    if !cout.is_empty() {
                        carry = Some(cout);
                    }
                }
                if let Some(c) = carry {
                    next.push(c);
                }
                outputs.push(next[0].clone());
                acc = next;
            }
            outputs.extend(acc.into_iter().skip(1));
            let mut inputs = Vec::new();
            for i in 0..4 {
                inputs.push(format!("a{i}"));
            }
            for i in 0..4 {
                inputs.push(format!("b{i}"));
            }
            Netlist {
                inputs,
                outputs,
                gates: b.into_gates(),
            }
        }

        /// A fuller 4-bit ALU with a three-bit opcode: 000 AND, 001 OR,
        /// 010 XOR, 011 NOT a, 100 ADD, 101 SUB (a + !b + 1), 110 SHL a,
        /// 111 pass a.  Outputs r0..r3, the adder carry and a zero flag.
        pub(crate) fn alu4_full() -> Netlist {
            let mut b = NetlistBuilder::new();
            let n = [b.not("s0"), b.not("s1"), b.not("s2")];
            let s = ["s0".to_string(), "s1".to_string(), "s2".to_string()];
            let mut sel = Vec::new();
            for op in 0..8u8 {
                let bit = |k: usize| -> String {
                    if (op >> k) & 1 == 1 {
                        s[k].clone()
                    } else {
                        n[k].clone()
                    }
                };
                let bits = vec![bit(0), bit(1), bit(2)];
                sel.push(b.and_reduce(bits));
            }
            // Subtraction shares the adder: the b operand is inverted and
            // the carry-in forced high when s0 is set with s2.
            let sub = b.and_reduce(vec!["s2".to_string(), n[1].clone(), "s0".to_string()]);
            let nsub = b.not(&sub);
            let mut carry = sub.clone();
            let mut results = Vec::new();
            let mut previous_a: Option<String> = None;
            for i in 0..4 {
                let (x, y) = (format!("a{i}"), format!("b{i}"));
                let ny = b.not(&y);
                let operand = {
                    let keep = b.and_reduce(vec![y.clone(), nsub.clone()]);
                    let flip = b.and_reduce(vec![ny.clone(), sub.clone()]);
                    b.or_reduce(vec![keep, flip])
                };
                let (sum, cout) = full_adder(&mut b, &x, &operand, &carry);
                carry = cout;
                let and = b.and_reduce(vec![x.clone(), y.clone()]);
                let or = b.or_reduce(vec![x.clone(), y.clone()]);
                let xo = xor(&mut b, &x, &y);
                let nx = b.not(&x);
                let shifted = previous_a.clone().unwrap_or_else(|| {
                    // Bit 0 of a left shift is zero: an AND of a signal with
                    // its own inverse.
                    let zero = b.and_reduce(vec![x.clone(), nx.clone()]);
                    zero
                });
                let picks = vec![
                    b.and_reduce(vec![and, sel[0].clone()]),
                    b.and_reduce(vec![or, sel[1].clone()]),
                    b.and_reduce(vec![xo, sel[2].clone()]),
                    b.and_reduce(vec![nx, sel[3].clone()]),
                    b.and_reduce(vec![sum.clone(), sel[4].clone()]),
                    b.and_reduce(vec![sum, sel[5].clone()]),
                    b.and_reduce(vec![shifted, sel[6].clone()]),
                    b.and_reduce(vec![x.clone(), sel[7].clone()]),
                ];
                let r = b.or_reduce(picks);
                results.push(r);
                previous_a = Some(x);
            }
            let any = b.or_reduce(results.clone());
            let zero = b.not(&any);
            let mut outputs = results;
            outputs.push(carry);
            outputs.push(zero);
            let mut inputs = Vec::new();
            for i in 0..4 {
                inputs.push(format!("a{i}"));
            }
            for i in 0..4 {
                inputs.push(format!("b{i}"));
            }
            inputs.extend(["s2".to_string(), "s1".to_string(), "s0".to_string()]);
            Netlist {
                inputs,
                outputs,
                gates: b.into_gates(),
            }
        }

        /// The `REDA_EXTRA_CIRCUITS` subset/resume filter every corpus
        /// harness here honours, in one place: no variable set runs the whole
        /// corpus, a comma-separated list runs exactly the cases it names.
        fn case_is_selected(name: &str) -> bool {
            std::env::var("REDA_EXTRA_CIRCUITS")
                .map_or(true, |selected| selected.split(',').any(|s| s == name))
        }

        fn run_cases(cases: Vec<(String, Netlist)>) {
            let mut failures = Vec::new();
            for (name, netlist) in cases {
                if !case_is_selected(&name) {
                    continue;
                }
                let started = std::time::Instant::now();
                let result = compile_fragment_synth(
                    SynthesisInput {
                        lowered: &netlist,
                        source_provenance: None,
                        pins: None,
                    },
                    SynthesisBudget::Evaluations(0),
                );
                match result {
                    Ok(result) => eprintln!(
                        "CIRCUIT {name}: OK gates={} ticks={} blocks={} in {:?}",
                        netlist.gates.len(),
                        result.metrics.quality.observed_settle,
                        result.metrics.quality.non_air_blocks,
                        started.elapsed()
                    ),
                    Err(error) => {
                        eprintln!(
                            "CIRCUIT {name}: ERR gates={} {error} in {:?}",
                            netlist.gates.len(),
                            started.elapsed()
                        );
                        failures.push(name);
                    }
                }
            }
            assert_eq!(failures, Vec::<String>::new());
        }

        /// The four large flat cases, shared by the release corpus test
        /// below and by the thread-count matrix, so neither list can drift
        /// from the other.
        fn large_circuit_cases() -> Vec<(String, Netlist)> {
            vec![
                ("ripple_adder8".into(), ripple_adder(8)),
                ("alu4".into(), alu4()),
                ("alu4_full".into(), alu4_full()),
                ("multiplier4".into(), multiplier4()),
            ]
        }

        #[test]
        #[ignore = "release-only: run with `cargo test --release --lib large_circuits -- --ignored`"]
        fn every_large_circuit_certifies_with_the_topology_aware_seed() {
            run_cases(large_circuit_cases());
        }

        /// The sixteen extra flat cases, shared exactly as
        /// [`large_circuit_cases`] is.
        fn extra_circuit_cases() -> Vec<(String, Netlist)> {
            let mut cases: Vec<(String, Netlist)> = Vec::new();
            for segment in 1..7 {
                let (netlist, _) =
                    crate::circuits::seven_segment::build_single_segment_netlist(segment);
                cases.push((
                    format!("segment_{}", (b'a' + segment as u8) as char),
                    netlist,
                ));
            }
            cases.push(("majority3".into(), majority3()));
            cases.push(("parity4".into(), parity4()));
            cases.push(("decoder_2_to_4".into(), decoder_2_to_4()));
            cases.push(("mux_4_to_1".into(), mux_4_to_1()));
            cases.push(("and8".into(), wide_gate("and", 8)));
            cases.push(("or8".into(), wide_gate("or", 8)));
            cases.push(("equal4".into(), equal4()));
            cases.push(("incrementer4".into(), half_adder_chain(4)));
            cases.push(("ripple_adder2".into(), ripple_adder(2)));
            cases.push(("ripple_adder4".into(), ripple_adder(4)));
            cases
        }

        #[test]
        #[ignore = "release-only: run with `cargo test --release --lib extra_circuits -- --ignored`"]
        fn every_extra_circuit_certifies_with_the_topology_aware_seed() {
            run_cases(extra_circuit_cases());
        }

        /// Every distinct module reachable from `design.top` (`top` itself
        /// included), walking `ModuleInstance::module` transitively.
        ///
        /// Private, and only ever called on an already-specialised design --
        /// see [`compiled_module_count`], the sole caller, for why the design
        /// must be `HierarchicalNetlist::specialise_constants`'s output
        /// rather than the design as originally built.
        fn distinct_modules(design: &crate::compile::HierarchicalNetlist) -> usize {
            use std::collections::BTreeSet;
            let mut seen = BTreeSet::new();
            let mut pending = vec![design.top.clone()];
            while let Some(name) = pending.pop() {
                if !seen.insert(name.clone()) {
                    continue;
                }
                if let Some(module) = design.modules.get(&name) {
                    for instance in &module.instances {
                        pending.push(instance.module.clone());
                    }
                }
            }
            seen.len()
        }

        /// "How many distinct modules does `compile_hierarchical` compile
        /// once each for `design`" -- the count the hierarchical harness
        /// reports as `blocks_compiled=`.
        ///
        /// Specialises `design`'s constant-tied ports first, then walks the
        /// result with [`distinct_modules`], because `compile_hierarchical`
        /// itself calls `HierarchicalNetlist::specialise_constants` *before*
        /// deriving the module set it compiles (`instantiated_modules` in
        /// `hierarchy_api.rs`, private to that module and therefore not
        /// reusable here): an instance whose port is tied to a constant
        /// compiles a structurally different clone
        /// (`"<module>@<port>=<bit>"`, with fewer inputs than the original --
        /// see `specialise_constants`'s doc comment), and that clone, not the
        /// original module, is what actually gets compiled. Walking the raw
        /// design undercounts by exactly the number of such clones
        /// (`alu4_full`'s bit-0 `shift_in` tie is one example: the raw walk
        /// sees `top`+`slice` == 2, but three modules are actually compiled --
        /// `top`, `slice`, `slice@shift_in=0`).
        ///
        /// Folding the specialise-then-count sequence into one function
        /// removes the precondition entirely rather than merely documenting
        /// it: every caller -- the harness and the unit test below -- passes
        /// the design as originally built and gets the post-specialisation
        /// count back, so there is no raw-design call site left that could
        /// silently skip the step. This also walks the *public*
        /// `specialise_constants()` output rather than reaching into
        /// `hierarchy_api`'s private `instantiated_modules`, so that a future
        /// change to what gets specialised cannot leave this count silently
        /// stale: both this walk and `compile_hierarchical` start from the
        /// same public transform, so whatever it produces is what both agree
        /// on.
        fn compiled_module_count(
            design: &crate::compile::HierarchicalNetlist,
        ) -> Result<usize, crate::compile::HierarchyError> {
            design
                .specialise_constants()
                .map(|specialised| distinct_modules(&specialised))
        }

        /// Pins [`compiled_module_count`] against a constant-tied circuit and
        /// a constant-free one, passing each the design as originally built
        /// (not pre-specialised) -- not against a released binary, so this
        /// never touches `compile_hierarchical` and runs in milliseconds:
        /// `alu4_full` ties bit 0's `shift_in` to `PortBinding::Zero`, which
        /// must specialise `slice` into a separate `slice@shift_in=0` clone
        /// (`top`, `slice`, `slice@shift_in=0` == 3 -- this would read 2 if
        /// `compiled_module_count` ever stopped specialising before
        /// counting); `ripple_adder8` ties no port to a constant anywhere, so
        /// its count is unaffected by specialisation (`top`, `full_adder` ==
        /// 2).
        #[test]
        fn compiled_module_count_counts_constant_specialised_clones_separately() {
            use crate::circuits::hierarchical_builder::circuits as h;

            assert_eq!(
                compiled_module_count(&h::alu4_full()).expect("alu4_full's constant tie folds"),
                3
            );
            assert_eq!(
                compiled_module_count(&h::ripple_adder(8))
                    .expect("ripple_adder8 has no constant ties to fold"),
                2
            );
        }

        /// The second characterization point's budget: `REDA_RETENTION_BUDGET`
        /// when set, otherwise `u64::MAX` -- proposal-stream exhaustion, the
        /// only point a retention-policy change can move. Setting it to `0`
        /// collapses both points onto today's budget-zero numbers. A value
        /// that does not parse is a typo in the run, so it panics by name
        /// rather than silently exhausting.
        fn retention_budget() -> u64 {
            match std::env::var("REDA_RETENTION_BUDGET") {
                Ok(raw) => raw.trim().parse::<u64>().unwrap_or_else(|error| {
                    panic!("REDA_RETENTION_BUDGET={raw:?} is not a u64: {error}")
                }),
                Err(_) => u64::MAX,
            }
        }

        /// The one place a retention record is formatted, so a pre-feature
        /// transcript and a post-feature one cannot diverge in shape. All four
        /// `QualityKey` fields are printed rather than the two the `CIRCUIT`
        /// line carries, because the acceptance rule this baseline exists to
        /// characterize is defined over all four; `evaluations_used` and
        /// `stop_reason` say whether the proposal stream truly exhausted or
        /// merely hit its cap, the fingerprints say which case was compiled
        /// and which candidate won, and each `ProposalTrace` follows on its
        /// own line.
        fn print_retention_record(
            name: &str,
            budget: u64,
            result: &SynthesisResult,
            elapsed_ms: u128,
        ) {
            let q = result.metrics.quality;
            println!(
                "RETENTION name={name} budget={budget} settle={} blocks={} volume={} static={} evals={} stop={:?} wall_ms={elapsed_ms} case={} candidate={}",
                q.observed_settle,
                q.non_air_blocks,
                q.occupied_volume,
                q.static_routed_delay.0,
                result.evaluations_used,
                result.stop_reason,
                result.case_fingerprint.as_str(),
                result.candidate_fingerprint.as_str(),
            );
            for entry in &result.trace {
                println!("TRACE name={name} entry={entry:?}");
            }
        }

        /// [`run_cases`]'s hierarchical counterpart: same `REDA_EXTRA_CIRCUITS`
        /// filter and the same "assert no failures" shape, but driving
        /// `compile_hierarchical` on a [`crate::compile::HierarchicalNetlist`]
        /// instead of `compile_fragment_synth` on an already-flat [`Netlist`].
        /// Prints enough for a release run's log to be self-explaining on its
        /// own: the flat gate count (off the *specialised* design, the same
        /// one `compile_hierarchical` itself derives before lowering) and the
        /// distinct-module count via [`compiled_module_count`], the settle
        /// ticks and non-air block count off the certified candidate's own
        /// metrics, and wall time -- and, on failure, the error
        /// `compile_hierarchical` returned, so a refusal names itself instead
        /// of only tripping the final assertion.
        ///
        /// Each case is compiled once per entry in `points`, in the order
        /// given, and every compile prints its `CIRCUIT` line -- unchanged in
        /// shape -- followed by its own [`print_retention_record`], which is
        /// the line that names the budget. A failure is reported under the
        /// point's label, so a case that certifies at one budget and refuses
        /// at another says which.
        fn run_hierarchical_cases(
            cases: Vec<(String, crate::compile::HierarchicalNetlist)>,
            points: &[(&str, SynthesisBudget)],
        ) {
            use crate::compile::compile_hierarchical;

            let mut failures = Vec::new();
            for (name, design) in cases {
                if !case_is_selected(&name) {
                    continue;
                }
                let gate_count = design
                    .specialise_constants()
                    .ok()
                    .and_then(|specialised| specialised.flatten().ok())
                    .map(|(flat, _)| flat.gates.len());
                let blocks_compiled = compiled_module_count(&design).ok();
                let gates_str = gate_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "?".to_string());
                let blocks_compiled_str = blocks_compiled
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "?".to_string());
                for (label, budget) in points {
                    let SynthesisBudget::Evaluations(evaluations) = *budget else {
                        panic!("{name} {label}: retention points are evaluation budgets");
                    };
                    let started = std::time::Instant::now();
                    match compile_hierarchical(&design, *budget, None) {
                        Ok(result) => {
                            let elapsed = started.elapsed();
                            eprintln!(
                                "CIRCUIT {name} (hierarchical): OK gates={gates_str} \
                                 blocks_compiled={blocks_compiled_str} \
                                 ticks={} blocks={} in {elapsed:?}",
                                result.metrics.quality.observed_settle,
                                result.metrics.quality.non_air_blocks,
                            );
                            print_retention_record(
                                &name,
                                evaluations,
                                &result,
                                elapsed.as_millis(),
                            );
                        }
                        Err(error) => {
                            eprintln!(
                                "CIRCUIT {name} (hierarchical): ERR gates={gates_str} \
                                 blocks_compiled={blocks_compiled_str} {error} in {:?}",
                                started.elapsed()
                            );
                            failures.push(format!("{name} {label}"));
                        }
                    }
                }
            }
            assert_eq!(failures, Vec::<String>::new());
        }

        /// The four hierarchy cases, shared exactly as
        /// [`large_circuit_cases`] is.
        fn hierarchical_circuit_cases() -> Vec<(String, HierarchicalNetlist)> {
            use crate::circuits::hierarchical_builder::circuits as h;
            vec![
                ("ripple_adder8".into(), h::ripple_adder(8)),
                ("alu4_full".into(), h::alu4_full()),
                ("multiplier4".into(), h::multiplier4()),
                ("alu8".into(), h::alu8()),
            ]
        }

        /// The plan's hierarchical acceptance corpus: the same four shapes
        /// [`every_large_circuit_certifies_with_the_topology_aware_seed`]
        /// certifies flat, this time built as a real module hierarchy
        /// (`crate::circuits::hierarchical_builder::circuits`) and compiled
        /// through [`compile_hierarchical`] end to end -- leaf blocks
        /// compiled once and stamped at every instance, then spliced into
        /// one flat candidate and certified exactly as the flat front door
        /// certifies its own candidate.
        ///
        /// `alu8` is also covered, alone, by
        /// `compile::fragment_synth::hierarchy_api::tests::alu8_the_three_level_acceptance_circuit_certifies`
        /// (with its own per-bit `output_positions` assertions, which this
        /// uniform harness does not make). That test is deliberately left in
        /// place rather than deleted: it is the one place `alu8`'s output
        /// wiring is checked bit by bit. Including `alu8` here too is not
        /// pointless duplication of *coverage* -- it is what makes this
        /// harness actually the acceptance corpus (four circuits, one
        /// uniform report line each) rather than three plus a footnote. The
        /// cost is a second ~371s compile of `alu8` when this whole test
        /// runs unfiltered; a run that only wants the fast three can set
        /// `REDA_EXTRA_CIRCUITS=ripple_adder8,alu4_full,multiplier4` to skip
        /// it, since `hierarchy_api`'s own test already certifies `alu8` on
        /// its own.
        ///
        /// Each case is certified at two budget points. Budget zero is the
        /// acceptance statement this test has always made -- the seed alone
        /// certifies -- and is still every case's first `CIRCUIT` line. The
        /// second is [`retention_budget`], the point at which a proposal can
        /// actually be accepted. Recording both through
        /// [`print_retention_record`] makes this a re-runnable
        /// characterization as well as an acceptance run; the cost is that
        /// every case compiles twice, which `REDA_RETENTION_BUDGET=0` avoids.
        #[test]
        #[ignore = "release-only: run with `cargo test --release --lib every_hierarchical_circuit -- --ignored --nocapture`"]
        fn every_hierarchical_circuit_certifies_through_module_floorplan() {
            let points = [
                ("budget=0", SynthesisBudget::Evaluations(0)),
                (
                    "budget=retention",
                    SynthesisBudget::Evaluations(retention_budget()),
                ),
            ];
            run_hierarchical_cases(hierarchical_circuit_cases(), &points);
        }

        /// Task 4's target oracle: does `ripple_adder8`'s hierarchical
        /// block-edge proposal stream ever reach BOTH quality gates --
        /// `observed_settle <= 474` and `non_air_blocks <= 100_615` -- on the
        /// SAME certified result, at any evaluation budget up to full stream
        /// exhaustion? See
        /// `.superpowers/sdd/2026-09-05-timing-aware-module-floorplan/task-4-report.md`
        /// for how this measurement is used.
        ///
        /// Budgets 0/1/2/4 are the same deterministic staircase
        /// `hierarchy_api::tests::evaluation_budgets_0_1_2_4_are_deterministic_quality_staircases`
        /// already proves for a small fixture; the fifth point uses
        /// `SynthesisBudget::Evaluations(u64::MAX)` -- no arbitrary cap, so
        /// the run can only end by the finite block-edge proposal stream
        /// itself running out (`HierarchicalProposalStream::next` emits
        /// exactly one proposal per edge, in `explicit_block_edges`' order).
        /// This last point must ALWAYS report `StopReason::ProposalStreamExhausted`
        /// with `evaluations_used < u64::MAX`, regardless of whether an
        /// earlier, smaller budget already met both quality gates --
        /// otherwise a smaller budget "passing" would mask the true
        /// exhaustion point silently hitting an evaluation cap instead.
        #[test]
        #[ignore = "release-only: run with `cargo test --release --lib ripple_adder8_hierarchical_budget_target_oracle -- --ignored --nocapture`"]
        fn ripple_adder8_hierarchical_budget_target_oracle() {
            use crate::circuits::hierarchical_builder::circuits as h;
            use crate::compile::compile_hierarchical;
            use crate::compile::fragment_synth::search::StopReason;

            const MAX_OBSERVED_SETTLE: u64 = 474;
            const MAX_NON_AIR_BLOCKS: u64 = 100_615;

            let design = h::ripple_adder(8);
            let points: [(&str, SynthesisBudget); 5] = [
                ("budget=0", SynthesisBudget::Evaluations(0)),
                ("budget=1", SynthesisBudget::Evaluations(1)),
                ("budget=2", SynthesisBudget::Evaluations(2)),
                ("budget=4", SynthesisBudget::Evaluations(4)),
                ("budget=exhaustion", SynthesisBudget::Evaluations(u64::MAX)),
            ];

            let mut target_met = false;
            let mut last = None;
            for (label, budget) in points {
                let started = std::time::Instant::now();
                let result = compile_hierarchical(&design, budget, None)
                    .unwrap_or_else(|error| panic!("{label}: ripple_adder8 must certify: {error}"));
                let quality = result.metrics.quality;
                eprintln!(
                    "TARGET {label}: ticks={} blocks={} static_delay={:?} \
                     evaluations_used={} stop_reason={:?} case_fingerprint={} \
                     candidate_fingerprint={} trace={:?} in {:?}",
                    quality.observed_settle,
                    quality.non_air_blocks,
                    quality.static_routed_delay,
                    result.evaluations_used,
                    result.stop_reason,
                    result.case_fingerprint.as_str(),
                    result.candidate_fingerprint.as_str(),
                    result.trace,
                    started.elapsed(),
                );
                if quality.observed_settle <= MAX_OBSERVED_SETTLE
                    && quality.non_air_blocks <= MAX_NON_AIR_BLOCKS
                {
                    target_met = true;
                }
                last = Some((label, result.stop_reason, result.evaluations_used));
            }

            let (label, stop_reason, evaluations_used) =
                last.expect("five budget points always run");
            assert_eq!(
                stop_reason,
                StopReason::ProposalStreamExhausted,
                "{label}: the last (largest) budget point uses \
                 SynthesisBudget::Evaluations(u64::MAX), an uncapped run, so it must \
                 always end by exhausting the finite block-edge proposal stream -- \
                 regardless of whether an earlier point already met both quality \
                 gates (target_met={target_met}) -- an EvaluationBudget stop here \
                 would mean the run was silently capped instead of truly exhausted"
            );
            assert!(
                evaluations_used < u64::MAX,
                "{label}: stream exhaustion must use fewer than u64::MAX evaluations, \
                 got {evaluations_used}"
            );
        }

        /// Every stable field of a [`SynthesisResult`], compared exactly as
        /// `hierarchy_api::tests::parallel_and_sequential_block_compiles_agree`
        /// compares its own one-vs-many pair.
        ///
        /// `CandidateMetrics` is one comparison covering many of Task 4's
        /// rows at once: quality, the transition manifest hash, count and
        /// cap, the worst transition indices, and the equivalence-certificate,
        /// timing-graph, candidate and emitted-world fingerprints. Terminal
        /// classification and the cap-work counters ride each
        /// `ProposalTrace`, so the trace comparison owns those rows.
        ///
        /// The one row no `SynthesisResult` field exposes is the
        /// per-transition manifest measurements; those are compared directly
        /// on `CertifiedCandidate` at one, two and four workers by
        /// `certification::tests::certified_candidate_is_identical_at_one_two_and_four_workers`,
        /// and are not re-derived here.
        fn assert_same_result(
            label: &str,
            reference: &SynthesisResult,
            observed: &SynthesisResult,
        ) {
            assert_eq!(
                observed.case_fingerprint, reference.case_fingerprint,
                "{label}: case fingerprint"
            );
            assert_eq!(
                observed.candidate_fingerprint, reference.candidate_fingerprint,
                "{label}: candidate fingerprint"
            );
            assert_eq!(observed.metrics, reference.metrics, "{label}: metrics");
            assert_eq!(
                canonical_world_fingerprint(&observed.compiled.world),
                canonical_world_fingerprint(&reference.compiled.world),
                "{label}: emitted world"
            );
            assert_eq!(
                observed.compiled.input_positions, reference.compiled.input_positions,
                "{label}: input positions"
            );
            assert_eq!(
                observed.compiled.output_positions, reference.compiled.output_positions,
                "{label}: output positions"
            );
            assert_eq!(
                observed.compiled.gate_output_positions, reference.compiled.gate_output_positions,
                "{label}: gate output positions"
            );
            assert_eq!(
                observed.compiled.gate_facings, reference.compiled.gate_facings,
                "{label}: gate facings"
            );
            assert_eq!(
                observed.compiled.observations, reference.compiled.observations,
                "{label}: circuit observations"
            );
            assert_eq!(
                observed.evaluations_used, reference.evaluations_used,
                "{label}: evaluations used"
            );
            assert_eq!(
                observed.stop_reason, reference.stop_reason,
                "{label}: stop reason"
            );
            assert_eq!(observed.trace, reference.trace, "{label}: proposal trace");
        }

        /// Task 4's 1/2/4 acceptance matrix over one corpus: compile every
        /// case inside a one-worker scope as the reference, then again at
        /// each remaining count, and require the whole stable result back
        /// each time.
        ///
        /// The budget is one evaluation, not zero: a zero-evaluation compile
        /// has an empty trace, which would leave the proposal-trace, terminal
        /// and cap-work rows vacuous. Every reference compile must therefore
        /// evaluate a proposal, so a case that never reaches its proposal
        /// stream fails here instead of quietly proving nothing.
        ///
        /// Cases are subset by `REDA_EXTRA_CIRCUITS` through the same
        /// [`case_is_selected`] filter the sibling corpus harnesses use, so an
        /// interrupted release run can resume on the cases it has left.
        ///
        /// A refusal at any count fails the matrix: these corpora certify.
        fn assert_matrix_agrees<T, E: std::fmt::Display>(
            cases: Vec<(String, T)>,
            compile: impl Fn(&T) -> Result<SynthesisResult, E>,
        ) {
            let counts = [1, 2, 4];
            let mut ran = 0usize;
            for (name, case) in &cases {
                if !case_is_selected(name) {
                    continue;
                }
                ran += 1;
                let run = |threads: usize| {
                    let started = std::time::Instant::now();
                    let result = with_certification_threads(threads, || compile(case))
                        .unwrap_or_else(|error| {
                            panic!("{name} must certify at {threads} worker(s): {error}")
                        });
                    eprintln!(
                        "THREADS {name} workers={threads}: ticks={} blocks={} \
                         evaluations={} trace={} in {:?}",
                        result.metrics.quality.observed_settle,
                        result.metrics.quality.non_air_blocks,
                        result.evaluations_used,
                        result.trace.len(),
                        started.elapsed()
                    );
                    result
                };
                let reference = run(counts[0]);
                assert!(
                    !reference.trace.is_empty(),
                    "{name}: the reference compile must evaluate one proposal"
                );
                for &threads in &counts[1..] {
                    assert_same_result(
                        &format!("{name} at {threads} worker(s)"),
                        &reference,
                        &run(threads),
                    );
                }
            }
            assert!(
                ran > 0,
                "REDA_EXTRA_CIRCUITS={:?} selected no case, so nothing was compared",
                std::env::var("REDA_EXTRA_CIRCUITS").ok()
            );
        }

        fn assert_thread_counts_agree(cases: Vec<(String, Netlist)>) {
            assert_matrix_agrees(cases, |netlist: &Netlist| {
                compile_fragment_synth(
                    SynthesisInput {
                        lowered: netlist,
                        source_provenance: None,
                        pins: None,
                    },
                    SynthesisBudget::Evaluations(1),
                )
            });
        }

        fn assert_hierarchical_thread_counts_agree(cases: Vec<(String, HierarchicalNetlist)>) {
            assert_matrix_agrees(cases, |design: &HierarchicalNetlist| {
                crate::compile::compile_hierarchical(design, SynthesisBudget::Evaluations(1), None)
            });
        }

        /// The matrix over the sixteen flat extra cases.
        ///
        /// The six-case `fragment_acceptance` corpus and the pinned
        /// seven-segment IO contract are covered by Task 4's two command-line
        /// rows -- the `fragment_acceptance` binary run at each setting and
        /// `build_circuit_pins::compile_hierarchical_preserves_the_checked_seven_segment_pin_contract`
        /// -- and the refusal rows by
        /// `certification::tests::exhaustive_cap_refusal_reports_the_same_lowest_mask_at_every_worker_count`,
        /// `certification::tests::simulator_event_cap_refuses_instead_of_returning_a_capped_score`,
        /// `certification::tests::divergence_is_a_named_transition_refusal`
        /// and
        /// `certification::tests::earlier_manifest_error_wins_over_a_later_worker_panic`,
        /// so neither is repeated over these corpora.
        #[test]
        #[ignore = "release-only, serial: run with `cargo test --release --lib certification_thread_counts -- --ignored --nocapture --test-threads=1`"]
        fn every_extra_circuit_agrees_across_certification_thread_counts() {
            assert_thread_counts_agree(extra_circuit_cases());
        }

        /// The matrix over the four flat large cases.
        #[test]
        #[ignore = "release-only, serial: run with `cargo test --release --lib certification_thread_counts -- --ignored --nocapture --test-threads=1`"]
        fn every_large_circuit_agrees_across_certification_thread_counts() {
            assert_thread_counts_agree(large_circuit_cases());
        }

        /// The matrix over the four hierarchy cases, where leaf workers, the
        /// sequential parent loop and the top proposal stream all carry real
        /// work -- `parallel_and_sequential_block_compiles_agree` proves the
        /// same equality only on its seconds-sized three-level fixture.
        #[test]
        #[ignore = "release-only, serial: run with `cargo test --release --lib certification_thread_counts -- --ignored --nocapture --test-threads=1`"]
        fn every_hierarchical_circuit_agrees_across_certification_thread_counts() {
            assert_hierarchical_thread_counts_agree(hierarchical_circuit_cases());
        }
    }

    #[test]
    fn full_adder_routes_never_occupy_or_hug_a_later_access_corridor() {
        let (netlist, _) = crate::circuits::full_adder::build_full_adder_netlist();

        let (result, violations) = build_checking_corridors(&netlist, None);

        assert_eq!(violations, Vec::<String>::new());
        result.expect("full_adder must route and certify once corridors are protected");
    }
    /// The parent's own planning netlist for one stamped full adder:
    /// three primary inputs feed the block, the block's first output feeds
    /// one real NOR gate whose output is exported, and the block's second
    /// output is exported directly.
    fn parent_planning_over_one_full_adder(
        block: &CompiledBlock,
    ) -> (Netlist, Vec<String>, Vec<String>) {
        let inputs = block.lowered.inputs.clone();
        // Parent signals carrying the block's outputs, in declared order.
        let carried = vec!["s0".to_string(), "s1".to_string()];
        let mut gates = vec![Gate {
            name: "g0".into(),
            inputs: vec![carried[0].clone()],
            output: "z".into(),
            kind: GateKind::Nor(1),
        }];
        // The synthetic `Buf` tail `with_blocks` expects: one row per block
        // output, in declared order, after every real gate.
        for (index, signal) in carried.iter().enumerate() {
            gates.push(Gate {
                name: format!("u0.{index}"),
                inputs: inputs.clone(),
                output: signal.clone(),
                kind: GateKind::Buf,
            });
        }
        let planning = Netlist {
            inputs: inputs.clone(),
            outputs: vec!["z".to_string(), carried[1].clone()],
            gates,
        };
        (planning, inputs, carried)
    }

    fn shift(at: Anchor, offset: Offset) -> Anchor {
        Anchor {
            x: at.x + offset.dx,
            y: at.y + offset.dy,
            z: at.z + offset.dz,
        }
    }

    #[test]
    fn a_parent_routes_into_and_out_of_a_block_with_repeaters_at_the_boundary() {
        let (library, config) = default_services_parts();
        let services = services(&library, &config);
        let fa = crate::compile::lowering::lower_optimised(
            &crate::circuits::full_adder::build_full_adder_netlist().0,
        )
        .unwrap();
        let block =
            crate::compile::fragment_synth::blocks::compile_block("full_adder", &fa, services)
                .expect("the full adder compiles as a block");
        let (planning, block_inputs, block_outputs) = parent_planning_over_one_full_adder(&block);
        let specs = [crate::compile::fragment_synth::instance_graph::BlockSpec {
            name: "u0",
            block: 0,
            inputs: &block_inputs,
            outputs: &block_outputs,
        }];
        let graph = InstanceGraph::with_blocks(&planning, &library, &specs).expect("parent graph");
        let planned = plan_parent_with_services(
            SeedInput {
                lowered: &planning,
                source_provenance: None,
                pins: None,
            },
            services,
            graph,
            ParentBlocks {
                compiled: std::slice::from_ref(&block),
            },
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("plans");

        let block_id = planned.candidate.instances.blocks[0].id;
        let offset = planned.block_offsets[&block_id];

        // Every parent route into the block ends in a repeater on the
        // block's lever cell.  A repeater's `facing` names the side its
        // input arrives on, so the repeater that drives the block's own
        // root dust (one cell east of the lever) faces West.
        for (name, port) in &block.inputs {
            let lever = shift(port.cell, offset);
            let branch = planned
                .candidate
                .routes
                .values()
                .flat_map(|route| &route.branches)
                .find(|branch| branch.terminal.at == lever)
                .unwrap_or_else(|| panic!("no parent route reaches block input {name}"));
            assert_eq!(branch.terminal.state.kind, BlockKind::Repeater);
            assert_eq!(branch.terminal.state.facing, Some(Facing::West));
        }
        // Every parent route out of the block starts on the block's lamp cell.
        for (name, port) in &block.outputs {
            let lamp = shift(port.cell, offset);
            assert!(
                planned
                    .candidate
                    .routes
                    .values()
                    .any(|route| route.branches.iter().any(|branch| branch.root == lamp)),
                "no parent route leaves block output {name}"
            );
        }
        // The block body is reserved: no parent route cell lies inside the
        // block's box except on one of its own port cells.
        let inside = |at: Anchor| {
            at.x >= block.bounds.min.x + offset.dx
                && at.x <= block.bounds.max.x + offset.dx
                && at.z >= block.bounds.min.z + offset.dz
                && at.z <= block.bounds.max.z + offset.dz
        };
        for route in planned.candidate.routes.values() {
            for cell in &route.cells {
                let on_port = block
                    .inputs
                    .values()
                    .chain(block.outputs.values())
                    .any(|port| shift(port.cell, offset) == cell.at);
                assert!(
                    !inside(cell.at) || on_port,
                    "route cell {:?} inside the block",
                    cell.at
                );
            }
        }
    }

    #[test]
    fn block_placement_override_moves_only_the_selected_block_body_and_ports() {
        let (library, config) = default_services_parts();
        let lowered = crate::compile::lowering::lower_optimised(&not_netlist()).unwrap();
        let compiled = crate::compile::fragment_synth::blocks::compile_block(
            "not",
            &lowered,
            services(&library, &config),
        )
        .expect("the not gate compiles as a block");
        let blocks = [compiled.clone(), compiled.clone()];
        let left_inputs = vec!["a0".to_string()];
        let right_inputs = vec!["a1".to_string()];
        let left_outputs = vec!["y0".to_string()];
        let right_outputs = vec!["y1".to_string()];
        let planning = Netlist {
            inputs: vec![left_inputs[0].clone(), right_inputs[0].clone()],
            outputs: vec![left_outputs[0].clone(), right_outputs[0].clone()],
            gates: vec![
                Gate {
                    name: "left.0".into(),
                    inputs: left_inputs.clone(),
                    output: left_outputs[0].clone(),
                    kind: GateKind::Buf,
                },
                Gate {
                    name: "right.0".into(),
                    inputs: right_inputs.clone(),
                    output: right_outputs[0].clone(),
                    kind: GateKind::Buf,
                },
            ],
        };
        let specs = [
            crate::compile::fragment_synth::instance_graph::BlockSpec {
                name: "left",
                block: 0,
                inputs: &left_inputs,
                outputs: &left_outputs,
            },
            crate::compile::fragment_synth::instance_graph::BlockSpec {
                name: "right",
                block: 1,
                inputs: &right_inputs,
                outputs: &right_outputs,
            },
        ];
        let baseline_graph =
            InstanceGraph::with_blocks(&planning, &library, &specs).expect("parent graph");
        let block_ids = baseline_graph
            .blocks
            .iter()
            .map(|block| block.id)
            .collect::<Vec<_>>();
        let baseline = plan_parent_with_services(
            SeedInput {
                lowered: &planning,
                source_provenance: None,
                pins: None,
            },
            services(&library, &config),
            baseline_graph,
            ParentBlocks { compiled: &blocks },
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .expect("baseline parent plans");
        let (dx, dz) = (4, -3);
        let overridden = plan_parent_with_services(
            SeedInput {
                lowered: &planning,
                source_provenance: None,
                pins: None,
            },
            services(&library, &config),
            InstanceGraph::with_blocks(&planning, &library, &specs).expect("parent graph"),
            ParentBlocks { compiled: &blocks },
            &BTreeMap::new(),
            &BTreeMap::from([(block_ids[0], BlockPlacementOffset { dx, dz })]),
        )
        .expect("overridden parent plans");

        let body = |planned: &PlannedParent, block| {
            planned.candidate.placements[&PrimitiveId {
                instance: block,
                node: TopologyNodeId(0),
            }]
                .blocks
                .iter()
                .map(|cell| cell.at)
                .collect::<BTreeSet<_>>()
        };
        let shifted = |at: Anchor| Anchor {
            x: at.x + dx,
            z: at.z + dz,
            ..at
        };
        assert_eq!(
            body(&overridden, block_ids[0]),
            body(&baseline, block_ids[0])
                .into_iter()
                .map(shifted)
                .collect(),
        );
        assert_eq!(
            body(&overridden, block_ids[1]),
            body(&baseline, block_ids[1]),
        );

        for (block, offset) in [
            (block_ids[0], Offset { dx, dy: 0, dz }),
            (
                block_ids[1],
                Offset {
                    dx: 0,
                    dy: 0,
                    dz: 0,
                },
            ),
        ] {
            let baseline_offset = baseline.block_offsets[&block];
            let expected_offset = Offset {
                dx: baseline_offset.dx + offset.dx,
                dy: baseline_offset.dy,
                dz: baseline_offset.dz + offset.dz,
            };
            assert_eq!(overridden.block_offsets[&block], expected_offset);
            for port in compiled.inputs.values() {
                let terminal = shift(port.cell, expected_offset);
                assert!(
                    overridden
                        .candidate
                        .routes
                        .values()
                        .flat_map(|route| &route.branches)
                        .any(|branch| branch.terminal.at == terminal),
                    "no parent route reaches input port at {terminal:?}",
                );
            }
            for port in compiled.outputs.values() {
                let root = shift(port.cell, expected_offset);
                assert!(
                    overridden
                        .candidate
                        .routes
                        .values()
                        .flat_map(|route| &route.branches)
                        .any(|branch| branch.root == root),
                    "no parent route leaves output port at {root:?}",
                );
            }
        }
    }

    /// The no-blocks invariant.  Both literals were read off the seed as it
    /// stood immediately before Task 10 touched it (a throwaway test that
    /// panicked with the certified metrics of these two circuits), so this
    /// asserts the block-aware pipeline leaves a flat design bit-for-bit
    /// where it was, not merely self-consistent.
    #[test]
    fn a_flat_design_certifies_to_the_pre_block_fingerprints() {
        let (and4, _) = build_and4_netlist();
        let and4 = build(&and4).unwrap();
        assert_eq!(
            and4.metrics().candidate_fingerprint.as_str(),
            "318f8c04304e88773f0cf809e498305828afd308fe4b3b003fd3a25fe519284c"
        );
        assert_eq!(
            and4.metrics().emitted_world_fingerprint.as_str(),
            "97fcaad9a1b4f456718a4073ec22e0f67aae322de0e4d07e62255d65e963d30d"
        );
        let full_adder = crate::compile::lowering::lower_optimised(
            &crate::circuits::full_adder::build_full_adder_netlist().0,
        )
        .unwrap();
        let full_adder = build(&full_adder).unwrap();
        assert_eq!(
            full_adder.metrics().candidate_fingerprint.as_str(),
            "02c3401071d4f93964147d52561fc7e1b38f1be65765232d8f280df71a5912c5"
        );
        assert_eq!(
            full_adder.metrics().emitted_world_fingerprint.as_str(),
            "5c5ffd5e96504dd559d499cb96c4eda692c59bf9f724136d5bdd6fe8a6df9173"
        );
    }
}
