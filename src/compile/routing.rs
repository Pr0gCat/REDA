//! Durable typed physical routing authority shared by legacy and fragment paths.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::compile::fragment_synth::identity::{
    ConnectionId, PhysicalEndpointId, PortId, PrimitiveId, RouteId, RoutedSinkId,
};
use crate::compile::geometry::Anchor;
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum DelayedOwner {
    Primitive(PrimitiveId),
    Route(RouteId),
    InputBinding(PortId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DelayedComponent {
    pub at: Anchor,
    pub owner: DelayedOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlacedBlock {
    pub at: Anchor,
    pub state: BlockState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouteTerminalKind {
    RepeaterIntoSupport,
    DirectedDustIntoSupport,
    BareMergeDust,
    BareMergeRepeater,
    OutputTerminalRepeater,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalStyle {
    DirectedDustIntoSupport,
    RepeaterIntoSupport,
}

impl From<TerminalStyle> for RouteTerminalKind {
    fn from(style: TerminalStyle) -> Self {
        match style {
            TerminalStyle::DirectedDustIntoSupport => Self::DirectedDustIntoSupport,
            TerminalStyle::RepeaterIntoSupport => Self::RepeaterIntoSupport,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalApproach {
    pub predecessor: Anchor,
    pub terminal: Anchor,
    pub support: Anchor,
    pub predecessor_strength: u8,
    pub isolation_proven: bool,
}

impl TerminalApproach {
    pub fn new(
        predecessor: Anchor,
        terminal: Anchor,
        support: Anchor,
        predecessor_strength: u8,
        isolation_proven: bool,
    ) -> Self {
        Self {
            predecessor,
            terminal,
            support,
            predecessor_strength,
            isolation_proven,
        }
    }
}

pub fn terminal_style(approach: &TerminalApproach) -> TerminalStyle {
    let incoming = horizontal_direction(approach.predecessor, approach.terminal);
    let outgoing = horizontal_direction(approach.terminal, approach.support);
    if approach.predecessor_strength > 1
        && approach.isolation_proven
        && incoming.is_some()
        && incoming == outgoing
    {
        TerminalStyle::DirectedDustIntoSupport
    } else {
        TerminalStyle::RepeaterIntoSupport
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalRecord {
    pub sink: RoutedSinkId,
    pub at: Anchor,
    pub state: BlockState,
    pub kind: RouteTerminalKind,
    pub repeaters: u64,
    pub delayed_owner: Option<DelayedOwner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RealisedRouteBranch {
    pub sink: RoutedSinkId,
    pub target: RouteTarget,
    pub root: Anchor,
    pub path: Vec<Anchor>,
    pub terminal: TerminalRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum RouteTarget {
    Connection(ConnectionId),
    DeclaredOutput(PortId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RealisedRouteTree {
    pub id: RouteId,
    pub source: PhysicalEndpointId,
    pub cells: Vec<PlacedBlock>,
    pub floors: Vec<PlacedBlock>,
    pub branches: Vec<RealisedRouteBranch>,
}

impl RealisedRouteTree {
    pub fn owned_blocks(&self) -> impl Iterator<Item = PlacedBlock> + '_ {
        self.cells.iter().chain(self.floors.iter()).cloned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RouterLimits {
    pub max_node_expansions: u64,
    pub max_queue_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TerminalContract {
    Source {
        signal_strength: u8,
    },
    Sink {
        target: RouteTarget,
        support: Anchor,
        requirement: TerminalRequirement,
    },
}

impl TerminalContract {
    pub fn target(&self) -> Option<RouteTarget> {
        match self {
            Self::Source { .. } => None,
            Self::Sink { target, .. } => Some(*target),
        }
    }

    fn source_strength(&self) -> Option<u8> {
        match self {
            Self::Source { signal_strength } => Some(*signal_strength),
            Self::Sink { .. } => None,
        }
    }

    fn sink_parts(&self) -> Option<(RouteTarget, Anchor, TerminalRequirement)> {
        match self {
            Self::Source { .. } => None,
            Self::Sink {
                target,
                support,
                requirement,
            } => Some((*target, *support, *requirement)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TerminalRequirement {
    Automatic,
    Repeater,
    DirectedDust,
    Exact(RouteTerminalKind),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteEndpoint {
    pub id: PhysicalEndpointId,
    pub anchor: Anchor,
    /// Direction in which the route leaves the source endpoint.
    pub allowed_exit: Facing,
    pub terminal: TerminalContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteSink {
    pub id: RoutedSinkId,
    pub endpoint: PhysicalEndpointId,
    pub anchor: Anchor,
    /// Direction from this terminal cell toward the only allowed predecessor.
    pub allowed_entry: Facing,
    pub terminal: TerminalContract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("a physical route request must contain at least one sink")]
pub struct EmptyRouteSinks;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NonEmptyRouteSinks(Vec<RouteSink>);

impl NonEmptyRouteSinks {
    pub fn new(sinks: Vec<RouteSink>) -> Result<Self, EmptyRouteSinks> {
        if sinks.is_empty() {
            Err(EmptyRouteSinks)
        } else {
            Ok(Self(sinks))
        }
    }

    pub fn as_slice(&self) -> &[RouteSink] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum PhysicalReservationOwner {
    Route(RouteId),
    Endpoint(PhysicalEndpointId),
    KeepOut(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PhysicalReservationKind {
    Conductor(BlockState),
    Floor(BlockState),
    MandatoryAir,
    KeepOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhysicalReservation {
    pub owner: PhysicalReservationOwner,
    pub kind: PhysicalReservationKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PhysicalReservations {
    cells: BTreeMap<Anchor, PhysicalReservation>,
}

impl PhysicalReservations {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reserve(
        &mut self,
        at: Anchor,
        owner: PhysicalReservationOwner,
        kind: PhysicalReservationKind,
    ) -> Option<PhysicalReservation> {
        self.cells.insert(at, PhysicalReservation { owner, kind })
    }

    pub fn reserve_conductor(&mut self, at: Anchor, owner: RouteId, state: BlockState) {
        self.reserve(
            at,
            PhysicalReservationOwner::Route(owner),
            PhysicalReservationKind::Conductor(state),
        );
    }

    pub fn get(&self, at: &Anchor) -> Option<&PhysicalReservation> {
        self.cells.get(at)
    }
}

pub struct RouteRequest<'a> {
    pub id: RouteId,
    pub source: RouteEndpoint,
    pub sinks: &'a NonEmptyRouteSinks,
    pub reservations: &'a PhysicalReservations,
    pub limits: RouterLimits,
}

pub trait PhysicalRouter {
    fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure>;
}

/// Name-free fragment-side adapter.  It deliberately performs no coordinate
/// or string flattening: the caller's typed sink order reaches the authority
/// unchanged.
#[derive(Debug, Clone)]
pub struct FragmentRouterAdapter<R> {
    router: R,
}

impl<R> FragmentRouterAdapter<R> {
    pub fn new(router: R) -> Self {
        Self { router }
    }
}

impl<R: PhysicalRouter> PhysicalRouter for FragmentRouterAdapter<R> {
    fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
        self.router.route(request)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouterLimitKind {
    NodeExpansions,
    QueueEntries,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouterRefusalCategory {
    InvalidRequest,
    NoLocalRoute,
    WrongRepeaterAxis,
    PhysicalInvariant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RouterFailure {
    RouterLimitExceeded {
        route: RouteId,
        source: PhysicalEndpointId,
        sink: RoutedSinkId,
        kind: RouterLimitKind,
        limit: u64,
        work_used: u64,
    },
    NoLocalRoute {
        route: RouteId,
        source: PhysicalEndpointId,
        sink: RoutedSinkId,
    },
    WrongRepeaterAxis {
        connection: ConnectionId,
        at: Anchor,
    },
    InvalidRequest {
        route: RouteId,
        source: PhysicalEndpointId,
        sink: Option<RoutedSinkId>,
    },
    Refused {
        route: RouteId,
        source: PhysicalEndpointId,
        sink: Option<RoutedSinkId>,
        category: RouterRefusalCategory,
    },
}

impl std::fmt::Display for RouterFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RouterLimitExceeded {
                route, sink, kind, ..
            } => write!(
                formatter,
                "route {route:?} exceeded {kind:?} while routing sink {sink:?}"
            ),
            Self::NoLocalRoute { route, sink, .. } => {
                write!(
                    formatter,
                    "route {route:?} sink {sink:?} has no local physical route"
                )
            }
            Self::WrongRepeaterAxis { connection, at } => write!(
                formatter,
                "connection {connection:?} has a repeater on the wrong axis at {at:?}"
            ),
            Self::InvalidRequest { route, .. } => {
                write!(
                    formatter,
                    "route {route:?} is not a well-typed physical request"
                )
            }
            Self::Refused {
                route, category, ..
            } => write!(
                formatter,
                "legacy physical router refused route {route:?} as {category:?}"
            ),
        }
    }
}

impl std::error::Error for RouterFailure {}

impl RouterFailure {
    pub fn category(&self) -> RouterRefusalCategory {
        match self {
            Self::RouterLimitExceeded { .. } | Self::InvalidRequest { .. } => {
                RouterRefusalCategory::InvalidRequest
            }
            Self::NoLocalRoute { .. } => RouterRefusalCategory::NoLocalRoute,
            Self::WrongRepeaterAxis { .. } => RouterRefusalCategory::WrongRepeaterAxis,
            Self::Refused { category, .. } => *category,
        }
    }
}

/// The production fragment router.  Its counters are owned by one `route`
/// invocation, so work spent on earlier ordered sinks is never reset.
#[derive(Debug, Clone, Copy, Default)]
pub struct DurablePhysicalRouter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SearchState {
    estimate: u64,
    travelled: u64,
    at: Anchor,
}

#[derive(Debug, Default)]
struct RouterWork {
    node_expansions: u64,
    queue_entries: u64,
}

impl RouterWork {
    fn queue(
        &mut self,
        request: &RouteRequest<'_>,
        sink: RoutedSinkId,
    ) -> Result<(), RouterFailure> {
        self.queue_entries = self.queue_entries.saturating_add(1);
        if self.queue_entries > request.limits.max_queue_entries {
            return Err(RouterFailure::RouterLimitExceeded {
                route: request.id,
                source: request.source.id,
                sink,
                kind: RouterLimitKind::QueueEntries,
                limit: request.limits.max_queue_entries,
                work_used: self.queue_entries,
            });
        }
        Ok(())
    }

    fn expand(
        &mut self,
        request: &RouteRequest<'_>,
        sink: RoutedSinkId,
    ) -> Result<(), RouterFailure> {
        self.node_expansions = self.node_expansions.saturating_add(1);
        if self.node_expansions > request.limits.max_node_expansions {
            return Err(RouterFailure::RouterLimitExceeded {
                route: request.id,
                source: request.source.id,
                sink,
                kind: RouterLimitKind::NodeExpansions,
                limit: request.limits.max_node_expansions,
                work_used: self.node_expansions,
            });
        }
        Ok(())
    }
}

impl PhysicalRouter for DurablePhysicalRouter {
    fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure> {
        validate_request(&request)?;
        let source_strength =
            request
                .source
                .terminal
                .source_strength()
                .ok_or(RouterFailure::InvalidRequest {
                    route: request.id,
                    source: request.source.id,
                    sink: None,
                })?;
        let start = step(request.source.anchor, request.source.allowed_exit);
        let mut work = RouterWork::default();
        let mut cell_states = BTreeMap::<Anchor, BlockState>::new();
        let mut cell_order = Vec::<Anchor>::new();
        let mut floor_states = BTreeMap::<Anchor, BlockState>::new();
        let mut floor_order = Vec::<Anchor>::new();
        let mut branches = Vec::with_capacity(request.sinks.as_slice().len());

        for sink in request.sinks.as_slice() {
            let approach = step(sink.anchor, sink.allowed_entry);
            let mut path = search_path(&request, sink, start, approach, &cell_states, &mut work)?
                .ok_or(RouterFailure::NoLocalRoute {
                route: request.id,
                source: request.source.id,
                sink: sink.id,
            })?;
            if path.last() != Some(&sink.anchor) {
                path.push(sink.anchor);
            }

            realise_path(
                &request,
                sink,
                &path,
                source_strength,
                &mut cell_states,
                &mut cell_order,
                &mut floor_states,
                &mut floor_order,
            )?;
            certify_path(&request, sink, &path, &cell_states)?;

            let (target, _, requirement) =
                sink.terminal
                    .sink_parts()
                    .ok_or(RouterFailure::InvalidRequest {
                        route: request.id,
                        source: request.source.id,
                        sink: Some(sink.id),
                    })?;
            let state =
                cell_states
                    .get(&sink.anchor)
                    .cloned()
                    .ok_or(RouterFailure::InvalidRequest {
                        route: request.id,
                        source: request.source.id,
                        sink: Some(sink.id),
                    })?;
            let kind = terminal_kind(requirement, &state);
            let repeaters = path
                .iter()
                .filter(|at| {
                    cell_states
                        .get(at)
                        .is_some_and(|state| state.kind == BlockKind::Repeater)
                })
                .count() as u64;
            branches.push(RealisedRouteBranch {
                sink: sink.id,
                target,
                root: *path.first().expect("a routed path contains its start"),
                path,
                terminal: TerminalRecord {
                    sink: sink.id,
                    at: sink.anchor,
                    delayed_owner: (state.kind == BlockKind::Repeater)
                        .then_some(DelayedOwner::Route(request.id)),
                    state,
                    kind,
                    repeaters,
                },
            });
        }

        Ok(RealisedRouteTree {
            id: request.id,
            source: request.source.id,
            cells: cell_order
                .into_iter()
                .map(|at| PlacedBlock {
                    state: cell_states
                        .remove(&at)
                        .expect("cell order and state map are updated together"),
                    at,
                })
                .collect(),
            floors: floor_order
                .into_iter()
                .map(|at| PlacedBlock {
                    state: floor_states
                        .remove(&at)
                        .expect("floor order and state map are updated together"),
                    at,
                })
                .collect(),
            branches,
        })
    }
}

fn validate_request(request: &RouteRequest<'_>) -> Result<(), RouterFailure> {
    for sink in request.sinks.as_slice() {
        if sink.id.route != request.id || !matches!(sink.terminal, TerminalContract::Sink { .. }) {
            return Err(RouterFailure::InvalidRequest {
                route: request.id,
                source: request.source.id,
                sink: Some(sink.id),
            });
        }
    }
    if !matches!(request.source.terminal, TerminalContract::Source { .. }) {
        return Err(RouterFailure::InvalidRequest {
            route: request.id,
            source: request.source.id,
            sink: None,
        });
    }
    Ok(())
}

fn search_path(
    request: &RouteRequest<'_>,
    sink: &RouteSink,
    start: Anchor,
    goal: Anchor,
    laid: &BTreeMap<Anchor, BlockState>,
    work: &mut RouterWork,
) -> Result<Option<Vec<Anchor>>, RouterFailure> {
    work.queue(request, sink.id)?;
    let margin = manhattan(start, goal).saturating_add(2) as i32;
    let min = Anchor {
        x: start.x.min(goal.x).saturating_sub(margin),
        y: start.y.min(goal.y),
        z: start.z.min(goal.z).saturating_sub(margin),
    };
    let max = Anchor {
        x: start.x.max(goal.x).saturating_add(margin),
        y: start.y.max(goal.y).saturating_add(3),
        z: start.z.max(goal.z).saturating_add(margin),
    };
    let mut frontier = BTreeSet::from([SearchState {
        estimate: manhattan(start, goal),
        travelled: 0,
        at: start,
    }]);
    let mut travelled = BTreeMap::from([(start, 0u64)]);
    let mut previous = BTreeMap::<Anchor, Anchor>::new();

    while let Some(state) = frontier.iter().next().copied() {
        frontier.remove(&state);
        if state.at == goal {
            return Ok(Some(reconstruct_path(previous, goal)));
        }
        if travelled.get(&state.at) != Some(&state.travelled) {
            continue;
        }
        work.expand(request, sink.id)?;
        for next in neighbours(state.at) {
            if next == start || previous.contains_key(&next) {
                continue;
            }
            if next.x < min.x
                || next.x > max.x
                || next.y < min.y
                || next.y > max.y
                || next.z < min.z
                || next.z > max.z
            {
                continue;
            }
            if !cell_available(request.id, next, goal, request.reservations, laid) {
                continue;
            }
            if let Some(&before) = previous.get(&state.at) {
                let state_at = exact_or_dust(request.id, state.at, request.reservations, laid);
                if !route_step_is_legal(before, state.at, next, &state_at) {
                    continue;
                }
            }
            let next_travelled =
                state
                    .travelled
                    .saturating_add(if next.y == state.at.y { 1 } else { 3 });
            if travelled
                .get(&next)
                .is_some_and(|known| *known <= next_travelled)
            {
                continue;
            }
            work.queue(request, sink.id)?;
            travelled.insert(next, next_travelled);
            previous.insert(next, state.at);
            frontier.insert(SearchState {
                estimate: next_travelled.saturating_add(manhattan(next, goal)),
                travelled: next_travelled,
                at: next,
            });
        }
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn realise_path(
    request: &RouteRequest<'_>,
    sink: &RouteSink,
    path: &[Anchor],
    incoming: u8,
    cells: &mut BTreeMap<Anchor, BlockState>,
    cell_order: &mut Vec<Anchor>,
    floors: &mut BTreeMap<Anchor, BlockState>,
    floor_order: &mut Vec<Anchor>,
) -> Result<(), RouterFailure> {
    let (_, _, requirement) = sink
        .terminal
        .sink_parts()
        .ok_or(RouterFailure::InvalidRequest {
            route: request.id,
            source: request.source.id,
            sink: Some(sink.id),
        })?;
    let mut strength = incoming.min(MAX_SIGNAL_STRENGTH);
    for (index, &at) in path.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .map(|before| path[before])
            .unwrap_or(request.source.anchor);
        let next = path
            .get(index + 1)
            .copied()
            .or_else(|| sink.terminal.sink_parts().map(|(_, support, _)| support))
            .unwrap_or(sink.anchor);
        let exact = cells.get(&at).cloned().or_else(|| {
            request
                .reservations
                .get(&at)
                .filter(|claim| claim.owner == PhysicalReservationOwner::Route(request.id))
                .and_then(|claim| match &claim.kind {
                    PhysicalReservationKind::Conductor(state) => Some(state.clone()),
                    _ => None,
                })
        });
        let mut state = exact.unwrap_or_else(|| {
            let straight = horizontal_direction(previous, at)
                .zip(horizontal_direction(at, next))
                .is_some_and(|(entered, leaves)| entered == leaves);
            if strength <= 2 && straight {
                repeater_toward(horizontal_direction(previous, at).expect("straight is horizontal"))
            } else {
                dust()
            }
        });
        if at == sink.anchor && !cells.contains_key(&at) {
            state = match requirement {
                TerminalRequirement::Repeater
                | TerminalRequirement::Exact(
                    RouteTerminalKind::RepeaterIntoSupport
                    | RouteTerminalKind::BareMergeRepeater
                    | RouteTerminalKind::OutputTerminalRepeater,
                ) => repeater_toward(horizontal_direction(previous, at).ok_or(
                    RouterFailure::WrongRepeaterAxis {
                        connection: sink_connection(sink)?,
                        at,
                    },
                )?),
                TerminalRequirement::DirectedDust
                | TerminalRequirement::Exact(
                    RouteTerminalKind::DirectedDustIntoSupport | RouteTerminalKind::BareMergeDust,
                ) => dust(),
                TerminalRequirement::Automatic => state,
            };
        }
        if state.kind == BlockKind::Repeater {
            strength = MAX_SIGNAL_STRENGTH;
        } else {
            strength = strength.saturating_sub(1);
        }
        if cells.insert(at, state).is_none() {
            cell_order.push(at);
        }
        let floor = Anchor { y: at.y - 1, ..at };
        if floors.insert(floor, stone()).is_none() {
            floor_order.push(floor);
        }
    }
    Ok(())
}

fn certify_path(
    request: &RouteRequest<'_>,
    sink: &RouteSink,
    path: &[Anchor],
    states: &BTreeMap<Anchor, BlockState>,
) -> Result<(), RouterFailure> {
    let support = sink
        .terminal
        .sink_parts()
        .map(|(_, support, _)| support)
        .ok_or(RouterFailure::InvalidRequest {
            route: request.id,
            source: request.source.id,
            sink: Some(sink.id),
        })?;
    for (index, &at) in path.iter().enumerate() {
        let previous = index
            .checked_sub(1)
            .map(|before| path[before])
            .unwrap_or(request.source.anchor);
        let next = path.get(index + 1).copied().unwrap_or(support);
        let state = states.get(&at).expect("realisation covers every path cell");
        if !route_step_is_legal(previous, at, next, state) {
            if state.kind == BlockKind::Repeater {
                return Err(RouterFailure::WrongRepeaterAxis {
                    connection: sink_connection(sink)?,
                    at,
                });
            }
            return Err(RouterFailure::NoLocalRoute {
                route: request.id,
                source: request.source.id,
                sink: sink.id,
            });
        }
    }
    Ok(())
}

fn sink_connection(sink: &RouteSink) -> Result<ConnectionId, RouterFailure> {
    match sink.endpoint {
        PhysicalEndpointId::Landing(connection) => Ok(connection),
        _ => Err(RouterFailure::InvalidRequest {
            route: sink.id.route,
            source: sink.endpoint,
            sink: Some(sink.id),
        }),
    }
}

fn terminal_kind(requirement: TerminalRequirement, state: &BlockState) -> RouteTerminalKind {
    match requirement {
        TerminalRequirement::Exact(kind) => kind,
        TerminalRequirement::DirectedDust => RouteTerminalKind::DirectedDustIntoSupport,
        TerminalRequirement::Repeater => RouteTerminalKind::RepeaterIntoSupport,
        TerminalRequirement::Automatic if state.kind == BlockKind::Repeater => {
            RouteTerminalKind::RepeaterIntoSupport
        }
        TerminalRequirement::Automatic => RouteTerminalKind::DirectedDustIntoSupport,
    }
}

fn exact_or_dust(
    route: RouteId,
    at: Anchor,
    reservations: &PhysicalReservations,
    laid: &BTreeMap<Anchor, BlockState>,
) -> BlockState {
    laid.get(&at)
        .cloned()
        .or_else(|| {
            reservations.get(&at).and_then(|claim| {
                (claim.owner == PhysicalReservationOwner::Route(route))
                    .then_some(&claim.kind)
                    .and_then(|kind| match kind {
                        PhysicalReservationKind::Conductor(state) => Some(state.clone()),
                        _ => None,
                    })
            })
        })
        .unwrap_or_else(dust)
}

fn cell_available(
    route: RouteId,
    at: Anchor,
    goal: Anchor,
    reservations: &PhysicalReservations,
    laid: &BTreeMap<Anchor, BlockState>,
) -> bool {
    if at == goal || laid.contains_key(&at) {
        return true;
    }
    reservations.get(&at).is_none_or(|claim| {
        claim.owner == PhysicalReservationOwner::Route(route)
            && matches!(claim.kind, PhysicalReservationKind::Conductor(_))
    })
}

fn reconstruct_path(previous: BTreeMap<Anchor, Anchor>, goal: Anchor) -> Vec<Anchor> {
    let mut path = vec![goal];
    while let Some(parent) = previous.get(path.last().expect("path is non-empty")) {
        path.push(*parent);
    }
    path.reverse();
    path
}

fn neighbours(anchor: Anchor) -> Vec<Anchor> {
    let mut out = Vec::with_capacity(12);
    for horizontal in [Facing::West, Facing::East, Facing::North, Facing::South] {
        let sideways = step(anchor, horizontal);
        out.push(sideways);
        out.push(Anchor {
            y: sideways.y + 1,
            ..sideways
        });
        out.push(Anchor {
            y: sideways.y - 1,
            ..sideways
        });
    }
    out
}

fn manhattan(from: Anchor, to: Anchor) -> u64 {
    u64::from(from.x.abs_diff(to.x))
        + u64::from(from.y.abs_diff(to.y))
        + u64::from(from.z.abs_diff(to.z))
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

fn horizontal_direction(from: Anchor, to: Anchor) -> Option<Facing> {
    if from.y != to.y {
        return None;
    }
    match (to.x - from.x, to.z - from.z) {
        (1, 0) => Some(Facing::East),
        (-1, 0) => Some(Facing::West),
        (0, 1) => Some(Facing::South),
        (0, -1) => Some(Facing::North),
        _ => None,
    }
}

fn dust_step_is_locally_possible(from: Anchor, to: Anchor) -> bool {
    let horizontal = from.x.abs_diff(to.x) + from.z.abs_diff(to.z);
    horizontal == 1 && from.y.abs_diff(to.y) <= 1
}

/// Shared local cell-direction authority.
///
/// Dust stair support and lid state are deliberately outside this signature;
/// callers must additionally prove those world-dependent conditions.
pub fn route_step_is_legal(previous: Anchor, at: Anchor, next: Anchor, state: &BlockState) -> bool {
    match state.kind {
        BlockKind::RedstoneWire => {
            dust_step_is_locally_possible(previous, at) && dust_step_is_locally_possible(at, next)
        }
        BlockKind::Repeater => {
            let Some(facing) = state.facing else {
                return false;
            };
            horizontal_direction(at, previous) == Some(facing)
                && horizontal_direction(at, next) == Some(facing.opposite())
        }
        _ => false,
    }
}

fn dust() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::RedstoneWire;
    state.name = "minecraft:redstone_wire".to_string();
    state
}

fn stone() -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Solid;
    state.name = "minecraft:stone".to_string();
    state
}

fn repeater_toward(direction: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::Repeater;
    state.name = "minecraft:repeater".to_string();
    state.facing = Some(direction.opposite());
    state.delay = 1;
    state.lit = true;
    state
}

/// Exact full-state realisation of one newly laid branch tail.
pub(crate) struct LaidBranch {
    pub(crate) blocks: Vec<BlockState>,
    pub(crate) floors: Vec<BlockState>,
    pub(crate) strength_before_terminal: u8,
    pub(crate) repeaters: u64,
    pub(crate) carries: bool,
}

/// Continue a branch from an already-carried strength without reconstructing
/// any repeater state later.  Facing and delay are fixed here and copied all
/// the way into route candidates and emission.
pub(crate) fn realise_branch_from(
    previous_cell: Anchor,
    incoming: u8,
    cells: &[Anchor],
) -> LaidBranch {
    let source = previous_cell;
    let mut bends: BTreeSet<usize> = cells
        .windows(3)
        .enumerate()
        .filter(|(_, window)| {
            path_direction(window[0], window[1]) != path_direction(window[1], window[2])
        })
        .map(|(index, _)| index + 1)
        .collect();
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        if cell.y != previous.y {
            bends.insert(index);
        }
        previous = *cell;
    }
    let stairs = bends
        .iter()
        .filter(|&&index| {
            let before = if index == 0 { source } else { cells[index - 1] };
            cells[index].y != before.y
        })
        .count();
    let reserve = (stairs as i32).min(crate::compile::MAX_DUST_RUN - 2);
    let (is_repeater, _) = crate::compile::plan_bent_path(cells.len(), &bends, incoming, reserve);
    let mut is_repeater = is_repeater;
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        if cell.y != previous.y && index > 0 {
            let before = index - 1;
            if !bends.contains(&before) {
                is_repeater[before] = true;
            }
        }
        previous = *cell;
    }

    let mut blocks = Vec::with_capacity(cells.len());
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        let horizontal = horizontal_direction(previous, *cell);
        let block = match (is_repeater[index], horizontal) {
            (true, Some(direction)) => crate::compile::repeater(direction),
            _ => crate::compile::dust(),
        };
        blocks.push(block);
        previous = *cell;
    }
    let strength_before_terminal = match cells.len().checked_sub(2) {
        None => incoming,
        Some(index) => {
            let last_refresh = (0..=index).rev().find(|&i| is_repeater[i]);
            match last_refresh {
                Some(refresh) => MAX_SIGNAL_STRENGTH.saturating_sub((index - refresh) as u8),
                None => incoming.saturating_sub((index + 1) as u8),
            }
        }
    };
    let mut carried = incoming;
    let mut carries = true;
    for block in &blocks {
        if block.kind == BlockKind::Repeater {
            carried = MAX_SIGNAL_STRENGTH;
        } else {
            carried = carried.saturating_sub(1);
            if carried == 0 {
                carries = false;
                break;
            }
        }
    }
    LaidBranch {
        floors: vec![crate::compile::stone(); blocks.len()],
        repeaters: blocks
            .iter()
            .filter(|block| block.kind == BlockKind::Repeater)
            .count() as u64,
        blocks,
        strength_before_terminal,
        carries,
    }
}

fn path_direction(from: Anchor, to: Anchor) -> (i32, i32, i32) {
    (
        (to.x - from.x).signum(),
        (to.y - from.y).signum(),
        (to.z - from.z).signum(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StrengthSearchState {
    estimate: u64,
    travelled: u64,
    anchor: Anchor,
    entered: u8,
    carried: u8,
}

fn entered_code(from: Anchor, to: Anchor) -> u8 {
    match horizontal_direction(from, to) {
        Some(Facing::East) => 0,
        Some(Facing::West) => 1,
        Some(Facing::South) => 2,
        Some(Facing::North) => 3,
        _ => 4,
    }
}

/// Shared strength-aware expansion kernel.  Reservation and pricing policy
/// stay in adapters; state dominance, exact trunk state and reconstruction
/// live here once.
pub(crate) fn strength_aware_astar<Allowed, Price>(
    start: Anchor,
    goal: Anchor,
    trunk: &BTreeMap<Anchor, BlockState>,
    source_strength: u8,
    mut allowed: Allowed,
    mut price: Price,
) -> Option<Vec<Anchor>>
where
    Allowed: FnMut(&BTreeMap<Anchor, Anchor>, Anchor, Anchor) -> bool,
    Price: FnMut(&Anchor) -> u64,
{
    let start_state = StrengthSearchState {
        estimate: manhattan(start, goal),
        travelled: 0,
        anchor: start,
        entered: 4,
        carried: source_strength,
    };
    let mut frontier = BTreeSet::from([start_state]);
    let mut visited: BTreeMap<(Anchor, u8), Vec<(u64, u8)>> = BTreeMap::new();
    visited.insert((start, 4), vec![(0, start_state.carried)]);
    type StateKey = (Anchor, u8, u8, u64);
    let mut parent: BTreeMap<StateKey, StateKey> = BTreeMap::new();

    while let Some(state) = frontier.iter().next().copied() {
        frontier.remove(&state);
        if state.anchor == goal {
            let mut path = vec![state.anchor];
            let mut walk = (state.anchor, state.entered, state.carried, state.travelled);
            while let Some(&up) = parent.get(&walk) {
                path.push(up.0);
                walk = up;
            }
            path.reverse();
            return Some(path);
        }
        let chain: BTreeMap<Anchor, Anchor> = {
            let mut chain = BTreeMap::new();
            let mut walk = (state.anchor, state.entered, state.carried, state.travelled);
            while let Some(&up) = parent.get(&walk) {
                chain.insert(walk.0, up.0);
                walk = up;
            }
            chain
        };
        for next in neighbours(state.anchor) {
            if next == start || chain.contains_key(&next) || !allowed(&chain, state.anchor, next) {
                continue;
            }
            if let (Some(previous), Some(state_at)) =
                (chain.get(&state.anchor), trunk.get(&state.anchor))
            {
                if state_at.kind == BlockKind::Repeater
                    && !route_step_is_legal(*previous, state.anchor, next, state_at)
                {
                    continue;
                }
            }
            let carried = match trunk.get(&next) {
                Some(state_at) if state_at.kind == BlockKind::Repeater => {
                    if state_at.facing != horizontal_direction(next, state.anchor) {
                        continue;
                    }
                    MAX_SIGNAL_STRENGTH
                }
                Some(_) => match state.carried.checked_sub(1) {
                    None | Some(0) => continue,
                    Some(left) => left,
                },
                None => {
                    let step = entered_code(state.anchor, next);
                    let straight_through = state.entered == step
                        && step != 4
                        && !trunk.contains_key(&state.anchor)
                        && state.anchor != start;
                    let leaving = if straight_through {
                        MAX_SIGNAL_STRENGTH
                    } else {
                        state.carried
                    };
                    match leaving.checked_sub(1) {
                        None | Some(0) => continue,
                        Some(left) => left,
                    }
                }
            };
            const CLIMB_COST: u64 = 3;
            let closer_in_y = (next.y - goal.y).abs() < (state.anchor.y - goal.y).abs();
            let step_cost = if next.y == state.anchor.y || closer_in_y {
                1
            } else {
                CLIMB_COST
            };
            let next_travelled = state
                .travelled
                .saturating_add(step_cost)
                .saturating_add(price(&next));
            let entered = entered_code(state.anchor, next);
            let seen = visited.entry((next, entered)).or_default();
            if seen
                .iter()
                .any(|&(travelled, strength)| travelled <= next_travelled && strength >= carried)
            {
                continue;
            }
            seen.retain(|&(travelled, strength)| {
                !(next_travelled <= travelled && carried >= strength)
            });
            seen.push((next_travelled, carried));
            parent.insert(
                (next, entered, carried, next_travelled),
                (state.anchor, state.entered, state.carried, state.travelled),
            );
            frontier.insert(StrengthSearchState {
                estimate: next_travelled.saturating_add(manhattan(next, goal)),
                travelled: next_travelled,
                anchor: next,
                entered,
                carried,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::identity::{
        ConnectionId, InstanceId, PhysicalEndpointId, PortId, RouteId, RoutedSinkId,
    };
    use crate::compile::geometry::Anchor;
    use crate::redstone::world::block::{BlockKind, BlockState, Facing};

    fn at(x: i32, y: i32, z: i32) -> Anchor {
        Anchor { x, y, z }
    }

    fn connection(instance: u32, input_index: u16) -> ConnectionId {
        ConnectionId::External {
            instance: InstanceId(instance),
            input_index,
        }
    }

    fn endpoint(route: RouteId) -> RouteEndpoint {
        RouteEndpoint {
            id: PhysicalEndpointId::PrimaryInput(PortId(route.0)),
            anchor: at(0, 1, 0),
            allowed_exit: Facing::East,
            terminal: TerminalContract::Source {
                signal_strength: 15,
            },
        }
    }

    fn sink(route: RouteId, ordinal: u16, terminal: Anchor) -> RouteSink {
        let connection = connection(ordinal as u32 + 10, ordinal);
        RouteSink {
            id: RoutedSinkId { route, ordinal },
            endpoint: PhysicalEndpointId::Landing(connection),
            anchor: terminal,
            allowed_entry: Facing::West,
            terminal: TerminalContract::Sink {
                target: RouteTarget::Connection(connection),
                support: at(terminal.x + 1, terminal.y, terminal.z),
                requirement: TerminalRequirement::Repeater,
            },
        }
    }

    #[test]
    fn non_empty_route_sinks_reject_empty_and_preserve_caller_order() {
        assert_eq!(NonEmptyRouteSinks::new(Vec::new()), Err(EmptyRouteSinks));

        let route = RouteId(7);
        let second = sink(route, 1, at(4, 1, 2));
        let first = sink(route, 0, at(4, 1, 0));
        let ordered = NonEmptyRouteSinks::new(vec![second.clone(), first.clone()]).unwrap();

        assert_eq!(ordered.as_slice(), &[second, first]);
    }

    #[test]
    fn zero_limits_fail_deterministically_before_any_unbounded_fallback() {
        let route = RouteId(8);
        let source = endpoint(route);
        let sinks = NonEmptyRouteSinks::new(vec![sink(route, 0, at(4, 1, 0))]).unwrap();
        let reservations = PhysicalReservations::new();
        let router = DurablePhysicalRouter;

        let error = router
            .route(RouteRequest {
                id: route,
                source: source.clone(),
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 0,
                    max_queue_entries: 0,
                },
            })
            .unwrap_err();

        assert_eq!(
            error,
            RouterFailure::RouterLimitExceeded {
                route,
                source: source.id,
                sink: sinks.as_slice()[0].id,
                kind: RouterLimitKind::QueueEntries,
                limit: 0,
                work_used: 1,
            }
        );
    }

    #[test]
    fn repeater_legality_reads_the_exact_rear_and_front_axis() {
        let previous = at(1, 1, 0);
        let repeater_at = at(2, 1, 0);
        let next = at(3, 1, 0);
        let mut aligned = BlockState::air();
        aligned.kind = BlockKind::Repeater;
        aligned.name = "minecraft:repeater".to_string();
        aligned.facing = Some(Facing::West);
        aligned.delay = 4;
        let mut rotated = aligned.clone();
        rotated.facing = Some(Facing::North);

        assert!(route_step_is_legal(previous, repeater_at, next, &aligned));
        assert!(!route_step_is_legal(previous, repeater_at, next, &rotated));
        assert_eq!(
            aligned.delay, 4,
            "axis certification must not normalise delay"
        );
    }

    #[test]
    fn typed_fanout_keeps_shared_cells_branch_order_and_full_terminal_records() {
        let route = RouteId(9);
        let source = endpoint(route);
        let expected_sinks = vec![sink(route, 0, at(5, 1, 0)), sink(route, 1, at(5, 1, 2))];
        let sinks = NonEmptyRouteSinks::new(expected_sinks.clone()).unwrap();
        let reservations = PhysicalReservations::new();

        let tree = DurablePhysicalRouter
            .route(RouteRequest {
                id: route,
                source: source.clone(),
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 10_000,
                    max_queue_entries: 50_000,
                },
            })
            .unwrap();

        assert_eq!(tree.id, route);
        assert_eq!(tree.source, source.id);
        assert_eq!(
            tree.branches
                .iter()
                .map(|branch| branch.sink)
                .collect::<Vec<_>>(),
            expected_sinks
                .iter()
                .map(|sink| sink.id)
                .collect::<Vec<_>>()
        );
        let shared = tree.branches[0]
            .path
            .iter()
            .filter(|cell| tree.branches[1].path.contains(cell))
            .count();
        assert!(shared >= 1, "fanout must retain an actually shared trunk");
        assert!(tree.cells.iter().all(|cell| matches!(
            cell.state.kind,
            BlockKind::RedstoneWire | BlockKind::Repeater
        )));
        assert!(tree
            .floors
            .iter()
            .all(|floor| floor.state.kind == BlockKind::Solid));
        for (branch, expected) in tree.branches.iter().zip(expected_sinks) {
            assert_eq!(branch.sink, expected.id);
            assert_eq!(branch.target, expected.terminal.target().unwrap());
            assert_eq!(branch.terminal.sink, expected.id);
            assert_eq!(branch.terminal.at, expected.anchor);
            let owned = tree
                .cells
                .iter()
                .find(|cell| cell.at == expected.anchor)
                .unwrap();
            assert_eq!(branch.terminal.state, owned.state);
        }
    }

    #[test]
    fn fragment_adapter_preserves_distinct_typed_sink_identities() {
        let route = RouteId(10);
        let source = endpoint(route);
        let expected = vec![sink(route, 3, at(5, 1, 0)), sink(route, 8, at(5, 1, 2))];
        let sinks = NonEmptyRouteSinks::new(expected.clone()).unwrap();
        let reservations = PhysicalReservations::new();
        let adapter = FragmentRouterAdapter::new(DurablePhysicalRouter);

        let tree = adapter
            .route(RouteRequest {
                id: route,
                source,
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 10_000,
                    max_queue_entries: 50_000,
                },
            })
            .unwrap();

        assert_eq!(tree.branches[0].sink, expected[0].id);
        assert_eq!(tree.branches[1].sink, expected[1].id);
        assert_ne!(tree.branches[0].sink, tree.branches[1].sink);
        assert_ne!(tree.branches[0].target, tree.branches[1].target);
    }

    #[test]
    fn certification_names_the_connection_and_keeps_non_default_repeater_state_exact() {
        let route = RouteId(11);
        let source = endpoint(route);
        let typed_sink = sink(route, 0, at(4, 1, 0));
        let sinks = NonEmptyRouteSinks::new(vec![typed_sink.clone()]).unwrap();
        let mut exact = BlockState::air();
        exact.kind = BlockKind::Repeater;
        exact.name = "minecraft:repeater".to_string();
        exact.facing = Some(Facing::West);
        exact.delay = 4;
        exact.lit = true;
        let mut reservations = PhysicalReservations::new();
        reservations.reserve_conductor(at(2, 1, 0), route, exact.clone());

        let request = RouteRequest {
            id: route,
            source: source.clone(),
            sinks: &sinks,
            reservations: &reservations,
            limits: RouterLimits {
                max_node_expansions: 10_000,
                max_queue_entries: 50_000,
            },
        };
        let tree = DurablePhysicalRouter.route(request).unwrap();
        assert_eq!(
            tree.cells
                .iter()
                .find(|cell| cell.at == at(2, 1, 0))
                .unwrap()
                .state,
            exact
        );

        let mut rotated = exact;
        rotated.facing = Some(Facing::North);
        let path = vec![at(1, 1, 0), at(2, 1, 0), at(3, 1, 0), at(4, 1, 0)];
        let mut states = BTreeMap::new();
        for &cell in &path {
            states.insert(cell, dust());
        }
        states.insert(at(2, 1, 0), rotated);
        let request = RouteRequest {
            id: route,
            source,
            sinks: &sinks,
            reservations: &reservations,
            limits: RouterLimits {
                max_node_expansions: 10_000,
                max_queue_entries: 50_000,
            },
        };

        assert_eq!(
            certify_path(&request, &typed_sink, &path, &states),
            Err(RouterFailure::WrongRepeaterAxis {
                connection: connection(10, 0),
                at: at(2, 1, 0),
            })
        );
    }

    #[test]
    fn later_fanout_branch_cannot_reconstruct_an_existing_trunk_state() {
        let route = RouteId(12);
        let source = endpoint(route);
        let typed_sink = sink(route, 0, at(4, 1, 0));
        let sinks = NonEmptyRouteSinks::new(vec![typed_sink.clone()]).unwrap();
        let reservations = PhysicalReservations::new();
        let request = RouteRequest {
            id: route,
            source,
            sinks: &sinks,
            reservations: &reservations,
            limits: RouterLimits {
                max_node_expansions: 100,
                max_queue_entries: 100,
            },
        };
        let trunk_at = at(2, 1, 0);
        let mut exact = BlockState::air();
        exact.kind = BlockKind::Repeater;
        exact.name = "minecraft:repeater".to_string();
        exact.facing = Some(Facing::West);
        exact.delay = 4;
        exact.lit = true;
        let mut cells = BTreeMap::from([(trunk_at, exact.clone())]);
        let mut cell_order = vec![trunk_at];
        let mut floors = BTreeMap::new();
        let mut floor_order = Vec::new();

        realise_path(
            &request,
            &typed_sink,
            &[at(1, 1, 0), trunk_at, at(3, 1, 0), at(4, 1, 0)],
            15,
            &mut cells,
            &mut cell_order,
            &mut floors,
            &mut floor_order,
        )
        .unwrap();

        assert_eq!(cells.get(&trunk_at), Some(&exact));
    }
}
