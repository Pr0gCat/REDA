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
    RouteStair(RouteId),
    Sink(RoutedSinkId),
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
        if self.cells.contains_key(&at) {
            return None;
        }
        self.cells.insert(at, PhysicalReservation { owner, kind })
    }

    fn reserve_if_free(
        &mut self,
        at: Anchor,
        owner: PhysicalReservationOwner,
        kind: PhysicalReservationKind,
    ) -> bool {
        if self.cells.contains_key(&at) {
            return false;
        }
        self.cells.insert(at, PhysicalReservation { owner, kind });
        true
    }

    pub fn reserve_conductor(&mut self, at: Anchor, owner: RouteId, state: BlockState) {
        self.reserve(
            at,
            PhysicalReservationOwner::Route(owner),
            PhysicalReservationKind::Conductor(state),
        );
    }

    /// Turn one protected source endpoint into an exact route-owned refresh.
    ///
    /// Sparse placement reserves every future source before any route runs so
    /// an earlier net cannot consume it.  A dust junction has no guaranteed
    /// output strength beyond non-zero, so its outgoing route promotes that
    /// protection to a normalising repeater when the route is finally laid.
    /// No other reservation kind or endpoint may be overwritten.
    pub fn promote_endpoint_conductor(
        &mut self,
        at: Anchor,
        endpoint: PhysicalEndpointId,
        route: RouteId,
        state: BlockState,
    ) -> bool {
        let Some(existing) = self.cells.get(&at) else {
            return false;
        };
        if existing.owner != PhysicalReservationOwner::Endpoint(endpoint)
            || existing.kind != PhysicalReservationKind::KeepOut
        {
            return false;
        }
        self.cells.insert(
            at,
            PhysicalReservation {
                owner: PhysicalReservationOwner::Route(route),
                kind: PhysicalReservationKind::Conductor(state),
            },
        );
        true
    }

    /// Release one endpoint-owned keep-out at exactly `at`.
    ///
    /// Only the named endpoint's own keep-out may be withdrawn; any other
    /// owner or kind (route conductor, floor, mandatory air, another
    /// endpoint) is left untouched so a failed release never disturbs the
    /// reservation map mid-transaction.
    pub fn release_endpoint_keep_out(&mut self, at: Anchor, endpoint: PhysicalEndpointId) -> bool {
        let Some(existing) = self.cells.get(&at) else {
            return false;
        };
        if existing.owner != PhysicalReservationOwner::Endpoint(endpoint)
            || existing.kind != PhysicalReservationKind::KeepOut
        {
            return false;
        }
        self.cells.remove(&at);
        true
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteGuidance {
    pub origin: Anchor,
    pub lateral: Facing,
    pub track: i32,
    pub half_width: u32,
    pub penalty_per_block: u64,
}

impl RouteGuidance {
    fn penalty(self, at: Anchor) -> u64 {
        let relative = match self.lateral {
            Facing::North => self.origin.z.saturating_sub(at.z),
            Facing::South => at.z.saturating_sub(self.origin.z),
            Facing::East => at.x.saturating_sub(self.origin.x),
            Facing::West => self.origin.x.saturating_sub(at.x),
            Facing::Up | Facing::Down => 0,
        };
        (u64::from(relative.abs_diff(self.track)) > u64::from(self.half_width))
            .then_some(self.penalty_per_block)
            .unwrap_or(0)
    }
}

pub trait PhysicalRouter {
    fn route(&self, request: RouteRequest<'_>) -> Result<RealisedRouteTree, RouterFailure>;

    fn route_guided(
        &self,
        request: RouteRequest<'_>,
        guidance: Option<RouteGuidance>,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        let _ = guidance;
        self.route(request)
    }
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

    fn route_guided(
        &self,
        request: RouteRequest<'_>,
        guidance: Option<RouteGuidance>,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        self.router.route_guided(request, guidance)
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
    RingClosure {
        route: RouteId,
        source: PhysicalEndpointId,
        sink: RoutedSinkId,
        repeater: Anchor,
        charged: Vec<Anchor>,
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
            Self::RingClosure {
                route,
                sink,
                repeater,
                ..
            } => write!(
                formatter,
                "route {route:?} sink {sink:?} closes a ring through repeater {repeater:?}"
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
            Self::RingClosure { .. } => RouterRefusalCategory::PhysicalInvariant,
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
        route_seed_with_policy(request, RoutingJoinPolicy::Off, |_| 0, |_, _, _| {})
    }

    fn route_guided(
        &self,
        request: RouteRequest<'_>,
        guidance: Option<RouteGuidance>,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        route_seed_with_policy(
            request,
            RoutingJoinPolicy::Off,
            move |at| guidance.map_or(0, |guidance| guidance.penalty(*at)),
            |_, _, _| {},
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutingJoinPolicy {
    Off,
    Narrow,
    Wide,
}

const MAX_RING_REROUTES_PER_BRANCH: usize = 16;

fn branch_floor_overlap(
    suffix: &[Anchor],
    laid_cells: &BTreeMap<Anchor, BlockState>,
    laid_floors: &BTreeMap<Anchor, BlockState>,
) -> Option<Anchor> {
    let suffix_cells = suffix.iter().copied().collect::<BTreeSet<_>>();
    suffix.iter().copied().find(|at| {
        let floor_at = Anchor { y: at.y - 1, ..*at };
        laid_floors.contains_key(at)
            || laid_cells.contains_key(&floor_at)
            || suffix_cells.contains(&floor_at)
    })
}

fn available_tree_roots(
    start: Anchor,
    laid: &BTreeMap<Anchor, BlockState>,
    forbidden: &BTreeSet<Anchor>,
) -> BTreeSet<Anchor> {
    std::iter::once(start)
        .chain(
            laid.keys()
                .copied()
                .filter(|root| !forbidden.contains(root)),
        )
        .collect()
}

/// Private policy seam for legacy-parity congestion costs and transactional
/// claim mirroring.  Public requests and results remain entirely typed.
pub(crate) fn route_with_policy<Price, Claim>(
    request: RouteRequest<'_>,
    join_policy: RoutingJoinPolicy,
    price: Price,
    claim: Claim,
) -> Result<RealisedRouteTree, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
    route_with_local_policy(request, join_policy, false, false, price, claim)
}

/// Strict local route certification for newly generated fragment candidates.
/// The legacy adapter deliberately stays on `route_with_policy` until Task 13
/// accepts a shipping policy change; both paths still share this physical
/// implementation and differ only in whether extraction-time legacy layouts
/// are allowed to retain their established terminal transit behaviour.
pub(crate) fn route_strict_with_policy<Price, Claim>(
    request: RouteRequest<'_>,
    join_policy: RoutingJoinPolicy,
    price: Price,
    claim: Claim,
) -> Result<RealisedRouteTree, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
    route_with_local_policy(request, join_policy, true, false, price, claim)
}

/// The seed's own route policy: strict local certification plus the
/// seed-only rules (fanout tree roots, ring/floor reroutes, source-exit and
/// terminal-feedback keep-outs).  Legacy `lay_net`/`try_move` never see these,
/// so their layouts stay byte-exact.
pub(crate) fn route_seed_with_policy<Price, Claim>(
    request: RouteRequest<'_>,
    join_policy: RoutingJoinPolicy,
    price: Price,
    claim: Claim,
) -> Result<RealisedRouteTree, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
    route_with_local_policy(request, join_policy, true, true, price, claim)
}

fn route_with_local_policy<Price, Claim>(
    request: RouteRequest<'_>,
    join_policy: RoutingJoinPolicy,
    strict_local: bool,
    seed_rules: bool,
    mut price: Price,
    mut claim: Claim,
) -> Result<RealisedRouteTree, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
    let mut work = RouterWork::default();
    route_ordered_attempt(
        request,
        join_policy,
        strict_local,
        seed_rules,
        &mut work,
        &mut price,
        &mut claim,
    )
}

#[allow(clippy::too_many_arguments)]
fn route_ordered_attempt<Price, Claim>(
    request: RouteRequest<'_>,
    join_policy: RoutingJoinPolicy,
    strict_local: bool,
    seed_rules: bool,
    work: &mut RouterWork,
    price: &mut Price,
    claim: &mut Claim,
) -> Result<RealisedRouteTree, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
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
    let start = request.source.anchor;
    let mut reservations = request.reservations.clone();
    let mut cell_states = BTreeMap::<Anchor, BlockState>::new();
    let mut cell_order = Vec::<Anchor>::new();
    let mut floor_states = BTreeMap::<Anchor, BlockState>::new();
    let mut floor_order = Vec::<Anchor>::new();
    let mut branches = Vec::<RealisedRouteBranch>::with_capacity(request.sinks.as_slice().len());
    let mut tree_parent = BTreeMap::<Anchor, Anchor>::new();
    let mut staged_claims =
        Vec::<(Anchor, PhysicalReservationOwner, PhysicalReservationKind)>::new();

    let reroute_limit = if seed_rules {
        MAX_RING_REROUTES_PER_BRANCH
    } else {
        0
    };
    for sink in request.sinks.as_slice() {
        let mut ring_forbidden = BTreeSet::new();
        let mut ring_reroutes = 0usize;
        'branch_attempt: loop {
            let reservations_before = reservations.clone();
            let cell_states_before = cell_states.clone();
            let cell_order_len = cell_order.len();
            let floor_states_before = floor_states.clone();
            let floor_order_len = floor_order.len();
            let branches_len = branches.len();
            let staged_claims_len = staged_claims.len();
            let approach = step(sink.anchor, sink.allowed_entry);
            let own_join =
                TypedOwnJoinCheck::for_branch(join_policy, request.id, &cell_states, &reservations);
            let mut path = search_path(
                &request,
                sink,
                start,
                approach,
                &cell_states,
                &reservations,
                &tree_parent,
                &own_join,
                &ring_forbidden,
                strict_local,
                seed_rules,
                work,
                price,
            )?
            .ok_or(RouterFailure::NoLocalRoute {
                route: request.id,
                source: request.source.id,
                sink: sink.id,
            })?;
            if path.last() != Some(&sink.anchor) {
                path.push(sink.anchor);
            }
            reserve_typed_path(
                request.id,
                &path,
                &mut reservations,
                &mut |at, owner, kind| {
                    // Seed reroutes roll a branch back, so its claims are
                    // staged until the tree is accepted.  The legacy adapter
                    // prices later sinks off the claims made so far, so it
                    // must see them immediately.
                    if seed_rules {
                        staged_claims.push((at, owner, kind));
                    } else {
                        claim(at, owner, kind);
                    }
                },
            );

            let shared = path
                .iter()
                .take_while(|anchor| cell_states.contains_key(anchor))
                .count();
            let mut carried = source_strength;
            let mut previous_cell = request.source.anchor;
            let mut trunk_repeaters = 0u64;
            for anchor in &path[..shared] {
                let state = cell_states
                    .get(anchor)
                    .expect("a shared prefix has an exact laid state");
                if state.kind == BlockKind::Repeater {
                    carried = MAX_SIGNAL_STRENGTH;
                    trunk_repeaters += 1;
                } else {
                    carried = carried.saturating_sub(1);
                }
                previous_cell = *anchor;
            }
            let laid = realise_branch_from_with_boundary_policy(
                previous_cell,
                carried,
                &path[shared..],
                strict_local,
            );
            if !laid.carries {
                let reroute_cell = shared
                    .checked_sub(1)
                    .and_then(|index| path.get(index).copied())
                    .filter(|cell| *cell != start && *cell != sink.anchor);
                if ring_reroutes < reroute_limit {
                    if let Some(reroute_cell) = reroute_cell {
                        if ring_forbidden.insert(reroute_cell) {
                            reservations = reservations_before;
                            cell_states = cell_states_before;
                            cell_order.truncate(cell_order_len);
                            floor_states = floor_states_before;
                            floor_order.truncate(floor_order_len);
                            branches.truncate(branches_len);
                            staged_claims.truncate(staged_claims_len);
                            ring_reroutes += 1;
                            continue 'branch_attempt;
                        }
                    }
                }
                if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                    eprintln!(
                        "strict physical refusal: branch does not carry; route={:?} sink={:?} shared={shared} path_len={} incoming_strength={carried} local_reroutes={ring_reroutes}",
                        request.id,
                        sink.id,
                        path.len(),
                    );
                }
                return Err(RouterFailure::Refused {
                    route: request.id,
                    source: request.source.id,
                    sink: Some(sink.id),
                    category: RouterRefusalCategory::PhysicalInvariant,
                });
            }
            if let Some(overlap) = seed_rules
                .then(|| branch_floor_overlap(&path[shared..], &cell_states, &floor_states))
                .flatten()
            {
                if ring_reroutes < reroute_limit
                    && overlap != start
                    && overlap != sink.anchor
                    && ring_forbidden.insert(overlap)
                {
                    reservations = reservations_before;
                    cell_states = cell_states_before;
                    cell_order.truncate(cell_order_len);
                    floor_states = floor_states_before;
                    floor_order.truncate(floor_order_len);
                    branches.truncate(branches_len);
                    staged_claims.truncate(staged_claims_len);
                    ring_reroutes += 1;
                    continue 'branch_attempt;
                }
                if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                    eprintln!(
                        "strict physical refusal: branch floor overlap; route={:?} sink={:?} at={overlap:?} shared={shared} path_len={} local_reroutes={ring_reroutes}",
                        request.id,
                        sink.id,
                        path.len(),
                    );
                }
                return Err(RouterFailure::Refused {
                    route: request.id,
                    source: request.source.id,
                    sink: Some(sink.id),
                    category: RouterRefusalCategory::PhysicalInvariant,
                });
            }
            let budget_needs_repeater = laid
                .blocks
                .last()
                .is_some_and(|state| state.kind == BlockKind::Repeater);
            for ((&at, planned_state), floor) in path[shared..]
                .iter()
                .zip(laid.blocks.iter().cloned())
                .zip(laid.floors.iter().cloned())
            {
                let Some(state) = state_for_new_cell(
                    request.id,
                    strict_local,
                    request.reservations,
                    &cell_states,
                    at,
                    planned_state,
                ) else {
                    continue;
                };
                cell_states.insert(at, state);
                cell_order.push(at);
                let floor_at = Anchor { y: at.y - 1, ..at };
                let route_owns_floor = reservations.get(&floor_at).is_some_and(|claim| {
                    reservation_is_floor(claim)
                        && matches!(
                            claim.owner,
                            PhysicalReservationOwner::Route(owner)
                                | PhysicalReservationOwner::RouteStair(owner)
                                if owner == request.id
                        )
                });
                if (!seed_rules || route_owns_floor)
                    && floor_states.insert(floor_at, floor).is_none()
                {
                    floor_order.push(floor_at);
                }
            }

            let (target, support, requirement) =
                sink.terminal
                    .sink_parts()
                    .ok_or(RouterFailure::InvalidRequest {
                        route: request.id,
                        source: request.source.id,
                        sink: Some(sink.id),
                    })?;
            let predecessor = path
                .get(path.len().saturating_sub(2))
                .copied()
                .unwrap_or(request.source.anchor);
            let isolated =
                terminal_is_isolated_typed(&reservations, predecessor, sink.anchor, support);
            let kind = select_terminal_kind(
                requirement,
                budget_needs_repeater,
                predecessor,
                sink.anchor,
                support,
                laid.strength_before_terminal,
                isolated,
            );
            let terminal_state = match kind {
                RouteTerminalKind::RepeaterIntoSupport
                | RouteTerminalKind::BareMergeRepeater
                | RouteTerminalKind::OutputTerminalRepeater => {
                    let direction = horizontal_direction(predecessor, sink.anchor)
                    .or_else(|| horizontal_direction(sink.anchor, support))
                    .ok_or_else(|| {
                        if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                            eprintln!("strict physical refusal: terminal has no horizontal direction; sink={sink:?}; predecessor={predecessor:?}");
                        }
                        RouterFailure::Refused {
                        route: request.id,
                        source: request.source.id,
                        sink: Some(sink.id),
                        category: RouterRefusalCategory::PhysicalInvariant,
                    }})?;
                    repeater_toward(direction)
                }
                RouteTerminalKind::DirectedDustIntoSupport | RouteTerminalKind::BareMergeDust => {
                    dust()
                }
            };
            cell_states.insert(sink.anchor, terminal_state.clone());
            reserve_terminal_guard(
                sink.id,
                predecessor,
                sink.anchor,
                support,
                &mut reservations,
                &mut |at, owner, kind| {
                    // Seed reroutes roll a branch back, so its claims are
                    // staged until the tree is accepted.  The legacy adapter
                    // prices later sinks off the claims made so far, so it
                    // must see them immediately.
                    if seed_rules {
                        staged_claims.push((at, owner, kind));
                    } else {
                        claim(at, owner, kind);
                    }
                },
            );
            if strict_local {
                certify_path(&request, sink, &path, &cell_states)?;
            }

            let repeaters = trunk_repeaters + laid.repeaters;
            branches.push(RealisedRouteBranch {
                sink: sink.id,
                target,
                root: *path.first().expect("a routed path contains its start"),
                path,
                terminal: TerminalRecord {
                    sink: sink.id,
                    at: sink.anchor,
                    delayed_owner: (terminal_state.kind == BlockKind::Repeater)
                        .then_some(DelayedOwner::Route(request.id)),
                    state: terminal_state,
                    kind,
                    repeaters,
                },
            });
            if std::env::var_os("REDA_TRACE_ROUTE_SEARCH").is_some() {
                eprintln!(
                "route branch complete: route={:?} sink={:?} path_len={} shared={} queue_entries={} node_expansions={}",
                request.id,
                sink.id,
                branches.last().map_or(0, |branch| branch.path.len()),
                shared,
                work.queue_entries,
                work.node_expansions,
            );
            }

            if let Some((repeater, ring)) = ring_closed_in_typed(&cell_states, &reservations) {
                let branch = branches
                    .last()
                    .expect("the checked branch was just recorded");
                let suffix = &branch.path[shared..];
                let mut charged: Vec<_> = suffix
                    .iter()
                    .copied()
                    .filter(|cell| ring.contains(cell))
                    .collect();
                if charged.is_empty() {
                    charged = suffix.to_vec();
                }
                let failure = RouterFailure::RingClosure {
                    route: request.id,
                    source: request.source.id,
                    sink: sink.id,
                    repeater,
                    charged,
                };
                let reroute_cell = match &failure {
                    RouterFailure::RingClosure { charged, .. } => suffix
                        .iter()
                        .copied()
                        .find(|cell| {
                            *cell == repeater
                                && *cell != start
                                && *cell != sink.anchor
                                && !ring_forbidden.contains(cell)
                        })
                        .or_else(|| {
                            charged.iter().copied().find(|cell| {
                                *cell != start
                                    && *cell != sink.anchor
                                    && !ring_forbidden.contains(cell)
                            })
                        }),
                    _ => unreachable!(),
                };
                if ring_reroutes < reroute_limit {
                    if let Some(reroute_cell) = reroute_cell {
                        reservations = reservations_before;
                        cell_states = cell_states_before;
                        cell_order.truncate(cell_order_len);
                        floor_states = floor_states_before;
                        floor_order.truncate(floor_order_len);
                        branches.truncate(branches_len);
                        staged_claims.truncate(staged_claims_len);
                        ring_forbidden.insert(reroute_cell);
                        ring_reroutes += 1;
                        continue 'branch_attempt;
                    }
                }
                return Err(failure);
            }
            for edge in branches
                .last()
                .expect("the accepted branch was just recorded")
                .path
                .windows(2)
            {
                tree_parent.entry(edge[1]).or_insert(edge[0]);
            }
            break 'branch_attempt;
        }
    }

    for (at, owner, kind) in staged_claims {
        claim(at, owner, kind);
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

fn state_for_new_cell(
    route: RouteId,
    strict_local: bool,
    requested: &PhysicalReservations,
    laid: &BTreeMap<Anchor, BlockState>,
    at: Anchor,
    planned: BlockState,
) -> Option<BlockState> {
    // A later fanout branch may leave the shared prefix and re-enter an
    // already-laid cell.  That cell's exact state is authoritative; returning
    // `None` makes the production insertion loop leave both state and order
    // untouched.
    if laid.contains_key(&at) {
        return None;
    }

    // A durable typed reservation carries an exact BlockState and the strict
    // router must preserve it.  The legacy adapter's own-route
    // `Occupancy::Wire`, however, is only a socket-approach ownership preclaim:
    // the old realiser was still free to put a refresh there.  Treating that
    // compatibility placeholder as exact dust erased the g9 refresh in the
    // all-pinned full adder while leaving its repeater count unchanged.
    Some(
        strict_local
            .then(|| {
                requested
                    .get(&at)
                    .filter(|claim| owned_by_route(claim.owner, route))
                    .and_then(|claim| match &claim.kind {
                        PhysicalReservationKind::Conductor(state) => Some(state.clone()),
                        _ => None,
                    })
            })
            .flatten()
            .unwrap_or(planned),
    )
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

fn reservation_is_conductor(claim: &PhysicalReservation) -> bool {
    matches!(claim.kind, PhysicalReservationKind::Conductor(_))
}

fn reservation_is_floor(claim: &PhysicalReservation) -> bool {
    matches!(claim.kind, PhysicalReservationKind::Floor(_))
}

fn reservation_is_air(claim: &PhysicalReservation) -> bool {
    matches!(claim.kind, PhysicalReservationKind::MandatoryAir)
}

fn owned_by_route(owner: PhysicalReservationOwner, route: RouteId) -> bool {
    matches!(owner, PhysicalReservationOwner::Route(owner) if owner == route)
}

fn staircase_clearance_typed(from: Anchor, to: Anchor) -> Vec<Anchor> {
    if to.y == from.y {
        return Vec::new();
    }
    let riser = Anchor { y: from.y, ..to };
    if to.y > from.y {
        vec![
            riser,
            Anchor {
                y: from.y + 1,
                ..from
            },
        ]
    } else {
        vec![riser]
    }
}

fn self_obstructs_typed(
    previous: &BTreeMap<Anchor, Anchor>,
    at: Anchor,
    next: Anchor,
    seed_rules: bool,
) -> bool {
    let drop_blocker = (next.y < at.y).then(|| Anchor {
        x: next.x,
        y: at.y + 1,
        z: next.z,
    });
    let smothered = Anchor {
        x: next.x,
        y: next.y - 2,
        z: next.z,
    };
    let crushed_below = Anchor {
        y: next.y - 1,
        ..next
    };
    let floor_crushes_next = Anchor {
        y: next.y + 1,
        ..next
    };
    let mut successor = None;
    let mut walk = Some(at);
    while let Some(cell) = walk {
        if Some(cell) == drop_blocker
            || (seed_rules && cell == floor_crushes_next)
            || cell == crushed_below
            || (cell == smothered && successor.is_some_and(|after: Anchor| after.y > cell.y))
        {
            return true;
        }
        successor = Some(cell);
        walk = previous.get(&cell).copied();
    }
    false
}

fn anchor_is_free_for_typed(
    route: RouteId,
    anchor: Anchor,
    start: Anchor,
    goal: Anchor,
    terminal_support: Anchor,
    reservations: &PhysicalReservations,
) -> bool {
    if anchor != start
        && anchor != goal
        && reservations
            .get(&anchor)
            .is_some_and(|claim| !owned_by_route(claim.owner, route))
    {
        return false;
    }
    if reservations.get(&anchor).is_some_and(reservation_is_floor) {
        return false;
    }
    let below = Anchor {
        y: anchor.y - 1,
        ..anchor
    };
    if reservations
        .get(&below)
        .is_some_and(|claim| reservation_is_conductor(claim) || reservation_is_air(claim))
    {
        return false;
    }
    keep_out_typed(anchor).into_iter().all(|neighbour| {
        neighbour == start
            || neighbour == goal
            || (anchor == goal && neighbour == terminal_support)
            || reservations.get(&neighbour).is_none_or(|claim| {
                !reservation_is_conductor(claim) || owned_by_route(claim.owner, route)
            })
    })
}

fn staircase_cell_is_blocked(
    route: RouteId,
    from: Anchor,
    to: Anchor,
    cell: Anchor,
    reservations: &PhysicalReservations,
    seed_rules: bool,
) -> bool {
    let is_riser = to.y > from.y && cell.y == from.y;
    let Some(claim) = reservations.get(&cell) else {
        return false;
    };
    if is_riser {
        return (!owned_by_route(claim.owner, route)
            && claim.owner != PhysicalReservationOwner::RouteStair(route))
            || reservation_is_conductor(claim);
    }
    if seed_rules
        && claim.owner == PhysicalReservationOwner::RouteStair(route)
        && reservation_is_air(claim)
    {
        return false;
    }
    true
}

fn keep_out_typed(anchor: Anchor) -> Vec<Anchor> {
    let mut cells = Vec::with_capacity(12);
    for neighbour in horizontal_neighbours_typed(anchor) {
        cells.push(neighbour);
        cells.push(Anchor {
            y: neighbour.y + 1,
            ..neighbour
        });
        cells.push(Anchor {
            y: neighbour.y - 1,
            ..neighbour
        });
    }
    cells
}

fn horizontal_neighbours_typed(anchor: Anchor) -> [Anchor; 4] {
    [
        Anchor {
            x: anchor.x - 1,
            ..anchor
        },
        Anchor {
            x: anchor.x + 1,
            ..anchor
        },
        Anchor {
            z: anchor.z - 1,
            ..anchor
        },
        Anchor {
            z: anchor.z + 1,
            ..anchor
        },
    ]
}

fn reserve_typed_path<Claim>(
    route: RouteId,
    path: &[Anchor],
    reservations: &mut PhysicalReservations,
    claim: &mut Claim,
) where
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
    for window in path.windows(2) {
        for cell in staircase_clearance_typed(window[0], window[1]) {
            let is_riser = window[1].y > window[0].y && cell.y == window[0].y;
            let kind = if is_riser {
                PhysicalReservationKind::Floor(stone())
            } else {
                PhysicalReservationKind::MandatoryAir
            };
            let owner = PhysicalReservationOwner::RouteStair(route);
            if reservations.reserve_if_free(cell, owner, kind.clone()) {
                claim(cell, owner, kind);
            }
        }
    }
    for &at in path {
        let owner = PhysicalReservationOwner::Route(route);
        let conductor = PhysicalReservationKind::Conductor(dust());
        if reservations.reserve_if_free(at, owner, conductor.clone()) {
            claim(at, owner, conductor);
        }
        let floor_at = Anchor { y: at.y - 1, ..at };
        let floor = PhysicalReservationKind::Floor(stone());
        if reservations.reserve_if_free(floor_at, owner, floor.clone()) {
            claim(floor_at, owner, floor);
        }
    }
}

fn terminal_is_isolated_typed(
    reservations: &PhysicalReservations,
    predecessor: Anchor,
    terminal: Anchor,
    support: Anchor,
) -> bool {
    horizontal_neighbours_typed(terminal)
        .into_iter()
        .all(|neighbour| {
            neighbour == predecessor
                || neighbour == support
                || reservations
                    .get(&neighbour)
                    .is_none_or(|claim| !reservation_is_conductor(claim))
        })
}

fn select_terminal_kind(
    requirement: TerminalRequirement,
    budget_needs_repeater: bool,
    predecessor: Anchor,
    terminal: Anchor,
    support: Anchor,
    predecessor_strength: u8,
    isolation_proven: bool,
) -> RouteTerminalKind {
    match requirement {
        TerminalRequirement::Exact(kind) => kind,
        TerminalRequirement::Repeater => RouteTerminalKind::RepeaterIntoSupport,
        TerminalRequirement::DirectedDust => RouteTerminalKind::DirectedDustIntoSupport,
        TerminalRequirement::Automatic if budget_needs_repeater => {
            RouteTerminalKind::RepeaterIntoSupport
        }
        TerminalRequirement::Automatic => terminal_style(&TerminalApproach::new(
            predecessor,
            terminal,
            support,
            predecessor_strength,
            isolation_proven,
        ))
        .into(),
    }
}

fn reserve_terminal_guard<Claim>(
    sink: RoutedSinkId,
    predecessor: Anchor,
    terminal: Anchor,
    support: Anchor,
    reservations: &mut PhysicalReservations,
    claim: &mut Claim,
) where
    Claim: FnMut(Anchor, PhysicalReservationOwner, PhysicalReservationKind),
{
    for neighbour in horizontal_neighbours_typed(terminal) {
        if neighbour == predecessor || neighbour == support {
            continue;
        }
        let owner = PhysicalReservationOwner::Sink(sink);
        let kind = PhysicalReservationKind::KeepOut;
        if reservations.reserve_if_free(neighbour, owner, kind.clone()) {
            claim(neighbour, owner, kind);
        }
    }
}

fn join_lid_typed(anchor: Anchor, neighbour: Anchor) -> Option<Anchor> {
    match neighbour.y.cmp(&anchor.y) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => Some(Anchor {
            y: anchor.y + 1,
            ..anchor
        }),
        std::cmp::Ordering::Less => Some(Anchor {
            y: anchor.y,
            ..neighbour
        }),
    }
}

fn dust_join_neighbours_typed(cell: Anchor, sealed: &impl Fn(Anchor) -> bool) -> Vec<Anchor> {
    keep_out_typed(cell)
        .into_iter()
        .filter(|neighbour| !join_lid_typed(cell, *neighbour).is_some_and(sealed))
        .collect()
}

fn ring_closed_in_typed(
    states: &BTreeMap<Anchor, BlockState>,
    reservations: &PhysicalReservations,
) -> Option<(Anchor, BTreeSet<Anchor>)> {
    let sealed = |lid: Anchor| {
        !states.contains_key(&lid) && reservations.get(&lid).is_some_and(reservation_is_floor)
    };
    let repeater_input = |cell: Anchor, state: &BlockState| {
        (state.kind == BlockKind::Repeater)
            .then(|| state.facing.map(|facing| step(cell, facing)))
            .flatten()
    };
    let repeater_drives = |cell: Anchor, state: &BlockState| -> Vec<Anchor> {
        let Some(facing) = state.facing else {
            return Vec::new();
        };
        let output = step(cell, facing.opposite());
        if let Some(next) = states.get(&output) {
            return match next.kind {
                BlockKind::RedstoneWire => vec![output],
                BlockKind::Repeater if repeater_input(output, next) == Some(cell) => vec![output],
                _ => Vec::new(),
            };
        }
        let above = Anchor {
            y: output.y + 1,
            ..output
        };
        let ships_solid = states.contains_key(&above)
            || reservations.get(&output).is_some_and(reservation_is_floor);
        if !ships_solid {
            return Vec::new();
        }
        horizontal_neighbours_typed(output)
            .into_iter()
            .chain([
                Anchor {
                    y: output.y + 1,
                    ..output
                },
                Anchor {
                    y: output.y - 1,
                    ..output
                },
            ])
            .filter(|face| {
                states
                    .get(face)
                    .is_some_and(|standing| standing.kind == BlockKind::RedstoneWire)
            })
            .collect()
    };
    let steps_from = |cell: Anchor| -> Vec<Anchor> {
        let mut out = Vec::new();
        let Some(state) = states.get(&cell) else {
            return out;
        };
        match state.kind {
            BlockKind::RedstoneWire => {
                for joined in dust_join_neighbours_typed(cell, &sealed) {
                    if let Some(neighbour) = states.get(&joined) {
                        match neighbour.kind {
                            BlockKind::RedstoneWire => out.push(joined),
                            BlockKind::Repeater
                                if joined.y == cell.y
                                    && repeater_input(joined, neighbour) == Some(cell) =>
                            {
                                out.push(joined);
                            }
                            _ => {}
                        }
                    }
                }
            }
            BlockKind::Repeater => out.extend(repeater_drives(cell, state)),
            _ => {}
        }
        out
    };

    for (&cell, state) in states {
        let Some(input) = repeater_input(cell, state) else {
            continue;
        };
        if !states.contains_key(&input) {
            continue;
        }
        let mut seen = BTreeSet::from([cell]);
        let mut frontier = repeater_drives(cell, state);
        while let Some(at) = frontier.pop() {
            if !seen.insert(at) {
                continue;
            }
            if at == input {
                return Some((cell, seen));
            }
            frontier.extend(
                steps_from(at)
                    .into_iter()
                    .filter(|next| !seen.contains(next)),
            );
        }
    }
    None
}

struct TypedOwnJoinCheck {
    policy: RoutingJoinPolicy,
    route: RouteId,
    dust_component: BTreeMap<Anchor, u32>,
}

impl TypedOwnJoinCheck {
    fn for_branch(
        policy: RoutingJoinPolicy,
        route: RouteId,
        states: &BTreeMap<Anchor, BlockState>,
        reservations: &PhysicalReservations,
    ) -> Self {
        if policy != RoutingJoinPolicy::Narrow {
            return Self {
                policy,
                route,
                dust_component: BTreeMap::new(),
            };
        }
        let sealed = |lid: Anchor| {
            !states.contains_key(&lid) && reservations.get(&lid).is_some_and(reservation_is_floor)
        };
        let mut dust_component = BTreeMap::new();
        let mut next_component = 0u32;
        for (&cell, state) in states {
            if state.kind != BlockKind::RedstoneWire || dust_component.contains_key(&cell) {
                continue;
            }
            let component = next_component;
            next_component += 1;
            let mut frontier = vec![cell];
            while let Some(at) = frontier.pop() {
                if dust_component.insert(at, component).is_some() {
                    continue;
                }
                frontier.extend(dust_join_neighbours_typed(at, &sealed).into_iter().filter(
                    |joined| {
                        states
                            .get(joined)
                            .is_some_and(|block| block.kind == BlockKind::RedstoneWire)
                            && !dust_component.contains_key(joined)
                    },
                ));
            }
        }
        Self {
            policy,
            route,
            dust_component,
        }
    }

    fn blocks(
        &self,
        next: Anchor,
        at: Anchor,
        start: Anchor,
        goal: Anchor,
        reservations: &PhysicalReservations,
        previous: &BTreeMap<Anchor, Anchor>,
    ) -> bool {
        if self.policy == RoutingJoinPolicy::Off {
            return false;
        }
        let own_wire = |cell: &Anchor| {
            reservations.get(cell).is_some_and(|claim| {
                owned_by_route(claim.owner, self.route) && reservation_is_conductor(claim)
            })
        };
        if own_wire(&next) {
            return !(at == start || next == goal || own_wire(&at));
        }
        let halo: Vec<_> = dust_join_neighbours_typed(next, &|lid| {
            reservations.get(&lid).is_some_and(reservation_is_floor)
        })
        .into_iter()
        .filter(|cell| *cell != at && *cell != goal)
        .collect();
        for cell in &halo {
            if !own_wire(cell) {
                continue;
            }
            match self.policy {
                RoutingJoinPolicy::Wide => return true,
                RoutingJoinPolicy::Narrow => {
                    let departure =
                        std::iter::successors(Some(at), |cell| previous.get(cell).copied())
                            .find(|cell| own_wire(cell))
                            .unwrap_or(start);
                    let same_component = self
                        .dust_component
                        .get(&departure)
                        .zip(self.dust_component.get(cell))
                        .is_some_and(|(mine, joined)| mine == joined);
                    if !same_component {
                        return true;
                    }
                }
                RoutingJoinPolicy::Off => unreachable!(),
            }
        }
        if self.policy == RoutingJoinPolicy::Wide {
            let mut walk = previous.get(&at).copied();
            while let Some(cell) = walk {
                if halo.contains(&cell) {
                    return true;
                }
                walk = previous.get(&cell).copied();
            }
        }
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn search_path<Price>(
    request: &RouteRequest<'_>,
    sink: &RouteSink,
    start: Anchor,
    goal: Anchor,
    laid: &BTreeMap<Anchor, BlockState>,
    reservations: &PhysicalReservations,
    tree_parent: &BTreeMap<Anchor, Anchor>,
    own_join: &TypedOwnJoinCheck,
    forbidden: &BTreeSet<Anchor>,
    strict_local: bool,
    seed_rules: bool,
    work: &mut RouterWork,
    price: &mut Price,
) -> Result<Option<Vec<Anchor>>, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
{
    let margin = manhattan(start, goal).saturating_add(2) as i32;
    let min = Anchor {
        x: start.x.min(goal.x).saturating_sub(margin),
        y: start.y.min(goal.y),
        z: start.z.min(goal.z).saturating_sub(margin),
    };
    let max = Anchor {
        x: start.x.max(goal.x).saturating_add(margin),
        y: start
            .y
            .max(goal.y)
            .saturating_add(if seed_rules { 6 } else { 3 }),
        z: start.z.max(goal.z).saturating_add(margin),
    };
    let roots = if seed_rules {
        available_tree_roots(start, laid, forbidden)
    } else {
        BTreeSet::from([start])
    };
    let mut frontier = BTreeSet::new();
    let mut travelled = BTreeMap::new();
    for root in roots {
        work.queue(request, sink.id)?;
        frontier.insert(SearchState {
            estimate: manhattan(root, goal),
            travelled: 0,
            at: root,
        });
        travelled.insert(root, 0);
    }
    let mut previous = if seed_rules {
        tree_parent.clone()
    } else {
        BTreeMap::new()
    };
    let required_source_exit = step(start, request.source.allowed_exit);
    let terminal_feedback_keep_out = seed_rules.then(|| terminal_feedback_keep_out(sink));

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
            if forbidden.contains(&next) && next != start && next != goal {
                continue;
            }
            if seed_rules && state.at == start && next != required_source_exit {
                continue;
            }
            if terminal_feedback_keep_out
                .as_ref()
                .is_some_and(|keep_out| keep_out.contains(&next))
            {
                continue;
            }
            let above_terminal = Anchor {
                y: sink.anchor.y.saturating_add(1),
                ..sink.anchor
            };
            let above_approach = Anchor {
                y: goal.y.saturating_add(1),
                ..goal
            };
            if seed_rules && (next == above_terminal || next == above_approach) {
                continue;
            }
            if strict_local && next == sink.anchor && next != goal {
                continue;
            }
            if self_obstructs_typed(&previous, state.at, next, seed_rules) {
                continue;
            }
            if strict_local {
                if let Some(&before) = previous.get(&state.at) {
                    let exact = laid.get(&state.at).cloned().or_else(|| {
                        request.reservations.get(&state.at).and_then(|claim| {
                            owned_by_route(claim.owner, request.id)
                                .then_some(&claim.kind)
                                .and_then(|kind| match kind {
                                    PhysicalReservationKind::Conductor(state) => {
                                        Some(state.clone())
                                    }
                                    _ => None,
                                })
                        })
                    });
                    if exact.as_ref().is_some_and(|at_state| {
                        !route_step_is_legal(before, state.at, next, at_state)
                    }) {
                        continue;
                    }
                }
            }
            if own_join.blocks(next, state.at, start, goal, reservations, &previous) {
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
            let anchor_free =
                anchor_is_free_for_typed(request.id, next, start, goal, sink.anchor, reservations);
            let stair_blocked = staircase_clearance_typed(state.at, next)
                .into_iter()
                .any(|cell| {
                    staircase_cell_is_blocked(
                        request.id,
                        state.at,
                        next,
                        cell,
                        reservations,
                        seed_rules,
                    )
                });
            if !anchor_free || stair_blocked {
                continue;
            }
            // Preserve the legacy distance kernel exactly while it is routed
            // through this typed authority.  Returning toward the sink's Y
            // plane costs one; only a vertical step that moves farther away
            // costs the staircase premium.  Charging every vertical step
            // three changed equal-cost path ordering (and therefore emitted
            // repeater locations) for pinned layouts during extraction.
            let closer_in_y = (next.y - goal.y).abs() < (state.at.y - goal.y).abs();
            let step_cost = if next.y == state.at.y || closer_in_y {
                1
            } else {
                3
            };
            let next_travelled = state
                .travelled
                .saturating_add(step_cost)
                .saturating_add(price(&next));
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
    if std::env::var_os("REDA_TRACE_ROUTE_SEARCH").is_some() {
        let mut nearest = travelled.keys().copied().collect::<Vec<_>>();
        nearest.sort_by_key(|anchor| (manhattan(*anchor, goal), *anchor));
        nearest.truncate(8);
        let goal_neighbours = neighbours(goal)
            .into_iter()
            .map(|anchor| {
                let free = anchor_is_free_for_typed(
                    request.id,
                    anchor,
                    start,
                    goal,
                    sink.anchor,
                    reservations,
                );
                let stair = staircase_clearance_typed(anchor, goal)
                    .into_iter()
                    .filter(|cell| {
                        staircase_cell_is_blocked(
                            request.id,
                            anchor,
                            goal,
                            *cell,
                            reservations,
                            seed_rules,
                        )
                    })
                    .collect::<Vec<_>>();
                (
                    anchor,
                    travelled.get(&anchor).copied(),
                    reservations.get(&anchor).cloned(),
                    free,
                    stair,
                    terminal_feedback_keep_out
                        .as_ref()
                        .is_some_and(|keep_out| keep_out.contains(&anchor)),
                )
            })
            .collect::<Vec<_>>();
        let source_exit_neighbours = neighbours(required_source_exit)
            .into_iter()
            .map(|next| {
                let below = Anchor {
                    y: next.y - 1,
                    ..next
                };
                let anchor_blockers = [next, below]
                    .into_iter()
                    .chain(keep_out_typed(next))
                    .filter_map(|cell| {
                        reservations.get(&cell).and_then(|claim| {
                            ((cell == next && !owned_by_route(claim.owner, request.id))
                                || (cell == below
                                    && (reservation_is_conductor(claim)
                                        || reservation_is_air(claim)))
                                || (cell != next
                                    && cell != below
                                    && reservation_is_conductor(claim)
                                    && !owned_by_route(claim.owner, request.id)))
                            .then_some((cell, claim.clone()))
                        })
                    })
                    .collect::<Vec<_>>();
                let exact = laid.get(&required_source_exit).cloned().or_else(|| {
                    request
                        .reservations
                        .get(&required_source_exit)
                        .and_then(|claim| {
                            owned_by_route(claim.owner, request.id)
                                .then_some(&claim.kind)
                                .and_then(|kind| match kind {
                                    PhysicalReservationKind::Conductor(state) => {
                                        Some(state.clone())
                                    }
                                    _ => None,
                                })
                        })
                });
                (
                    next,
                    reservations.get(&next).cloned(),
                    self_obstructs_typed(&previous, required_source_exit, next, seed_rules),
                    exact.as_ref().is_some_and(|state| {
                        !route_step_is_legal(start, required_source_exit, next, state)
                    }),
                    own_join.blocks(
                        next,
                        required_source_exit,
                        start,
                        goal,
                        reservations,
                        &previous,
                    ),
                    anchor_is_free_for_typed(
                        request.id,
                        next,
                        start,
                        goal,
                        sink.anchor,
                        reservations,
                    ),
                    staircase_clearance_typed(required_source_exit, next)
                        .into_iter()
                        .filter(|cell| {
                            staircase_cell_is_blocked(
                                request.id,
                                required_source_exit,
                                next,
                                *cell,
                                reservations,
                                seed_rules,
                            )
                        })
                        .collect::<Vec<_>>(),
                    anchor_blockers,
                )
            })
            .collect::<Vec<_>>();
        eprintln!(
            "route search exhausted: route={:?} sink={:?} start={start:?} required_source_exit={required_source_exit:?} source_exit_reservation={:?} source_exit_free={} source_exit_neighbours(next,reservation,self_obstruct,axis,own_join,free,stair,anchor_blockers)={source_exit_neighbours:?} goal={goal:?} nearest={nearest:?} goal_neighbours={goal_neighbours:?}",
            request.id,
            sink.id,
            reservations.get(&required_source_exit),
            anchor_is_free_for_typed(
                request.id,
                required_source_exit,
                start,
                goal,
                sink.anchor,
                reservations,
            ),
        );
    }
    Ok(None)
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
        if index == 0 && at == request.source.anchor {
            continue;
        }
        let previous = index
            .checked_sub(1)
            .map(|before| path[before])
            .unwrap_or(request.source.anchor);
        let next = path.get(index + 1).copied().unwrap_or(support);
        let state = states.get(&at).expect("realisation covers every path cell");
        if !route_step_is_legal(previous, at, next, state) {
            if state.kind == BlockKind::Repeater {
                return match sink_connection(sink) {
                    Ok(connection) => Err(RouterFailure::WrongRepeaterAxis { connection, at }),
                    Err(_) => Err(RouterFailure::Refused {
                        route: request.id,
                        source: request.source.id,
                        sink: Some(sink.id),
                        category: RouterRefusalCategory::PhysicalInvariant,
                    }),
                };
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

fn terminal_feedback_keep_out(sink: &RouteSink) -> BTreeSet<Anchor> {
    let Some((_, support, requirement)) = sink.terminal.sink_parts() else {
        return BTreeSet::new();
    };
    let may_be_repeater = match requirement {
        TerminalRequirement::Automatic | TerminalRequirement::Repeater => true,
        TerminalRequirement::DirectedDust => false,
        TerminalRequirement::Exact(kind) => matches!(
            kind,
            RouteTerminalKind::RepeaterIntoSupport
                | RouteTerminalKind::BareMergeRepeater
                | RouteTerminalKind::OutputTerminalRepeater
        ),
    };
    if !may_be_repeater {
        return BTreeSet::new();
    }

    let mut keep_out = [support]
        .into_iter()
        .chain(horizontal_neighbours_typed(support))
        .chain([
            Anchor {
                y: support.y + 1,
                ..support
            },
            Anchor {
                y: support.y - 1,
                ..support
            },
        ])
        .filter(|cell| *cell != sink.anchor)
        .collect::<BTreeSet<_>>();
    let approach = step(sink.anchor, sink.allowed_entry);
    let runway = step(approach, sink.allowed_entry);
    for cell in [approach, runway] {
        keep_out.extend([
            Anchor {
                y: cell.y + 1,
                ..cell
            },
            Anchor {
                y: cell.y - 1,
                ..cell
            },
            Anchor {
                y: cell.y - 2,
                ..cell
            },
        ]);
    }
    keep_out
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
#[cfg(test)]
pub(crate) fn realise_branch_from(
    previous_cell: Anchor,
    incoming: u8,
    cells: &[Anchor],
) -> LaidBranch {
    realise_branch_from_with_boundary_policy(previous_cell, incoming, cells, false)
}

fn realise_branch_from_with_boundary_policy(
    previous_cell: Anchor,
    incoming: u8,
    cells: &[Anchor],
    include_boundary_bend: bool,
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
    if include_boundary_bend
        && cells.len() >= 2
        && path_direction(source, cells[0]) != path_direction(cells[0], cells[1])
    {
        bends.insert(0);
    }
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

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StrengthSearchState {
    estimate: u64,
    travelled: u64,
    anchor: Anchor,
    entered: u8,
    carried: u8,
}

#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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
    fn track_guidance_charges_each_outside_route_cell_once() {
        let guidance = RouteGuidance {
            origin: at(0, 1, 0),
            lateral: Facing::South,
            track: 6,
            half_width: 2,
            penalty_per_block: 3,
        };

        assert_eq!(guidance.penalty(at(20, 1, 4)), 0);
        assert_eq!(guidance.penalty(at(-20, 5, 8)), 0);
        assert_eq!(guidance.penalty(at(0, 1, 10)), 3);
        assert_eq!(guidance.penalty(at(0, 1, 0)), 3);
    }

    #[test]
    fn repeater_terminal_protects_vertical_clearance_over_its_runway() {
        let route = RouteId(7);
        let terminal = at(10, 1, 20);
        let sink = sink(route, 0, terminal);
        let approach = at(9, 1, 20);
        let runway = at(8, 1, 20);

        let keep_out = terminal_feedback_keep_out(&sink);

        for cell in [approach, runway] {
            assert!(!keep_out.contains(&cell));
            assert!(keep_out.contains(&Anchor { y: 2, ..cell }));
            assert!(keep_out.contains(&Anchor { y: 0, ..cell }));
            assert!(keep_out.contains(&Anchor { y: -1, ..cell }));
        }
    }

    #[test]
    fn only_the_named_endpoint_can_be_promoted_to_an_exact_route_refresh() {
        let at = at(3, 1, 4);
        let endpoint = PhysicalEndpointId::Junction(InstanceId(7));
        let route = RouteId(9);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            at,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );
        let exact = crate::compile::repeater(Facing::North);

        assert!(!reservations.promote_endpoint_conductor(
            at,
            PhysicalEndpointId::Junction(InstanceId(8)),
            route,
            exact.clone(),
        ));
        assert!(reservations.promote_endpoint_conductor(at, endpoint, route, exact.clone(),));
        assert_eq!(
            reservations.get(&at),
            Some(&PhysicalReservation {
                owner: PhysicalReservationOwner::Route(route),
                kind: PhysicalReservationKind::Conductor(exact),
            })
        );
    }

    #[test]
    fn release_endpoint_keep_out_removes_only_the_named_endpoints_keep_out() {
        let anchor = at(2, 1, 5);
        let endpoint = PhysicalEndpointId::Junction(InstanceId(3));
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            anchor,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );

        assert!(reservations.release_endpoint_keep_out(anchor, endpoint));
        assert_eq!(reservations.get(&anchor), None);
        assert_eq!(reservations, PhysicalReservations::new());
    }

    #[test]
    fn release_endpoint_keep_out_rejects_the_wrong_endpoint_and_missing_anchor() {
        let anchor = at(2, 1, 5);
        let endpoint = PhysicalEndpointId::Junction(InstanceId(3));
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            anchor,
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::KeepOut,
        );
        let before = reservations.clone();

        assert!(!reservations
            .release_endpoint_keep_out(anchor, PhysicalEndpointId::Junction(InstanceId(4))));
        assert!(!reservations.release_endpoint_keep_out(at(9, 9, 9), endpoint));
        assert_eq!(reservations, before);
    }

    #[test]
    fn release_endpoint_keep_out_rejects_other_owners_and_non_keep_out_kinds() {
        let endpoint = PhysicalEndpointId::Junction(InstanceId(3));
        let route = RouteId(6);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            at(0, 1, 0),
            PhysicalReservationOwner::Route(route),
            PhysicalReservationKind::KeepOut,
        );
        reservations.reserve(
            at(1, 1, 0),
            PhysicalReservationOwner::Sink(RoutedSinkId { route, ordinal: 0 }),
            PhysicalReservationKind::KeepOut,
        );
        reservations.reserve(
            at(2, 1, 0),
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::Conductor(crate::compile::repeater(Facing::North)),
        );
        reservations.reserve(
            at(3, 1, 0),
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::Floor(stone()),
        );
        reservations.reserve(
            at(4, 1, 0),
            PhysicalReservationOwner::Endpoint(endpoint),
            PhysicalReservationKind::MandatoryAir,
        );
        let before = reservations.clone();

        for x in 0..5 {
            assert!(!reservations.release_endpoint_keep_out(at(x, 1, 0), endpoint));
        }
        assert_eq!(reservations, before);
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
    fn a_route_using_an_existing_foreign_support_does_not_claim_that_floor() {
        let route = RouteId(8);
        let source = endpoint(route);
        let sinks = NonEmptyRouteSinks::new(vec![sink(route, 0, at(4, 1, 0))]).unwrap();
        let foreign_support = at(2, 0, 0);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            foreign_support,
            PhysicalReservationOwner::KeepOut(99),
            PhysicalReservationKind::KeepOut,
        );

        let tree = DurablePhysicalRouter
            .route(RouteRequest {
                id: route,
                source,
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 10_000,
                    max_queue_entries: 10_000,
                },
            })
            .unwrap();

        assert!(tree.cells.iter().any(|block| block.at == at(2, 1, 0)));
        assert!(!tree.floors.iter().any(|block| block.at == foreign_support));
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
    fn branch_suffix_boundary_bend_never_receives_a_repeater() {
        let source = at(1, 1, 0);
        let cells = [at(2, 1, 0), at(2, 1, 1), at(2, 1, 2)];

        let laid = realise_branch_from_with_boundary_policy(source, 2, &cells, true);

        assert_ne!(
            laid.blocks[0].kind,
            BlockKind::Repeater,
            "a refresh at the first suffix cell would enter from east and leave south"
        );
    }

    #[test]
    fn a_new_branch_cannot_place_a_conductor_in_an_existing_floor() {
        let overlap = at(3, 1, 4);
        let floors = BTreeMap::from([(overlap, stone())]);

        assert_eq!(
            branch_floor_overlap(&[at(2, 1, 4), overlap], &BTreeMap::new(), &floors),
            Some(overlap),
        );
    }

    #[test]
    fn a_new_branch_cannot_place_its_floor_on_an_existing_conductor() {
        let conductor = at(3, 1, 4);
        let overlap = Anchor {
            y: conductor.y + 1,
            ..conductor
        };
        let cells = BTreeMap::from([(conductor, dust())]);

        assert_eq!(
            branch_floor_overlap(&[at(2, 2, 4), overlap], &cells, &BTreeMap::new()),
            Some(overlap),
        );
    }

    #[test]
    fn a_branch_suffix_cannot_stack_a_floor_on_its_own_conductor() {
        let lower = at(3, 1, 4);
        let upper = Anchor {
            y: lower.y + 1,
            ..lower
        };

        assert_eq!(
            branch_floor_overlap(
                &[lower, at(4, 2, 4), upper],
                &BTreeMap::new(),
                &BTreeMap::new()
            ),
            Some(upper),
        );
    }

    #[test]
    fn a_rejected_tree_join_is_not_reintroduced_as_an_astar_root() {
        let start = at(0, 1, 0);
        let rejected = at(4, 1, 0);
        let usable = at(2, 1, 0);
        let laid = BTreeMap::from([(rejected, dust()), (usable, dust())]);

        assert_eq!(
            available_tree_roots(start, &laid, &BTreeSet::from([rejected])),
            BTreeSet::from([start, usable]),
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
    fn strict_segment_fanout_does_not_close_a_refresh_ring() {
        let route = RouteId(1);
        let source = RouteEndpoint {
            id: PhysicalEndpointId::PrimitiveOutput(
                crate::compile::fragment_synth::identity::PrimitiveId {
                    instance: InstanceId(4),
                    node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
                },
            ),
            anchor: at(31, 1, 40),
            allowed_exit: Facing::East,
            terminal: TerminalContract::Source {
                signal_strength: 15,
            },
        };
        let mut reservations = PhysicalReservations::new();
        for z in 42..=55 {
            reservations.reserve_conductor(at(32, 1, z), RouteId(0), dust());
            reservations.reserve(
                at(32, 0, z),
                PhysicalReservationOwner::RouteStair(RouteId(0)),
                PhysicalReservationKind::Floor(stone()),
            );
        }
        let make_sink = |ordinal: u16, instance: u32, terminal: Anchor| {
            let connection = ConnectionId::External {
                instance: InstanceId(instance),
                input_index: 0,
            };
            RouteSink {
                id: RoutedSinkId { route, ordinal },
                endpoint: PhysicalEndpointId::Landing(connection),
                anchor: terminal,
                allowed_entry: Facing::North,
                terminal: TerminalContract::Sink {
                    target: RouteTarget::Connection(connection),
                    support: at(terminal.x, terminal.y, terminal.z + 1),
                    requirement: TerminalRequirement::Repeater,
                },
            }
        };
        let sinks = NonEmptyRouteSinks::new(vec![
            make_sink(0, 7, at(36, 1, 43)),
            make_sink(1, 17, at(36, 1, 49)),
            make_sink(2, 11, at(36, 1, 199)),
        ])
        .unwrap();

        let result = DurablePhysicalRouter.route(RouteRequest {
            id: route,
            source,
            sinks: &sinks,
            reservations: &reservations,
            limits: RouterLimits {
                max_node_expansions: 262_144,
                max_queue_entries: 262_144,
            },
        });

        assert!(
            result.is_ok(),
            "the literal segment fanout request has legal space but closed a refresh ring: {result:?}"
        );
    }

    #[test]
    fn strict_segment_south_fanout_can_leave_its_nearest_branch() {
        let route = RouteId(1);
        let source = RouteEndpoint {
            id: PhysicalEndpointId::PrimitiveOutput(
                crate::compile::fragment_synth::identity::PrimitiveId {
                    instance: InstanceId(5),
                    node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
                },
            ),
            anchor: at(31, 1, 96),
            allowed_exit: Facing::East,
            terminal: TerminalContract::Source {
                signal_strength: 15,
            },
        };
        let make_sink = |ordinal: u16, instance: u32, terminal: Anchor| {
            let connection = ConnectionId::External {
                instance: InstanceId(instance),
                input_index: 1,
            };
            RouteSink {
                id: RoutedSinkId { route, ordinal },
                endpoint: PhysicalEndpointId::Landing(connection),
                anchor: terminal,
                allowed_entry: Facing::South,
                terminal: TerminalContract::Sink {
                    target: RouteTarget::Connection(connection),
                    support: at(terminal.x, terminal.y, terminal.z - 1),
                    requirement: TerminalRequirement::Repeater,
                },
            }
        };
        let sinks = NonEmptyRouteSinks::new(vec![
            make_sink(0, 7, at(36, 1, 95)),
            make_sink(1, 14, at(36, 1, 17)),
            make_sink(2, 17, at(36, 1, 101)),
            make_sink(3, 32, at(36, 1, 137)),
            make_sink(4, 35, at(36, 1, 143)),
            make_sink(5, 11, at(36, 1, 131)),
        ])
        .unwrap();

        let result = DurablePhysicalRouter.route(RouteRequest {
            id: route,
            source,
            sinks: &sinks,
            reservations: &PhysicalReservations::new(),
            limits: RouterLimits {
                max_node_expansions: 262_144,
                max_queue_entries: 262_144,
            },
        });

        assert!(
            result.is_ok(),
            "the nearest sink must not trap later fanout branches: {result:?}"
        );
    }

    #[test]
    fn guided_eight_sink_tree_grows_from_its_trunk_under_a_bounded_work_cap() {
        let route = RouteId(0);
        let source = RouteEndpoint {
            id: PhysicalEndpointId::PrimitiveOutput(
                crate::compile::fragment_synth::identity::PrimitiveId {
                    instance: InstanceId(4),
                    node: crate::compile::fragment_synth::identity::TopologyNodeId(0),
                },
            ),
            anchor: at(30, 1, 40),
            allowed_exit: Facing::East,
            terminal: TerminalContract::Source {
                signal_strength: 15,
            },
        };
        let make_sink = |ordinal: u16, instance: u32, z: i32| {
            let connection = ConnectionId::External {
                instance: InstanceId(instance),
                input_index: 0,
            };
            RouteSink {
                id: RoutedSinkId { route, ordinal },
                endpoint: PhysicalEndpointId::Landing(connection),
                anchor: at(36, 1, z),
                allowed_entry: Facing::East,
                terminal: TerminalContract::Sink {
                    target: RouteTarget::Connection(connection),
                    support: at(35, 1, z),
                    requirement: TerminalRequirement::Repeater,
                },
            }
        };
        let sinks = NonEmptyRouteSinks::new(vec![
            make_sink(0, 7, 45),
            make_sink(1, 14, 38),
            make_sink(2, 17, 52),
            make_sink(3, 23, 59),
            make_sink(4, 26, 66),
            make_sink(5, 29, 73),
            make_sink(6, 11, 87),
            make_sink(7, 20, 80),
        ])
        .unwrap();

        let result = DurablePhysicalRouter.route_guided(
            RouteRequest {
                id: route,
                source,
                sinks: &sinks,
                reservations: &PhysicalReservations::new(),
                limits: RouterLimits {
                    max_node_expansions: 262_144,
                    max_queue_entries: 35_000,
                },
            },
            Some(RouteGuidance {
                origin: at(0, 1, 0),
                lateral: Facing::East,
                track: 30,
                half_width: 2,
                penalty_per_block: 2,
            }),
        );

        assert!(
            result.is_ok(),
            "the literal eight-sink segment tree must grow from its trunk without restarting every search at the source: {result:?}"
        );
    }

    #[test]
    fn later_fanout_branch_can_reuse_its_own_stair_clearance() {
        let route = RouteId(9);
        let from = at(0, 1, 0);
        let to = at(1, 2, 0);
        let riser = at(1, 1, 0);
        let mandatory_air = at(0, 2, 0);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            riser,
            PhysicalReservationOwner::RouteStair(route),
            PhysicalReservationKind::Floor(stone()),
        );
        reservations.reserve(
            mandatory_air,
            PhysicalReservationOwner::RouteStair(route),
            PhysicalReservationKind::MandatoryAir,
        );

        assert!(!staircase_cell_is_blocked(
            route,
            from,
            to,
            riser,
            &reservations,
            true,
        ));
        assert!(!staircase_cell_is_blocked(
            route,
            from,
            to,
            mandatory_air,
            &reservations,
            true,
        ));
    }

    #[test]
    fn strict_route_leaves_the_source_through_its_allowed_exit() {
        let route = RouteId(10);
        let source = endpoint(route);
        let sinks = NonEmptyRouteSinks::new(vec![sink(route, 0, at(0, 1, 5))]).unwrap();
        let reservations = PhysicalReservations::new();

        let tree = DurablePhysicalRouter
            .route(RouteRequest {
                id: route,
                source: source.clone(),
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 100_000,
                    max_queue_entries: 200_000,
                },
            })
            .unwrap();

        assert_eq!(tree.branches[0].path[0], source.anchor);
        assert_eq!(
            tree.branches[0].path[1],
            step(source.anchor, source.allowed_exit),
        );
    }

    #[test]
    fn strict_route_does_not_feed_a_sink_repeater_back_into_its_input() {
        let route = RouteId(12);
        let source = RouteEndpoint {
            anchor: at(5, 2, 0),
            allowed_exit: Facing::West,
            ..endpoint(route)
        };
        let connection = connection(12, 0);
        let typed_sink = RouteSink {
            id: RoutedSinkId { route, ordinal: 0 },
            endpoint: PhysicalEndpointId::Landing(connection),
            anchor: at(2, 1, 0),
            allowed_entry: Facing::West,
            terminal: TerminalContract::Sink {
                target: RouteTarget::Connection(connection),
                support: at(3, 1, 0),
                requirement: TerminalRequirement::Repeater,
            },
        };
        let sinks = NonEmptyRouteSinks::new(vec![typed_sink]).unwrap();
        let reservations = PhysicalReservations::new();

        let result = DurablePhysicalRouter.route(RouteRequest {
            id: route,
            source,
            sinks: &sinks,
            reservations: &reservations,
            limits: RouterLimits {
                max_node_expansions: 100_000,
                max_queue_entries: 200_000,
            },
        });
        assert!(
            result.is_ok(),
            "a legal alternate approach exists, but the chosen suffix closed a repeater ring: {result:?}"
        );
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
        let reservations = PhysicalReservations::new();
        let trunk_at = at(2, 1, 0);
        let mut exact = BlockState::air();
        exact.kind = BlockKind::Repeater;
        exact.name = "minecraft:repeater".to_string();
        exact.facing = Some(Facing::West);
        exact.delay = 4;
        exact.lit = true;
        let cells = BTreeMap::from([(trunk_at, exact.clone())]);

        assert_eq!(
            state_for_new_cell(route, true, &reservations, &cells, trunk_at, dust()),
            None,
            "the production insertion path must skip a non-prefix re-entry"
        );
        assert_eq!(cells.get(&trunk_at), Some(&exact));
    }
}
