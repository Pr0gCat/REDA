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

/// The lowest `x` and `z` a plan may occupy: every cell with `x < x` or
/// `z < z` belongs to the caller, and nothing the planner places or routes may
/// stand there. The one exception is a pinned terminal's own caller cell,
/// which the pin names and REDA never writes.
///
/// A corner in plan only. `y` already has a bound -- the world floor, which
/// `relax::GROUND` and `validate_port_placements` both enforce -- and a
/// caller's contract is a row and a column of its own frame, not a storey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LowerBound {
    pub x: i32,
    pub z: i32,
}

impl LowerBound {
    /// Whether `at` lies in the caller-owned space below this bound.
    pub fn excludes(self, at: Anchor) -> bool {
        at.x < self.x || at.z < self.z
    }
}

#[derive(Debug, Default, Serialize)]
pub struct PhysicalReservations {
    cells: BTreeMap<Anchor, PhysicalReservation>,
    /// The caller's claim on everything below a [`LowerBound`]: one keep-out
    /// answered for every excluded cell, so the search refuses the caller's
    /// row and column the way it refuses any foreign keep-out. An explicit
    /// entry in `cells` answers first, which is what lets a pinned terminal's
    /// own footprint keep its real owner on the caller row.
    below: Option<(LowerBound, PhysicalReservation)>,
    /// Every cell an endpoint holds as a keep-out, by endpoint.
    ///
    /// `release_endpoint_keep_outs` releases one endpoint's guard cells and
    /// nothing else, and it is called once per fanout branch. Finding those
    /// cells by walking `cells` costs the whole map every time -- a dense
    /// parent world is hundreds of thousands of entries, of which a handful
    /// belong to the endpoint being released. This is that answer kept as the
    /// map is built, so the release touches only what it releases.
    ///
    /// A derived view, never an independent source of truth: it is written
    /// only by [`insert_cell`](Self::insert_cell) and
    /// [`remove_cell`](Self::remove_cell), which every mutation goes through,
    /// and an endpoint with no cells is dropped rather than left empty so the
    /// index stays an exact function of `cells` -- which is what keeps the
    /// derived `PartialEq` honest.
    ///
    /// Skipped when serialising: it says nothing `cells` does not, and a
    /// fingerprint must not move because of a lookaside.
    #[serde(skip)]
    endpoint_keep_outs: BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>>,
    /// What one branch attempt has changed, so it can be undone without
    /// copying the map.
    ///
    /// A branch attempt writes a handful of cells and may be abandoned and
    /// retried; the map it writes into is the whole world's. Cloning that per
    /// attempt is the cost of a rollback that only ever has to put a handful
    /// of cells back. Each entry is an anchor and whatever stood there before
    /// this attempt touched it -- `None` for a cell that did not exist -- so
    /// replaying in reverse restores the exact prior state, including for a
    /// cell touched more than once.
    ///
    /// `None` when nothing is being journalled. Transient bookkeeping, not
    /// content: skipped when serialising and ignored by equality, both of
    /// which answer for `cells` and `below` alone.
    #[serde(skip)]
    journal: Option<Vec<(Anchor, Option<PhysicalReservation>)>>,
}

impl Clone for PhysicalReservations {
    /// Everything the map holds, and none of what an attempt is part-way
    /// through.
    ///
    /// A clone is a second map, not a second participant in the original's
    /// transaction: the router clones the reservations it is handed and writes
    /// into that copy, and if the copy inherited a live journal those writes
    /// would be recorded against a rollback that belongs to the caller. The
    /// cells, the caller's lower bound and the endpoint index all come across
    /// exactly.
    fn clone(&self) -> Self {
        Self {
            cells: self.cells.clone(),
            below: self.below.clone(),
            endpoint_keep_outs: self.endpoint_keep_outs.clone(),
            journal: None,
        }
    }
}

impl PartialEq for PhysicalReservations {
    /// Two maps are equal when they hold the same reservations. The endpoint
    /// index is a function of `cells`, and the journal is whatever an
    /// in-flight attempt happens to have touched -- neither is content.
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells && self.below == other.below
    }
}

impl Eq for PhysicalReservations {}

impl PhysicalReservations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim every cell below `bound` for the caller.
    pub fn claim_below(&mut self, bound: LowerBound) {
        self.below = Some((
            bound,
            PhysicalReservation {
                owner: PhysicalReservationOwner::KeepOut(u32::MAX),
                kind: PhysicalReservationKind::KeepOut,
            },
        ));
    }

    pub fn lower_bound(&self) -> Option<LowerBound> {
        self.below.as_ref().map(|(bound, _)| *bound)
    }

    /// The endpoint a reservation is indexed under, if any.
    fn index_key(reservation: &PhysicalReservation) -> Option<PhysicalEndpointId> {
        match (reservation.owner, &reservation.kind) {
            (PhysicalReservationOwner::Endpoint(endpoint), PhysicalReservationKind::KeepOut) => {
                Some(endpoint)
            }
            _ => None,
        }
    }

    /// Write one cell and keep the index exact.
    ///
    /// The only way anything in this type reaches `cells` for a write, so an
    /// insert that replaces an entry cannot leave the replaced one indexed.
    fn insert_cell(
        &mut self,
        at: Anchor,
        reservation: PhysicalReservation,
    ) -> Option<PhysicalReservation> {
        let new_key = Self::index_key(&reservation);
        let previous = self.cells.insert(at, reservation);
        if let Some(journal) = self.journal.as_mut() {
            journal.push((at, previous.clone()));
        }
        let old_key = previous.as_ref().and_then(Self::index_key);
        if old_key != new_key {
            if let Some(endpoint) = old_key {
                self.unindex(endpoint, at);
            }
            if let Some(endpoint) = new_key {
                self.endpoint_keep_outs
                    .entry(endpoint)
                    .or_default()
                    .insert(at);
            }
        }
        previous
    }

    /// Erase one cell and keep the index exact.
    fn remove_cell(&mut self, at: &Anchor) -> Option<PhysicalReservation> {
        let previous = self.cells.remove(at);
        if let Some(journal) = self.journal.as_mut() {
            journal.push((*at, previous.clone()));
        }
        if let Some(endpoint) = previous.as_ref().and_then(Self::index_key) {
            self.unindex(endpoint, *at);
        }
        previous
    }

    fn unindex(&mut self, endpoint: PhysicalEndpointId, at: Anchor) {
        if let Some(cells) = self.endpoint_keep_outs.get_mut(&endpoint) {
            cells.remove(&at);
            if cells.is_empty() {
                self.endpoint_keep_outs.remove(&endpoint);
            }
        }
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
        self.insert_cell(at, PhysicalReservation { owner, kind })
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
        self.insert_cell(at, PhysicalReservation { owner, kind });
        true
    }

    pub fn reserve_conductor(&mut self, at: Anchor, owner: RouteId, state: BlockState) {
        self.reserve(
            at,
            PhysicalReservationOwner::Route(owner),
            PhysicalReservationKind::Conductor(state),
        );
    }

    /// Record one cell of a committed route as its authoritative owner.
    ///
    /// A trunk searches against a clone of this map whose soft keep-outs were
    /// released for that trunk alone; the path it commits must then land in
    /// the master map, where those keep-outs are still standing.  Plain
    /// [`reserve`](Self::reserve) is first-writer-wins, so it silently drops
    /// the commit and leaves a later trunk blind to the conductor that is
    /// physically there.
    ///
    /// `yielding` names exactly the keep-out owners the caller released for
    /// this route, and nothing else gives way: a keep-out held by an owner
    /// outside that set -- a child halo, the caller row, another trunk's
    /// endpoint guard, an earlier route's clearance -- is a cell this route
    /// was never allowed to search through, so it is refused rather than
    /// quietly taken.
    ///
    /// An identical claim is idempotent.  Two routes may share one floor cell
    /// -- a staircase support is a plain block, and the composer already
    /// accepts an identical block already in the world -- so an equal floor
    /// keeps its first owner and succeeds.
    ///
    /// Returns `false` only for a genuine collision, which the caller reports
    /// as an overlap.
    #[must_use]
    pub fn commit_routed(
        &mut self,
        at: Anchor,
        owner: PhysicalReservationOwner,
        kind: PhysicalReservationKind,
        yielding: &[PhysicalReservationOwner],
    ) -> bool {
        match self.cells.get(&at) {
            Some(existing) if existing.owner == owner && existing.kind == kind => return true,
            Some(existing)
                if matches!(
                    (&existing.kind, &kind),
                    (
                        PhysicalReservationKind::Floor(held),
                        PhysicalReservationKind::Floor(laid),
                    ) if held == laid
                ) =>
            {
                return true
            }
            Some(existing)
                if existing.kind != PhysicalReservationKind::KeepOut
                    || !yielding.contains(&existing.owner) =>
            {
                return false
            }
            _ => {}
        }
        self.insert_cell(at, PhysicalReservation { owner, kind });
        true
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
        self.insert_cell(
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
        self.remove_cell(&at);
        true
    }

    /// Release every keep-out owned by one endpoint.
    ///
    /// A fanout request keeps later sink endpoints protected until their own
    /// branch starts.  The branch-local router has the reservation snapshot,
    /// but not the parent's precomputed guard-cell list, so it releases the
    /// endpoint's complete keep-out set here immediately before searching.
    pub(crate) fn release_endpoint_keep_outs(&mut self, endpoint: PhysicalEndpointId) {
        // The index holds exactly what the old walk over `cells` selected, in
        // the same ascending order, so each cell still goes through
        // `release_endpoint_keep_out` and its checks unchanged.
        let Some(cells) = self.endpoint_keep_outs.get(&endpoint) else {
            return;
        };
        for at in cells.iter().copied().collect::<Vec<_>>() {
            self.release_endpoint_keep_out(at, endpoint);
        }
    }

    /// Release a parent-owned soft keep-out without touching conductors,
    /// floors, or another owner's reservation.
    pub fn release_keep_out(&mut self, at: Anchor, owner: PhysicalReservationOwner) -> bool {
        let Some(existing) = self.cells.get(&at) else {
            return false;
        };
        if existing.owner != owner || existing.kind != PhysicalReservationKind::KeepOut {
            return false;
        }
        self.remove_cell(&at);
        true
    }

    /// Start journalling, discarding any journal already open.
    ///
    /// Every attempt begins here, so a journal left behind by an accepted
    /// attempt can never be replayed against a later one.
    ///
    /// Also how a caller opens a temporary view of its own map: release what
    /// one route may search through, hand the map to the router, then
    /// [`rollback_attempt`](Self::rollback_attempt) to have the releases back.
    /// Cheaper than copying the map for the view, and the router's own copy
    /// never joins the transaction -- see this type's `Clone`.
    pub(crate) fn begin_attempt(&mut self) {
        self.journal = Some(Vec::new());
    }

    /// Undo everything since [`begin_attempt`](Self::begin_attempt).
    ///
    /// Replayed in reverse through the same `insert_cell`/`remove_cell` the
    /// forward writes went through, so the endpoint index comes back exact
    /// without being reasoned about separately. The journal is taken first, so
    /// the replay records nothing of its own.
    pub(crate) fn rollback_attempt(&mut self) {
        let Some(journal) = self.journal.take() else {
            return;
        };
        for (at, previous) in journal.into_iter().rev() {
            match previous {
                Some(reservation) => {
                    self.insert_cell(at, reservation);
                }
                None => {
                    self.remove_cell(&at);
                }
            }
        }
    }

    /// Stop journalling and keep everything written.
    pub(crate) fn end_attempt(&mut self) {
        self.journal = None;
    }

    pub fn get(&self, at: &Anchor) -> Option<&PhysicalReservation> {
        self.cells.get(at).or_else(|| {
            self.below
                .as_ref()
                .filter(|(bound, _)| bound.excludes(*at))
                .map(|(_, claim)| claim)
        })
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
    pub access_half_width: u32,
    pub preferred_y: Option<i32>,
    /// Height wanted off the track -- in the access columns -- when it differs
    /// from `preferred_y`.  Lanes at one height and approaches at another let
    /// one trunk's approach pass beneath another's lane.
    pub access_y: Option<i32>,
    pub hard: bool,
    pub penalty_per_block: u64,
}

impl RouteGuidance {
    fn relative(self, at: Anchor) -> i32 {
        match self.lateral {
            Facing::North => self.origin.z.saturating_sub(at.z),
            Facing::South => at.z.saturating_sub(self.origin.z),
            Facing::East => at.x.saturating_sub(self.origin.x),
            Facing::West => self.origin.x.saturating_sub(at.x),
            Facing::Up | Facing::Down => 0,
        }
    }

    pub(crate) fn allows(self, at: Anchor, source: Anchor, sink: Anchor) -> bool {
        if !self.hard
            || u64::from(self.relative(at).abs_diff(self.track)) <= u64::from(self.half_width)
        {
            return true;
        }
        match self.lateral {
            Facing::North | Facing::South => {
                at.x.abs_diff(source.x) <= self.access_half_width
                    || at.x.abs_diff(sink.x) <= self.access_half_width
            }
            Facing::East | Facing::West => {
                at.z.abs_diff(source.z) <= self.access_half_width
                    || at.z.abs_diff(sink.z) <= self.access_half_width
            }
            Facing::Up | Facing::Down => true,
        }
    }

    /// Inclusive absolute span of the admitted track along `lateral`, on the
    /// axis `lateral` runs on -- `z` for north/south, `x` for east/west.
    ///
    /// `None` when the guidance names no plane: a vertical `lateral` makes
    /// [`Self::relative`] zero and [`Self::allows`] unconditional, so there is
    /// nothing for a caller to include.
    fn track_span(self) -> Option<(i32, i32)> {
        let centre = match self.lateral {
            Facing::North => self.origin.z.saturating_sub(self.track),
            Facing::South => self.origin.z.saturating_add(self.track),
            Facing::East => self.origin.x.saturating_add(self.track),
            Facing::West => self.origin.x.saturating_sub(self.track),
            Facing::Up | Facing::Down => return None,
        };
        let half = i32::try_from(self.half_width).unwrap_or(i32::MAX);
        Some((centre.saturating_sub(half), centre.saturating_add(half)))
    }

    fn penalty(self, at: Anchor) -> u64 {
        let relative = self.relative(at);
        let outside_track = u64::from(relative.abs_diff(self.track)) > u64::from(self.half_width);
        let wanted_y = if outside_track {
            self.access_y.or(self.preferred_y)
        } else {
            self.preferred_y
        };
        let outside_height = wanted_y.is_some_and(|y| at.y != y);
        u64::from(outside_track as u8)
            .saturating_add(u64::from(outside_height as u8))
            .saturating_mul(self.penalty_per_block)
    }
}

/// **How much of a terminal's own straight runway a route must traverse.**
///
/// A packed child hands its parent three cells per boundary: the anchor and a
/// two-cell runway straight out along the terminal's own direction. Everything
/// beside that column -- the lateral guard and the `+/-Y` coupling ring -- stays
/// keep-out, because a foreign conductor there couples into the child. A search
/// that turns at the anchor therefore has nowhere legal to turn *into*, and the
/// first thing it can report is that it found no route at all.
///
/// This says the shape instead of leaving the search to discover it: a source
/// leaves through `cells` straight steps before it is free to turn, and a sink
/// is entered through the mirror-image suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TerminalRunway {
    /// No forced straight run. Seed rules still fix a source's *first* step to
    /// its `allowed_exit`, exactly as they always have, and a sink's approach
    /// cell is still its search goal; nothing else is constrained.
    ///
    /// This is what every caller that does not ask for a runway gets, so their
    /// routes are the routes they were before this contract existed.
    #[default]
    Free,
    /// The route traverses `cells` straight steps out of the source anchor
    /// along `allowed_exit` before it may turn, and reaches the sink anchor
    /// through the same `cells` straight steps taken in reverse along
    /// `allowed_entry`.
    ///
    /// `cells` counts the runway only -- the anchor itself is not one of them
    /// -- so a three-cell access column is `Forced { cells: 2 }`. Entering the
    /// far end of a runway is unconstrained: that end is where a source is
    /// free to turn, and mirrored, where a sink's approach may be joined.
    /// `cells: 0` is [`Free`](Self::Free) written the long way.
    Forced { cells: u32 },
}

impl TerminalRunway {
    fn cells(self) -> u32 {
        match self {
            Self::Free => 0,
            Self::Forced { cells } => cells,
        }
    }

    /// The runway as cells, starting at `anchor` and stepping along `along`.
    ///
    /// `chain[0]` is the anchor, `chain[1..]` the forced runway. Saturating
    /// steps cannot fold the chain back on itself for any runway a terminal
    /// can ask for, and a chain that did would only ever forbid more.
    fn chain(self, anchor: Anchor, along: Facing) -> Vec<Anchor> {
        let mut chain = Vec::with_capacity(self.cells() as usize + 1);
        chain.push(anchor);
        for _ in 0..self.cells() {
            chain.push(step(
                *chain.last().expect("the chain starts at its anchor"),
                along,
            ));
        }
        chain
    }
}

/// A forced runway at the source and one applied to every sink of a request.
///
/// One value for all sinks rather than one per sink: the geometry it states is
/// the child boundary contract, which is the same three-cell column at every
/// terminal a packed leaf exposes. A caller that needs them to differ has a
/// different contract, not a longer list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ForcedTerminalRunways {
    pub source: TerminalRunway,
    pub sinks: TerminalRunway,
}

/// One end's forced chain, resolved against the cells it applies to.
#[derive(Debug, Clone, Default)]
struct ForcedChain {
    cells: Vec<Anchor>,
}

impl ForcedChain {
    fn new(runway: TerminalRunway, anchor: Anchor, along: Facing) -> Self {
        Self {
            cells: match runway {
                TerminalRunway::Free => Vec::new(),
                TerminalRunway::Forced { .. } => runway.chain(anchor, along),
            },
        }
    }

    /// Leaving `at`: the one cell a route standing on the forced prefix may
    /// step to, or `None` where the prefix is over and the route may turn.
    fn required_step_from(&self, at: Anchor) -> Option<Anchor> {
        let index = self.cells.iter().position(|cell| *cell == at)?;
        self.cells.get(index + 1).copied()
    }

    /// Entering `at`: the one cell a route may come from to reach a forced
    /// suffix cell, or `None` where `at` is the far end or off the chain.
    fn required_predecessor_of(&self, at: Anchor) -> Option<Anchor> {
        let index = self.cells.iter().position(|cell| *cell == at)?;
        (index + 1 < self.cells.len()).then(|| self.cells[index + 1])
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

    /// Route under an explicit [`ForcedTerminalRunways`] contract.
    ///
    /// The default forwards to [`route`](Self::route) and drops the contract,
    /// which is exactly the prior semantics -- the same seam and the same
    /// default [`route_guided`](Self::route_guided) has. A router that means to
    /// honour a runway overrides this; [`DurablePhysicalRouter`] does, and
    /// [`FragmentRouterAdapter`] passes it through untouched.
    fn route_with_runways(
        &self,
        request: RouteRequest<'_>,
        runways: ForcedTerminalRunways,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        let _ = runways;
        self.route(request)
    }

    /// Route under a guidance **and** a runway contract at once.
    ///
    /// The two existing seams are exclusive -- [`route_guided`](Self::route_guided)
    /// passes no runway and [`route_with_runways`](Self::route_with_runways)
    /// passes no guidance -- so a caller that needs both has until now had to
    /// give one up. A packed parent needs both: the runway is what gets a
    /// route out of a certified child, and the guidance is what keeps two
    /// routes off each other's lane once they are out.
    ///
    /// The default drops the guidance and forwards to
    /// [`route_with_runways`](Self::route_with_runways), which is the stricter
    /// of the two contracts to lose: a router that ignores a lane still builds
    /// a correct route, one that ignores a runway does not.
    fn route_guided_with_runways(
        &self,
        request: RouteRequest<'_>,
        guidance: Option<RouteGuidance>,
        runways: ForcedTerminalRunways,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        let _ = guidance;
        self.route_with_runways(request, runways)
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

    fn route_with_runways(
        &self,
        request: RouteRequest<'_>,
        runways: ForcedTerminalRunways,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        self.router.route_with_runways(request, runways)
    }

    fn route_guided_with_runways(
        &self,
        request: RouteRequest<'_>,
        guidance: Option<RouteGuidance>,
        runways: ForcedTerminalRunways,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        self.router
            .route_guided_with_runways(request, guidance, runways)
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
        route_with_local_policy(
            request,
            RoutingJoinPolicy::Off,
            true,
            true,
            move |at| guidance.map_or(0, |guidance| guidance.penalty(*at)),
            |_, _, _| {},
            guidance,
            ForcedTerminalRunways::default(),
        )
    }

    fn route_with_runways(
        &self,
        request: RouteRequest<'_>,
        runways: ForcedTerminalRunways,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        route_with_local_policy(
            request,
            RoutingJoinPolicy::Off,
            true,
            true,
            |_| 0,
            |_, _, _| {},
            None,
            runways,
        )
    }

    fn route_guided_with_runways(
        &self,
        request: RouteRequest<'_>,
        guidance: Option<RouteGuidance>,
        runways: ForcedTerminalRunways,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        route_with_local_policy(
            request,
            RoutingJoinPolicy::Off,
            true,
            true,
            move |at| guidance.map_or(0, |guidance| guidance.penalty(*at)),
            |_, _, _| {},
            guidance,
            runways,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutingJoinPolicy {
    Off,
    Narrow,
    Wide,
}

const MAX_SEED_LOCAL_REROUTES_PER_BRANCH: usize = 16;
const MAX_STRICT_RING_REROUTES_PER_BRANCH: usize = 1;

fn ring_reroute_limit(strict_local: bool, seed_rules: bool) -> usize {
    if seed_rules {
        MAX_SEED_LOCAL_REROUTES_PER_BRANCH
    } else if strict_local {
        MAX_STRICT_RING_REROUTES_PER_BRANCH
    } else {
        0
    }
}

fn first_eligible_ring_reroute(
    charged: &[Anchor],
    start: Anchor,
    sink: Anchor,
    ring_forbidden: &BTreeSet<Anchor>,
) -> Option<Anchor> {
    charged
        .iter()
        .copied()
        .find(|cell| *cell != start && *cell != sink && !ring_forbidden.contains(cell))
}

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

/// Returns the direction a shared dust cell may be promoted in without
/// changing the typed path seen by any branch that already uses it.
fn shared_trunk_refresh_direction(
    request: &RouteRequest<'_>,
    candidate: Anchor,
    current_path: &[Anchor],
    branches: &[RealisedRouteBranch],
    states: &BTreeMap<Anchor, BlockState>,
    reservations: &PhysicalReservations,
) -> Option<Facing> {
    if candidate == request.source.anchor
        || request
            .sinks
            .as_slice()
            .iter()
            .any(|sink| sink.anchor == candidate)
        || states.get(&candidate)?.kind != BlockKind::RedstoneWire
    {
        return None;
    }

    let mut endpoints = None;
    for path in
        std::iter::once(current_path).chain(branches.iter().map(|branch| branch.path.as_slice()))
    {
        let mut seen = false;
        for (index, &at) in path.iter().enumerate().filter(|(_, at)| **at == candidate) {
            if std::mem::replace(&mut seen, true) {
                return None;
            }
            let (Some(&previous), Some(&next)) = (
                index.checked_sub(1).and_then(|i| path.get(i)),
                path.get(index + 1),
            ) else {
                return None;
            };
            let direction = horizontal_direction(previous, at)?;
            if previous.y != at.y
                || at.y != next.y
                || horizontal_direction(at, next) != Some(direction)
                || !route_step_is_legal(previous, at, next, &repeater_toward(direction))
            {
                return None;
            }
            match endpoints {
                Some((old_previous, old_next)) if (previous, next) != (old_previous, old_next) => {
                    return None
                }
                None => endpoints = Some((previous, next)),
                _ => {}
            }
        }
    }
    let (previous, next) = endpoints?;
    let sealed = |lid: Anchor| {
        !states.contains_key(&lid) && reservations.get(&lid).is_some_and(reservation_is_floor)
    };
    let neighbours = dust_join_neighbours_typed(candidate, &sealed)
        .into_iter()
        .filter(|at| {
            states.get(at).is_some_and(|state| {
                state.kind == BlockKind::RedstoneWire
                    || (state.kind == BlockKind::Repeater
                        && state.facing.is_some_and(|facing| {
                            step(*at, facing) == candidate
                                || step(*at, facing.opposite()) == candidate
                        }))
            })
        })
        .collect::<BTreeSet<_>>();
    (neighbours.iter().all(|at| *at == previous || *at == next)
        && [previous, next]
            .into_iter()
            .filter(|at| states.contains_key(at))
            .all(|at| neighbours.contains(&at)))
    .then_some(horizontal_direction(previous, candidate)?)
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

/// Private policy seam for legacy extraction congestion costs and transactional
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
    route_with_local_policy(
        request,
        join_policy,
        false,
        false,
        price,
        claim,
        None,
        ForcedTerminalRunways::default(),
    )
}

/// Strict local route certification for newly generated fragment candidates.
/// Strict routes and seed routes share the geometry-aware reserve policy;
/// legacy extraction stays on `route_with_policy` for its legacy policy.
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
    route_with_local_policy(
        request,
        join_policy,
        true,
        false,
        price,
        claim,
        None,
        ForcedTerminalRunways::default(),
    )
}

/// The seed's own route policy: strict local certification plus the
/// seed-only rules (fanout tree roots, ring/floor reroutes, source-exit and
/// terminal-feedback keep-outs). Legacy `lay_net`/`try_move` stay on
/// `route_with_policy`, so these seed-only rules do not affect them.
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
    route_with_local_policy(
        request,
        join_policy,
        true,
        true,
        price,
        claim,
        None,
        ForcedTerminalRunways::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn route_with_local_policy<Price, Claim>(
    request: RouteRequest<'_>,
    join_policy: RoutingJoinPolicy,
    strict_local: bool,
    seed_rules: bool,
    mut price: Price,
    mut claim: Claim,
    guidance: Option<RouteGuidance>,
    runways: ForcedTerminalRunways,
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
        guidance,
        runways,
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
    guidance: Option<RouteGuidance>,
    runways: ForcedTerminalRunways,
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
    // The source's forced prefix is one chain for the whole tree: every fanout
    // branch leaves the source through the same runway, including a branch
    // that re-roots on a cell of it.
    let source_runway = ForcedChain::new(
        runways.source,
        request.source.anchor,
        request.source.allowed_exit,
    );
    let mut reservations = request.reservations.clone();
    let mut cell_states = BTreeMap::<Anchor, BlockState>::new();
    let mut cell_order = Vec::<Anchor>::new();
    let mut floor_states = BTreeMap::<Anchor, BlockState>::new();
    let mut floor_order = Vec::<Anchor>::new();
    let mut branches = Vec::<RealisedRouteBranch>::with_capacity(request.sinks.as_slice().len());
    let mut tree_parent = BTreeMap::<Anchor, Anchor>::new();
    let mut staged_claims =
        Vec::<(Anchor, PhysicalReservationOwner, PhysicalReservationKind)>::new();
    let stage_claims = strict_local || seed_rules;

    let ring_reroute_limit = ring_reroute_limit(strict_local, seed_rules);
    let reserve_policy = reserve_policy(strict_local, seed_rules);
    for sink in request.sinks.as_slice() {
        let mut ring_forbidden = BTreeSet::new();
        let mut local_reroutes = 0usize;
        let mut ring_reroutes = 0usize;
        'branch_attempt: loop {
            reservations.begin_attempt();
            let cell_states_before = cell_states.clone();
            let cell_order_len = cell_order.len();
            let floor_states_before = floor_states.clone();
            let floor_order_len = floor_order.len();
            let branches_len = branches.len();
            let staged_claims_len = staged_claims.len();
            reservations.release_endpoint_keep_outs(sink.endpoint);
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
                guidance,
                &source_runway,
                &ForcedChain::new(runways.sinks, sink.anchor, sink.allowed_entry),
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
                    // Reroutes roll a branch back, so claims are staged until
                    // the tree is accepted.  The legacy adapter has no
                    // reroutes and keeps its immediate claim behaviour.
                    if stage_claims {
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
            let shared_strength = |states: &BTreeMap<Anchor, BlockState>| {
                let mut carried = source_strength;
                let mut previous_cell = request.source.anchor;
                let mut trunk_repeaters = 0u64;
                for anchor in &path[..shared] {
                    let state = states
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
                (previous_cell, carried, trunk_repeaters)
            };
            let (previous_cell, mut carried, mut trunk_repeaters) = shared_strength(&cell_states);
            // The branch ends on the sink's own cell, so a refresh the budget
            // demands there is only real if `select_terminal_kind` will keep
            // it. A contract that cannot host a repeater has the refresh
            // walked back onto the branch instead -- or, when nothing before
            // the terminal may hold one, is refused rather than laid with a
            // terminal that reads zero while the count says it was refreshed.
            let terminal_hosts_refresh = sink
                .terminal
                .sink_parts()
                .is_some_and(|(_, _, requirement)| terminal_hosts_demanded_refresh(requirement));
            let mut laid = realise_branch_cells(
                previous_cell,
                carried,
                &path[shared..],
                strict_local,
                reserve_policy,
                terminal_hosts_refresh,
            );
            let mut promoted = None;
            if strict_local && !laid.carries {
                for &candidate in path[..shared].iter().rev() {
                    let Some(direction) = shared_trunk_refresh_direction(
                        &request,
                        candidate,
                        &path,
                        &branches,
                        &cell_states,
                        &reservations,
                    ) else {
                        continue;
                    };
                    let original = cell_states
                        .insert(candidate, repeater_toward(direction))
                        .expect("a shared refresh candidate has an exact laid state");
                    let (refreshed_previous, refreshed_carried, refreshed_repeaters) =
                        shared_strength(&cell_states);
                    let refreshed = realise_branch_cells(
                        refreshed_previous,
                        refreshed_carried,
                        &path[shared..],
                        strict_local,
                        reserve_policy,
                        terminal_hosts_refresh,
                    );
                    let certified = refreshed.carries
                        && branches
                            .iter()
                            .filter(|branch| branch.path.contains(&candidate))
                            .all(|branch| {
                                request
                                    .sinks
                                    .as_slice()
                                    .iter()
                                    .find(|accepted| accepted.id == branch.sink)
                                    .is_some_and(|accepted| {
                                        certify_path(&request, accepted, &branch.path, &cell_states)
                                            .is_ok()
                                    })
                            });
                    if certified {
                        carried = refreshed_carried;
                        trunk_repeaters = refreshed_repeaters;
                        laid = refreshed;
                        promoted = Some(candidate);
                        break;
                    }
                    cell_states.insert(candidate, original);
                }
            }
            if !laid.carries {
                let reroute_cell = shared
                    .checked_sub(1)
                    .and_then(|index| path.get(index).copied())
                    .filter(|cell| *cell != start && *cell != sink.anchor);
                if seed_rules && local_reroutes < MAX_SEED_LOCAL_REROUTES_PER_BRANCH {
                    if let Some(reroute_cell) = reroute_cell {
                        if ring_forbidden.insert(reroute_cell) {
                            reservations.rollback_attempt();
                            cell_states = cell_states_before;
                            cell_order.truncate(cell_order_len);
                            floor_states = floor_states_before;
                            floor_order.truncate(floor_order_len);
                            branches.truncate(branches_len);
                            staged_claims.truncate(staged_claims_len);
                            local_reroutes += 1;
                            continue 'branch_attempt;
                        }
                    }
                }
                if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                    eprintln!(
                        "strict physical refusal: branch does not carry; route={:?} sink={:?} shared={shared} path_len={} incoming_strength={carried} local_reroutes={local_reroutes}",
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
                if seed_rules
                    && local_reroutes < MAX_SEED_LOCAL_REROUTES_PER_BRANCH
                    && overlap != start
                    && overlap != sink.anchor
                    && ring_forbidden.insert(overlap)
                {
                    reservations.rollback_attempt();
                    cell_states = cell_states_before;
                    cell_order.truncate(cell_order_len);
                    floor_states = floor_states_before;
                    floor_order.truncate(floor_order_len);
                    branches.truncate(branches_len);
                    staged_claims.truncate(staged_claims_len);
                    local_reroutes += 1;
                    continue 'branch_attempt;
                }
                if std::env::var_os("REDA_TRACE_SEED_REPAIRS").is_some() {
                    eprintln!(
                        "strict physical refusal: branch floor overlap; route={:?} sink={:?} at={overlap:?} shared={shared} path_len={} local_reroutes={local_reroutes}",
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
                    // Reroutes roll a branch back, so claims are staged until
                    // the tree is accepted.  The legacy adapter has no
                    // reroutes and keeps its immediate claim behaviour.
                    if stage_claims {
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
            if let Some((repeater, ring_reachable)) =
                ring_closed_in_typed(&cell_states, &reservations)
            {
                let branch = branches
                    .last()
                    .expect("the checked branch was just recorded");
                let suffix = &branch.path[shared..];
                // This traversal region exposes the closure; it is not an exact cycle witness.
                let mut charged: Vec<_> = suffix
                    .iter()
                    .copied()
                    .filter(|cell| ring_reachable.contains(cell))
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
                    RouterFailure::RingClosure { charged, .. } => {
                        first_eligible_ring_reroute(charged, start, sink.anchor, &ring_forbidden)
                    }
                    _ => unreachable!(),
                };
                if ring_reroutes < ring_reroute_limit {
                    if let Some(reroute_cell) = reroute_cell {
                        reservations.rollback_attempt();
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
            if let Some(promoted) = promoted {
                for branch in branches[..branches_len]
                    .iter_mut()
                    .filter(|branch| branch.path.contains(&promoted))
                {
                    branch.terminal.repeaters += 1;
                }
            }
            for edge in branches
                .last()
                .expect("the accepted branch was just recorded")
                .path
                .windows(2)
            {
                tree_parent.entry(edge[1]).or_insert(edge[0]);
            }
            reservations.end_attempt();
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

/// Which of `candidates` the path behind `at` obstructs, deciding all of them
/// in one walk of that path.
///
/// [`self_obstructs_typed`] answers for a single candidate and walks the chain
/// from `at` back to the source to do it; asking it once per neighbour walks
/// the same chain twelve times per expansion, which is where a long route
/// spends nearly all of its search.
///
/// The four conditions it tests are each "this chain cell sits in the
/// candidate's own `x`/`z` column, at one particular height", so a chain cell
/// can be matched against every candidate at once:
///
/// - the drop blocker, at `at.y + 1`, for a candidate below `at`;
/// - the floor that would crush the candidate, one above it, seed rules only;
/// - the cell crushed beneath the candidate, one below it;
/// - the smothering cell two below it, and only when the chain cell's own
///   successor -- the cell after it on the way to `at` -- stands higher.
///
/// The scalar version returns on its first hit; this one keeps walking,
/// because it is answering for candidates it may not have hit yet. That
/// changes nothing: obstruction is a disjunction over chain cells, and the
/// successor each cell sees is fixed by the chain, not by how far the walk
/// got. `self_obstructs_typed` remains the reference, and
/// `the_batch_matches_twelve_scalar_calls` holds the two to it.
fn self_obstructs_batch(
    previous: &BTreeMap<Anchor, Anchor>,
    at: Anchor,
    candidates: &[Anchor],
    seed_rules: bool,
    obstructed: &mut [bool],
) {
    debug_assert_eq!(candidates.len(), obstructed.len());
    obstructed.fill(false);
    let mut successor: Option<Anchor> = None;
    let mut walk = Some(at);
    while let Some(cell) = walk {
        let rises_after = successor.is_some_and(|after: Anchor| after.y > cell.y);
        for (candidate, obstructed) in candidates.iter().zip(obstructed.iter_mut()) {
            if *obstructed || cell.x != candidate.x || cell.z != candidate.z {
                continue;
            }
            if (candidate.y < at.y && cell.y == at.y + 1)
                || (seed_rules && cell.y == candidate.y + 1)
                || cell.y == candidate.y - 1
                || (cell.y == candidate.y - 2 && rises_after)
            {
                *obstructed = true;
            }
        }
        successor = Some(cell);
        walk = previous.get(&cell).copied();
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

pub(crate) fn keep_out_typed(anchor: Anchor) -> Vec<Anchor> {
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
        // A bare merge lands on another route's dust one cell past the
        // terminal, so the terminal must still carry at least 2 for the
        // junction to read 1.  The legacy planner picks BareMergeRepeater
        // itself from its landing strength; a caller that cannot know the
        // path length in advance asks for dust and gets the refresh only when
        // the budget says the join would otherwise read zero.
        TerminalRequirement::Exact(RouteTerminalKind::BareMergeDust)
            if budget_needs_repeater || predecessor_strength < 3 =>
        {
            RouteTerminalKind::BareMergeRepeater
        }
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
    guidance: Option<RouteGuidance>,
    source_runway: &ForcedChain,
    sink_runway: &ForcedChain,
) -> Result<Option<Vec<Anchor>>, RouterFailure>
where
    Price: FnMut(&Anchor) -> u64,
{
    let margin = manhattan(start, goal).saturating_add(2) as i32;
    let mut min = Anchor {
        x: start.x.min(goal.x).saturating_sub(margin),
        y: start.y.min(goal.y),
        z: start.z.min(goal.z).saturating_sub(margin),
    };
    let mut max = Anchor {
        x: start.x.max(goal.x).saturating_add(margin),
        y: start
            .y
            .max(goal.y)
            .saturating_add(if seed_rules { 6 } else { 3 }),
        z: start.z.max(goal.z).saturating_add(margin),
    };

    // A hard guidance names a plane the branch's own endpoints need not
    // bracket. The parent owns the corridor and hands each trunk a lane; a
    // trunk whose source and sink sit a few cells apart on the portal row can
    // be handed a lane far deeper than the straight-line margin reaches, and
    // then every cell this box admits is one the guidance forbids, or the
    // reverse. The search reports no route where the corridor plainly has one.
    //
    // Measured in `seven_segment` composed through recursive contracts: trunk
    // `g32` runs (623,1,117) -> (651,1,116), margin 31, so the box floors at
    // z = 85 -- and its lane is z = 72. The two access columns dead-end at the
    // box wall thirteen cells short of the only row that crosses between them.
    //
    // The box is a bound that keeps A* finite, not a policy: it has to contain
    // what the guidance admits. Only `hard` guidance is included, because a
    // soft one states a preference the search may decline, and widening for it
    // would move routes nobody asked to move.
    if let Some(guidance) = guidance.filter(|guidance| guidance.hard) {
        if let Some((lo, hi)) = guidance.track_span() {
            match guidance.lateral {
                Facing::North | Facing::South => {
                    min.z = min.z.min(lo);
                    max.z = max.z.max(hi);
                }
                Facing::East | Facing::West => {
                    min.x = min.x.min(lo);
                    max.x = max.x.max(hi);
                }
                Facing::Up | Facing::Down => {}
            }
        }
    }

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
        // One walk of the path behind `state.at` for all twelve neighbours,
        // read back below in the order `neighbours` produced them.
        let candidates = neighbours(state.at);
        let mut obstructed = vec![false; candidates.len()];
        self_obstructs_batch(
            &previous,
            state.at,
            &candidates,
            seed_rules,
            &mut obstructed,
        );
        for (candidate, self_obstructs) in candidates.into_iter().zip(obstructed) {
            let next = candidate;
            if guidance.is_some_and(|guidance| !guidance.allows(next, start, goal)) {
                continue;
            }
            if forbidden.contains(&next) && next != start && next != goal {
                continue;
            }
            if seed_rules && state.at == start && next != required_source_exit {
                continue;
            }
            // The forced runways. A route standing on the source's prefix has
            // exactly one admissible step, and a forced suffix cell has
            // exactly one admissible predecessor -- the cell behind it, which
            // is the same statement read from the other end. Both are
            // positional, so a fanout branch that re-roots on a runway cell
            // leaves it the same way the first branch did.
            if source_runway
                .required_step_from(state.at)
                .is_some_and(|required| next != required)
            {
                continue;
            }
            if sink_runway
                .required_predecessor_of(next)
                .is_some_and(|required| state.at != required)
            {
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
            if self_obstructs {
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
            // Legacy extraction charges every vertical edge the staircase
            // premium; strict/seed routes retain the toward-goal discount.
            let step_cost = search_step_cost(strict_local, seed_rules, state.at.y, next.y, goal.y);
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

fn search_step_cost(
    strict_local: bool,
    seed_rules: bool,
    from_y: i32,
    to_y: i32,
    goal_y: i32,
) -> u64 {
    if from_y == to_y {
        return 1;
    }
    if !strict_local && !seed_rules {
        return 3;
    }
    ((to_y - goal_y).abs() < (from_y - goal_y).abs())
        .then_some(1)
        .unwrap_or(3)
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

/// Whether a sink contract may end in a repeater at all: the set the
/// terminal feedback keep-out protects.
fn terminal_may_be_repeater(requirement: TerminalRequirement) -> bool {
    match requirement {
        TerminalRequirement::Automatic | TerminalRequirement::Repeater => true,
        TerminalRequirement::DirectedDust => false,
        TerminalRequirement::Exact(kind) => matches!(
            kind,
            RouteTerminalKind::RepeaterIntoSupport
                | RouteTerminalKind::BareMergeRepeater
                | RouteTerminalKind::OutputTerminalRepeater
        ),
    }
}

/// Whether a refresh the dust budget demands on the terminal cell survives
/// `select_terminal_kind`. That is [`terminal_may_be_repeater`] plus the
/// bare-merge dust contract, which asks for dust but is upgraded to
/// `BareMergeRepeater` exactly when the budget demands it. Every other
/// dust contract overwrites the demanded repeater with dust, so the branch
/// planner must not count on the terminal cell for its last refresh.
fn terminal_hosts_demanded_refresh(requirement: TerminalRequirement) -> bool {
    terminal_may_be_repeater(requirement)
        || matches!(
            requirement,
            TerminalRequirement::Exact(RouteTerminalKind::BareMergeDust)
        )
}

fn terminal_feedback_keep_out(sink: &RouteSink) -> BTreeSet<Anchor> {
    let Some((_, support, requirement)) = sink.terminal.sink_parts() else {
        return BTreeSet::new();
    };
    if !terminal_may_be_repeater(requirement) {
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
    realise_branch_from_with_boundary_policy(
        previous_cell,
        incoming,
        cells,
        false,
        ReservePolicy::TotalStairs,
    )
}

/// How much of the dust budget a branch holds back so that a forced refresh
/// always has somewhere to stand.
///
/// `plan_bent_path` places a repeater when the budget runs out, then walks
/// *back* from that index to the nearest cell that may host one, skipping
/// every bend and staircase step. The reserve is what guarantees that walk
/// still lands inside the run rather than past its start -- so what it has to
/// cover is the longest stretch it might have to walk over, not how many such
/// cells the branch contains in total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReservePolicy {
    /// Every staircase step on the branch, capped at the dust run.
    ///
    /// What the legacy planner has always used, and what its pinned layouts
    /// are measured against. Correct but blunt: a branch with many *isolated*
    /// stairs saturates the cap, the threshold collapses to two cells, and it
    /// is refreshed roughly every third cell whether or not anything about its
    /// geometry required it.
    TotalStairs,
    /// Nothing held back: each refresh is demanded only where the wire would
    /// otherwise die, and stands on the latest cell before that point where
    /// a straight, flat repeater is legal.
    ///
    /// A reserve is a *global* concession -- it shortens every cycle on the
    /// branch to pay for the one stretch of ineligible cells the walk-back
    /// may have to cross. But `plan_bent_path`'s walk-back already crosses
    /// any such stretch on its own, bounded only by the previous refresh: a
    /// reserve never makes a placement feasible that a zero reserve refuses,
    /// it only moves every refresh earlier. So the cycle is left at the full
    /// `MAX_DUST_RUN`, the walk-back does the geometry-specific work, and the
    /// only stretch that cannot be crossed -- ineligible cells from the
    /// previous refresh all the way to exhaustion -- is reported as a branch
    /// that does not carry (`LaidBranch::carries`), never bridged with a
    /// repeater the certifier would reject.
    ///
    /// Eligibility here is `route_step_is_legal`'s own geometry for a
    /// repeater: entered flat and straight, and left the same way. On a
    /// strict branch that is the same set of cells the legacy reserve counts
    /// (turns, staircase steps, the cell before a climb, a boundary turn) --
    /// derived from the rule certification applies rather than restated.
    LatestLegalCell,
}

fn reserve_policy(strict_local: bool, seed_rules: bool) -> ReservePolicy {
    if strict_local || seed_rules {
        ReservePolicy::LatestLegalCell
    } else {
        ReservePolicy::TotalStairs
    }
}

/// Whether a repeater at `cells[index]` satisfies `route_step_is_legal`'s
/// geometry: entered flat and straight from its predecessor, and left the
/// same way. The last cell's exit is the terminal's business, so only its
/// entry is checked here -- the same rule the staircase set applies there.
/// Whether the terminal contract can keep a refresh on that cell at all is
/// the caller's to enforce (`realise_branch_cells` marks the last cell
/// ineligible when `terminal_hosts_refresh` is false).
fn straight_flat_repeater_fits(source: Anchor, cells: &[Anchor], index: usize) -> bool {
    let previous = if index == 0 { source } else { cells[index - 1] };
    let Some(entered) = horizontal_direction(previous, cells[index]) else {
        return false;
    };
    match cells.get(index + 1) {
        Some(&next) => horizontal_direction(cells[index], next) == Some(entered),
        None => true,
    }
}

/// [`realise_branch_cells`] with a terminal that may host the demanded
/// refresh: what every caller that is not laying a real sink contract
/// wants, and the legacy reserve's only shape.
#[cfg(test)]
fn realise_branch_from_with_boundary_policy(
    previous_cell: Anchor,
    incoming: u8,
    cells: &[Anchor],
    include_boundary_bend: bool,
    reserve_policy: ReservePolicy,
) -> LaidBranch {
    realise_branch_cells(
        previous_cell,
        incoming,
        cells,
        include_boundary_bend,
        reserve_policy,
        true,
    )
}

/// Lay `cells` from `previous_cell` carrying `incoming`. The last cell is
/// the sink terminal; `terminal_hosts_refresh` says whether a repeater the
/// budget demands there is kept by the terminal contract
/// (`terminal_hosts_demanded_refresh`). Under `LatestLegalCell` a terminal
/// that cannot keep it is simply ineligible, so `plan_bent_path` walks the
/// demand back onto the branch or reports the branch as not carrying; the
/// legacy reserve ignores the flag and keeps its pinned layouts.
fn realise_branch_cells(
    previous_cell: Anchor,
    incoming: u8,
    cells: &[Anchor],
    include_boundary_bend: bool,
    reserve_policy: ReservePolicy,
    terminal_hosts_refresh: bool,
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
    let (ineligible, reserve) = match reserve_policy {
        ReservePolicy::TotalStairs => {
            let stairs = bends
                .iter()
                .filter(|&&index| {
                    let before = if index == 0 { source } else { cells[index - 1] };
                    cells[index].y != before.y
                })
                .count();
            let reserve = (stairs as i32).min(crate::compile::MAX_DUST_RUN - 2);
            (bends, reserve)
        }
        // `plan_bent_path` with no reserve is already the demand-driven walk:
        // it triggers on the first cell whose dust would read zero and walks
        // back to the latest cell outside `ineligible`, stopping at the
        // previous refresh. What it needs from here is the exact legality
        // set, not a shortened cycle.
        ReservePolicy::LatestLegalCell => {
            let mut ineligible = (0..cells.len())
                .filter(|&index| !straight_flat_repeater_fits(source, cells, index))
                .collect::<BTreeSet<usize>>();
            if !terminal_hosts_refresh {
                ineligible.extend(cells.len().checked_sub(1));
            }
            (ineligible, 0)
        }
    };
    let (is_repeater, _) =
        crate::compile::plan_bent_path(cells.len(), &ineligible, incoming, reserve);
    let mut is_repeater = is_repeater;
    let mut previous = source;
    for (index, cell) in cells.iter().enumerate() {
        if cell.y != previous.y && index > 0 {
            let before = index - 1;
            // The cell before a step exits off the level, so under `LatestLegalCell`
            // it is always in `ineligible` and this never fires; the legacy
            // reserve keeps the forced pre-stair refresh it always had.
            if !ineligible.contains(&before) {
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
    // The only honest carry check: every dust cell of the tail read from the
    // exact blocks above. A stretch `plan_bent_path` could not refresh --
    // ineligible from the previous refresh to exhaustion -- shows up here as
    // a zero, and the branch is reported as not carrying rather than bridged.
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

    /// What `release_endpoint_keep_outs` used to compute by walking the whole
    /// map: every cell this endpoint holds as a keep-out, ascending.
    fn endpoint_keep_outs_by_scan(
        reservations: &PhysicalReservations,
        endpoint: PhysicalEndpointId,
    ) -> Vec<Anchor> {
        reservations
            .cells
            .iter()
            .filter_map(|(at, reservation)| {
                (reservation.owner == PhysicalReservationOwner::Endpoint(endpoint)
                    && reservation.kind == PhysicalReservationKind::KeepOut)
                    .then_some(*at)
            })
            .collect()
    }

    /// The index rebuilt from `cells` alone -- what it must always equal.
    fn index_by_scan(
        reservations: &PhysicalReservations,
    ) -> BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>> {
        let mut index: BTreeMap<PhysicalEndpointId, BTreeSet<Anchor>> = BTreeMap::new();
        for (at, reservation) in &reservations.cells {
            if let (
                PhysicalReservationOwner::Endpoint(endpoint),
                PhysicalReservationKind::KeepOut,
            ) = (reservation.owner, &reservation.kind)
            {
                index.entry(endpoint).or_default().insert(*at);
            }
        }
        index
    }

    fn assert_index_exact(reservations: &PhysicalReservations, note: &str) {
        assert_eq!(
            reservations.endpoint_keep_outs,
            index_by_scan(reservations),
            "the index drifted from the cells it derives from: {note}"
        );
    }

    fn endpoint_id(port: u32) -> PhysicalEndpointId {
        PhysicalEndpointId::PrimaryInput(PortId(port))
    }

    #[test]
    fn the_endpoint_index_survives_every_mutation() {
        let first = endpoint_id(1);
        let second = endpoint_id(2);
        let mut reservations = PhysicalReservations::new();
        assert_index_exact(&reservations, "empty");

        // reserve: indexed only for an endpoint keep-out.
        for (index, at) in [at(1, 1, 1), at(2, 1, 1), at(3, 1, 1)]
            .into_iter()
            .enumerate()
        {
            reservations.reserve(
                at,
                PhysicalReservationOwner::Endpoint(first),
                PhysicalReservationKind::KeepOut,
            );
            assert_index_exact(&reservations, &format!("reserve {index}"));
        }
        reservations.reserve(
            at(9, 1, 1),
            PhysicalReservationOwner::Endpoint(second),
            PhysicalReservationKind::KeepOut,
        );
        reservations.reserve(
            at(4, 1, 1),
            PhysicalReservationOwner::Route(RouteId(1)),
            PhysicalReservationKind::Conductor(dust()),
        );
        reservations.reserve(
            at(5, 1, 1),
            PhysicalReservationOwner::KeepOut(7),
            PhysicalReservationKind::KeepOut,
        );
        assert_index_exact(&reservations, "mixed owners");

        // reserve on an occupied cell changes nothing.
        reservations.reserve(
            at(1, 1, 1),
            PhysicalReservationOwner::Endpoint(second),
            PhysicalReservationKind::KeepOut,
        );
        assert_index_exact(&reservations, "reserve on an occupied cell");
        assert_eq!(
            endpoint_keep_outs_by_scan(&reservations, second),
            vec![at(9, 1, 1)]
        );

        // reserve_if_free, both outcomes.
        assert!(reservations.reserve_if_free(
            at(6, 1, 1),
            PhysicalReservationOwner::Endpoint(second),
            PhysicalReservationKind::KeepOut,
        ));
        assert!(!reservations.reserve_if_free(
            at(6, 1, 1),
            PhysicalReservationOwner::Endpoint(first),
            PhysicalReservationKind::KeepOut,
        ));
        assert_index_exact(&reservations, "reserve_if_free");

        // promote_endpoint_conductor: an indexed cell becomes a conductor.
        assert!(reservations.promote_endpoint_conductor(at(2, 1, 1), first, RouteId(3), dust()));
        assert_index_exact(&reservations, "promote_endpoint_conductor");
        assert!(!endpoint_keep_outs_by_scan(&reservations, first).contains(&at(2, 1, 1)));
        // The overwrite replaces an endpoint keep-out with a route conductor,
        // so the anchor has to leave the index with it -- otherwise a later
        // release would hand back a cell the route now owns.
        assert!(
            !reservations
                .endpoint_keep_outs
                .get(&first)
                .is_some_and(|cells| cells.contains(&at(2, 1, 1))),
            "promote_endpoint_conductor left its overwritten guard indexed"
        );

        // commit_routed replacing an indexed keep-out.
        assert!(reservations.commit_routed(
            at(3, 1, 1),
            PhysicalReservationOwner::Route(RouteId(4)),
            PhysicalReservationKind::Conductor(dust()),
            &[PhysicalReservationOwner::Endpoint(first)],
        ));
        assert_index_exact(&reservations, "commit_routed replacing a guard");

        // commit_routed refused: nothing moves.
        assert!(!reservations.commit_routed(
            at(9, 1, 1),
            PhysicalReservationOwner::Route(RouteId(5)),
            PhysicalReservationKind::Conductor(dust()),
            &[],
        ));
        assert_index_exact(&reservations, "commit_routed refused");

        // commit_routed *into* an endpoint keep-out: the new entry is indexed.
        assert!(reservations.commit_routed(
            at(5, 1, 1),
            PhysicalReservationOwner::Endpoint(second),
            PhysicalReservationKind::KeepOut,
            &[PhysicalReservationOwner::KeepOut(7)],
        ));
        assert_index_exact(&reservations, "commit_routed into a guard");

        // release_keep_out on an endpoint-owned cell.
        assert!(
            reservations.release_keep_out(at(5, 1, 1), PhysicalReservationOwner::Endpoint(second),)
        );
        assert_index_exact(&reservations, "release_keep_out");

        // release_endpoint_keep_out, hit and miss.
        assert!(reservations.release_endpoint_keep_out(at(1, 1, 1), first));
        assert!(!reservations.release_endpoint_keep_out(at(1, 1, 1), first));
        assert_index_exact(&reservations, "release_endpoint_keep_out");

        // A clone carries the index, and mutating the clone leaves it exact.
        let mut clone = reservations.clone();
        assert_eq!(clone, reservations);
        clone.release_endpoint_keep_outs(second);
        assert_index_exact(&clone, "clone released");
        assert_index_exact(&reservations, "original after the clone was released");
    }

    #[test]
    fn a_rolled_back_attempt_restores_the_map_exactly() {
        let guarded = endpoint_id(1);
        let mut reservations = PhysicalReservations::new();
        for x in 0..20 {
            reservations.reserve(
                at(x, 1, 0),
                PhysicalReservationOwner::KeepOut(u32::MAX - 1),
                PhysicalReservationKind::KeepOut,
            );
        }
        for x in 0..4 {
            reservations.reserve(
                at(x, 2, 0),
                PhysicalReservationOwner::Endpoint(guarded),
                PhysicalReservationKind::KeepOut,
            );
        }
        let before = reservations.clone();

        // Everything a branch attempt does to the map: release the sink's own
        // guards, then reserve a path and a terminal guard over free cells.
        reservations.begin_attempt();
        reservations.release_endpoint_keep_outs(guarded);
        let mut claimed = Vec::new();
        reserve_typed_path(
            RouteId(1),
            &[at(0, 5, 0), at(1, 5, 0), at(2, 5, 0)],
            &mut reservations,
            &mut |at, owner, kind| claimed.push((at, owner, kind)),
        );
        reserve_terminal_guard(
            RoutedSinkId {
                route: RouteId(1),
                ordinal: 0,
            },
            at(1, 5, 0),
            at(2, 5, 0),
            at(3, 5, 0),
            &mut reservations,
            &mut |at, owner, kind| claimed.push((at, owner, kind)),
        );
        assert!(
            !claimed.is_empty(),
            "the attempt must have written something"
        );
        assert_ne!(
            reservations, before,
            "the attempt must have changed the map"
        );

        reservations.rollback_attempt();
        assert_eq!(
            reservations, before,
            "rollback must restore the exact reservations a clone would have"
        );
        assert_index_exact(&reservations, "after rollback");
        assert_eq!(
            endpoint_keep_outs_by_scan(&reservations, guarded).len(),
            4,
            "the released guards must come back"
        );
    }

    #[test]
    fn repeated_attempts_roll_back_to_the_same_place_and_commit_once() {
        let guarded = endpoint_id(2);
        let mut reservations = PhysicalReservations::new();
        for x in 0..4 {
            reservations.reserve(
                at(x, 2, 0),
                PhysicalReservationOwner::Endpoint(guarded),
                PhysicalReservationKind::KeepOut,
            );
        }
        let before = reservations.clone();

        // Three abandoned attempts over different cells, as a branch that
        // reroutes twice would make.
        for round in 0..3i32 {
            reservations.begin_attempt();
            reservations.release_endpoint_keep_outs(guarded);
            reserve_typed_path(
                RouteId(round as u32),
                &[at(round, 6, 0), at(round + 1, 6, 0)],
                &mut reservations,
                &mut |_, _, _| {},
            );
            reservations.rollback_attempt();
            assert_eq!(reservations, before, "rollback {round}");
            assert_index_exact(&reservations, "repeated rollback");
        }

        // The accepted attempt keeps everything it wrote, and ending the
        // journal means a later rollback cannot reach back into it.
        reservations.begin_attempt();
        reservations.release_endpoint_keep_outs(guarded);
        reserve_typed_path(
            RouteId(9),
            &[at(0, 7, 0), at(1, 7, 0)],
            &mut reservations,
            &mut |_, _, _| {},
        );
        let committed = reservations.clone();
        reservations.end_attempt();
        reservations.rollback_attempt();
        assert_eq!(
            reservations, committed,
            "a rollback after the attempt ended must be a no-op"
        );
        assert_index_exact(&reservations, "after commit");
        assert!(endpoint_keep_outs_by_scan(&reservations, guarded).is_empty());
    }

    #[test]
    fn a_cell_touched_twice_in_one_attempt_rolls_back_to_its_first_value() {
        let guarded = endpoint_id(3);
        let cell = at(4, 4, 4);
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(
            cell,
            PhysicalReservationOwner::Endpoint(guarded),
            PhysicalReservationKind::KeepOut,
        );
        let before = reservations.clone();

        reservations.begin_attempt();
        // Released, re-reserved to another owner, then promoted: three writes
        // to one anchor, which the journal has to unwind in order.
        assert!(reservations.release_endpoint_keep_out(cell, guarded));
        reservations.reserve(
            cell,
            PhysicalReservationOwner::Endpoint(endpoint_id(4)),
            PhysicalReservationKind::KeepOut,
        );
        assert!(reservations.promote_endpoint_conductor(cell, endpoint_id(4), RouteId(1), dust()));
        reservations.rollback_attempt();

        assert_eq!(
            reservations, before,
            "the first value must be the one restored"
        );
        assert_index_exact(&reservations, "after unwinding a repeated write");
        assert_eq!(
            endpoint_keep_outs_by_scan(&reservations, guarded),
            vec![cell]
        );
    }

    #[test]
    fn releasing_an_endpoint_matches_the_old_full_scan_exactly() {
        let first = endpoint_id(1);
        let second = endpoint_id(2);
        let mut reservations = PhysicalReservations::new();
        // A map dense with cells that are not this endpoint's, which is the
        // shape the scan used to pay for.
        for x in 0..40 {
            for z in 0..10 {
                reservations.reserve(
                    at(x, 1, z),
                    PhysicalReservationOwner::KeepOut(u32::MAX - 1),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
        for x in 0..6 {
            reservations.reserve(
                at(x, 2, 0),
                PhysicalReservationOwner::Endpoint(first),
                PhysicalReservationKind::KeepOut,
            );
            reservations.reserve(
                at(x, 3, 0),
                PhysicalReservationOwner::Endpoint(second),
                PhysicalReservationKind::KeepOut,
            );
        }
        // One of this endpoint's cells promoted away, so the index and the
        // scan must agree about a cell that is no longer a guard.
        assert!(reservations.promote_endpoint_conductor(at(0, 2, 0), first, RouteId(1), dust()));

        let expected = endpoint_keep_outs_by_scan(&reservations, first);
        assert_eq!(expected.len(), 5);
        assert_eq!(
            reservations
                .endpoint_keep_outs
                .get(&first)
                .map(|cells| cells.iter().copied().collect::<Vec<_>>()),
            Some(expected.clone()),
            "the index must name exactly what the scan named, in the same order"
        );

        let mut scanned = reservations.clone();
        for at in &expected {
            scanned.release_endpoint_keep_out(*at, first);
        }
        reservations.release_endpoint_keep_outs(first);

        assert_eq!(
            reservations, scanned,
            "releasing by index must leave the identical map the scan did"
        );
        assert!(endpoint_keep_outs_by_scan(&reservations, first).is_empty());
        assert_eq!(endpoint_keep_outs_by_scan(&reservations, second).len(), 6);
        assert_index_exact(&reservations, "after a full endpoint release");

        // An endpoint with nothing indexed is a no-op, not a panic.
        let before = reservations.clone();
        reservations.release_endpoint_keep_outs(endpoint_id(99));
        assert_eq!(reservations, before);
    }

    /// Every `previous` chain shape the batch has to agree with the scalar on.
    ///
    /// Straight runs, ascents, descents, a chain that doubles back over its own
    /// column, and one with repeated geometry -- the cases where a chain cell
    /// lands exactly one or two below a candidate, or one above it.
    fn obstruction_chains() -> Vec<(Anchor, BTreeMap<Anchor, Anchor>)> {
        // `previous` is the search's parent map: a path back to the source,
        // never a cycle. A repeated cell would make one, and both the scalar
        // and the batch would walk it forever -- the shape A* cannot produce,
        // so neither guards against it.
        let chain_of = |cells: &[Anchor]| {
            let unique: BTreeSet<Anchor> = cells.iter().copied().collect();
            assert_eq!(unique.len(), cells.len(), "a chain must not revisit a cell");
            let mut previous = BTreeMap::new();
            for pair in cells.windows(2) {
                previous.insert(pair[1], pair[0]);
            }
            (*cells.last().unwrap(), previous)
        };
        vec![
            // A single cell with no history.
            (at(5, 5, 5), BTreeMap::new()),
            // Flat run east.
            chain_of(&[at(0, 5, 0), at(1, 5, 0), at(2, 5, 0), at(3, 5, 0)]),
            // Ascent, so a chain cell sits below later candidates.
            chain_of(&[at(0, 1, 0), at(1, 2, 0), at(2, 3, 0), at(3, 4, 0)]),
            // Descent.
            chain_of(&[at(0, 6, 0), at(1, 5, 0), at(2, 4, 0), at(3, 3, 0)]),
            // Up then back over the same column: the smothering case, where
            // the successor stands higher than the cell.
            chain_of(&[at(2, 3, 0), at(2, 4, 0), at(1, 4, 0), at(1, 5, 0)]),
            // Down then along, so a cell sits one above a candidate.
            chain_of(&[at(1, 6, 1), at(1, 5, 1), at(1, 4, 1), at(2, 4, 1)]),
            // The same two columns walked at several heights. Every cell is
            // distinct: `previous` is a tree, and a chain that revisited a cell
            // would make the walk -- scalar and batch alike -- never terminate.
            chain_of(&[
                at(4, 1, 4),
                at(4, 2, 4),
                at(5, 2, 4),
                at(5, 3, 4),
                at(4, 3, 4),
                at(4, 4, 4),
            ]),
            // Long flat chain, to exercise a walk that does not hit anything.
            chain_of(&(0..24).map(|x| at(x, 7, 9)).collect::<Vec<_>>()),
        ]
    }

    #[test]
    fn the_batch_matches_twelve_scalar_calls() {
        let mut compared = 0usize;
        let mut obstructions = 0usize;
        for (at_cell, previous) in obstruction_chains() {
            // Ask about the neighbours of the chain's own end, and about a few
            // other origins, so candidates land above, below and beside it.
            for origin in [
                at_cell,
                at(at_cell.x, at_cell.y + 1, at_cell.z),
                at(1, 4, 0),
            ] {
                let candidates = neighbours(origin);
                assert_eq!(candidates.len(), 12);
                for seed_rules in [false, true] {
                    let scalar: Vec<bool> = candidates
                        .iter()
                        .map(|next| self_obstructs_typed(&previous, origin, *next, seed_rules))
                        .collect();
                    let mut batched = vec![false; candidates.len()];
                    self_obstructs_batch(&previous, origin, &candidates, seed_rules, &mut batched);
                    assert_eq!(
                        batched, scalar,
                        "batch and scalar disagree at {origin:?} seed_rules={seed_rules} \
                         chain={previous:?}"
                    );
                    compared += candidates.len();
                    obstructions += scalar.iter().filter(|hit| **hit).count();
                }
            }
        }
        assert!(compared >= 500, "only {compared} candidates compared");
        assert!(
            obstructions > 0,
            "every candidate was free, so the comparison proved nothing"
        );
    }

    #[test]
    fn the_batch_agrees_when_the_walk_starts_away_from_the_chain() {
        // `search_path` also asks about a cell that is not the chain's end --
        // the required source exit -- so the walk begins somewhere `previous`
        // may know nothing about.
        let (_, previous) = {
            let cells = [at(0, 1, 0), at(1, 2, 0), at(2, 2, 0), at(2, 3, 0)];
            let mut map = BTreeMap::new();
            for pair in cells.windows(2) {
                map.insert(pair[1], pair[0]);
            }
            (cells[cells.len() - 1], map)
        };
        for origin in [at(2, 3, 0), at(9, 9, 9), at(1, 2, 0)] {
            for seed_rules in [false, true] {
                let candidates = neighbours(origin);
                let scalar: Vec<bool> = candidates
                    .iter()
                    .map(|next| self_obstructs_typed(&previous, origin, *next, seed_rules))
                    .collect();
                let mut batched = vec![false; candidates.len()];
                self_obstructs_batch(&previous, origin, &candidates, seed_rules, &mut batched);
                assert_eq!(batched, scalar, "{origin:?} seed_rules={seed_rules}");
            }
        }
    }

    /// The access-band blanket owner `compose` releases per trunk.
    const BAND: PhysicalReservationOwner = PhysicalReservationOwner::KeepOut(u32::MAX - 1);

    #[test]
    fn a_committed_route_takes_the_keep_outs_it_was_released() {
        let cell = at(4, 2, 6);
        let route = RouteId(7);
        let guard = PhysicalReservationOwner::Endpoint(PhysicalEndpointId::PrimaryInput(PortId(3)));
        for held in [BAND, guard] {
            let mut reservations = PhysicalReservations::new();
            reservations.reserve(cell, held, PhysicalReservationKind::KeepOut);
            assert!(reservations.commit_routed(
                cell,
                PhysicalReservationOwner::Route(route),
                PhysicalReservationKind::Conductor(dust()),
                &[BAND, guard],
            ));
            assert_eq!(
                reservations.get(&cell),
                Some(&PhysicalReservation {
                    owner: PhysicalReservationOwner::Route(route),
                    kind: PhysicalReservationKind::Conductor(dust()),
                }),
                "a released {held:?} keep-out did not yield to the route that was laid"
            );
        }
    }

    #[test]
    fn a_committed_route_is_refused_by_a_keep_out_it_was_never_released() {
        let cell = at(4, 2, 6);
        let route = RouteId(7);
        let later = PhysicalReservationOwner::Endpoint(PhysicalEndpointId::PrimaryInput(PortId(9)));
        // A child halo, the caller row, a later trunk's endpoint guard, and an
        // earlier route's clearance all stood during this trunk's search.
        for held in [
            PhysicalReservationOwner::KeepOut(0),
            PhysicalReservationOwner::KeepOut(u32::MAX),
            later,
            PhysicalReservationOwner::KeepOut(1),
        ] {
            let mut reservations = PhysicalReservations::new();
            reservations.reserve(cell, held, PhysicalReservationKind::KeepOut);
            assert!(
                !reservations.commit_routed(
                    cell,
                    PhysicalReservationOwner::Route(route),
                    PhysicalReservationKind::Conductor(dust()),
                    &[BAND],
                ),
                "{held:?} was not released to this trunk but gave way anyway"
            );
            assert_eq!(
                reservations.get(&cell).map(|held| held.owner),
                Some(held),
                "a refused commit must leave the map untouched"
            );
        }
    }

    #[test]
    fn a_committed_route_is_refused_by_another_route_conductor() {
        let cell = at(4, 2, 6);
        let mut reservations = PhysicalReservations::new();
        let held = PhysicalReservation {
            owner: PhysicalReservationOwner::Route(RouteId(1)),
            kind: PhysicalReservationKind::Conductor(dust()),
        };
        reservations.reserve(cell, held.owner, held.kind.clone());
        assert!(!reservations.commit_routed(
            cell,
            PhysicalReservationOwner::Route(RouteId(2)),
            PhysicalReservationKind::Conductor(dust()),
            &[BAND, held.owner],
        ));
        assert_eq!(
            reservations.get(&cell),
            Some(&held),
            "a refused commit must leave the map untouched"
        );
    }

    #[test]
    fn an_identical_floor_is_shared_and_keeps_its_first_owner() {
        let cell = at(4, 1, 6);
        let first = PhysicalReservationOwner::RouteStair(RouteId(1));
        let mut reservations = PhysicalReservations::new();
        reservations.reserve(cell, first, PhysicalReservationKind::Floor(stone()));
        assert!(reservations.commit_routed(
            cell,
            PhysicalReservationOwner::RouteStair(RouteId(2)),
            PhysicalReservationKind::Floor(stone()),
            &[BAND],
        ));
        assert_eq!(
            reservations.get(&cell).map(|held| held.owner),
            Some(first),
            "a shared floor must not change hands"
        );
        assert!(
            !reservations.commit_routed(
                cell,
                PhysicalReservationOwner::Route(RouteId(2)),
                PhysicalReservationKind::Conductor(dust()),
                &[BAND],
            ),
            "a conductor must not take a live floor cell"
        );
    }

    use crate::compile::MAX_DUST_RUN;

    #[test]
    fn reserve_policy_matches_route_mode() {
        assert_eq!(
            reserve_policy(false, false),
            ReservePolicy::TotalStairs,
            "only the non-strict legacy adapter keeps the pinned reserve"
        );
        for (strict_local, seed_rules) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                reserve_policy(strict_local, seed_rules),
                ReservePolicy::LatestLegalCell,
                "strict/seed route modes use the geometry-aware reserve"
            );
        }
    }

    /// A path along `+x` that climbs one cell every `stair_every` steps.
    ///
    /// Each climb is a single vertical step, so the cells a repeater may not
    /// stand on come in short runs with plain dust between them -- a long
    /// route over uneven ground, which is what a parent corridor lays.
    fn climbing_path(len: usize, stair_every: usize) -> Vec<Anchor> {
        let mut cells = Vec::with_capacity(len);
        let (mut x, mut y) = (1i32, 1i32);
        for index in 0..len {
            if index > 0 && index % stair_every == 0 {
                y += 1;
            } else {
                x += 1;
            }
            cells.push(Anchor { x, y, z: 0 });
        }
        cells
    }

    #[test]
    fn isolated_stairs_do_not_spend_the_whole_dust_budget() {
        // Fifteen climbs: enough that the legacy total saturates the cap, the
        // same shape the seven_segment decoder's long trunks have.
        let cells = climbing_path(180, 12);
        let source = Anchor { x: 0, y: 1, z: 0 };

        let laid = realise_branch_from_with_boundary_policy(
            source,
            MAX_SIGNAL_STRENGTH,
            &cells,
            false,
            ReservePolicy::LatestLegalCell,
        );
        assert!(laid.carries, "the branch must still arrive powered");

        // Isolated climbs hold back only their own longest run, so the
        // refreshes stay near what a legal dust run costs rather than landing
        // every third cell.
        let legal_bound = cells.len() as u64 / (MAX_DUST_RUN as u64 - 2);
        assert!(
            laid.repeaters <= legal_bound + 2,
            "{} repeaters over {} cells, expected near {legal_bound}",
            laid.repeaters,
            cells.len()
        );

        // The legacy policy on the identical path: every stair counted, the
        // cap saturated, a refresh roughly every third cell.
        let legacy = realise_branch_from(source, MAX_SIGNAL_STRENGTH, &cells);
        assert!(legacy.carries);
        assert!(
            legacy.repeaters > laid.repeaters * 3,
            "the legacy policy must be the expensive one: {} vs {}",
            legacy.repeaters,
            laid.repeaters
        );
    }

    #[test]
    fn a_run_of_adjacent_ineligible_cells_is_crossed_from_the_latest_cell_before_it() {
        // One long climb rather than many short ones: the walk-back has to
        // cross all of it and land on the last flat cell before it.
        let mut cells = Vec::new();
        let (mut x, mut y) = (1i32, 1i32);
        for _ in 0..40 {
            x += 1;
            cells.push(Anchor { x, y, z: 0 });
        }
        for _ in 0..6 {
            y += 1;
            cells.push(Anchor { x, y, z: 0 });
        }
        for _ in 0..40 {
            x += 1;
            cells.push(Anchor { x, y, z: 0 });
        }
        let source = Anchor { x: 0, y: 1, z: 0 };

        let laid = realise_branch_from_with_boundary_policy(
            source,
            MAX_SIGNAL_STRENGTH,
            &cells,
            false,
            ReservePolicy::LatestLegalCell,
        );
        assert!(
            laid.carries,
            "a six-cell climb must still be crossed with the signal intact"
        );
        // Hand-walked greedy plan: refresh where the wire would otherwise die
        // (14, 29), then 44 is inside the climb so the walk-back lands on the
        // last flat cell whose exit is flat (38), then 53, 68, 83.
        let refreshes = laid
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.kind == BlockKind::Repeater)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(refreshes, vec![14, 29, 38, 53, 68, 83]);
        // Nothing earlier than demanded: the same path costs no more than a
        // path of the same length with isolated single steps.
        let isolated = realise_branch_from_with_boundary_policy(
            source,
            MAX_SIGNAL_STRENGTH,
            &climbing_path(cells.len(), 20),
            false,
            ReservePolicy::LatestLegalCell,
        );
        assert!(isolated.carries);
        assert!(
            laid.repeaters <= isolated.repeaters + 1,
            "the long run must not shorten every cycle on the branch: {} vs {}",
            laid.repeaters,
            isolated.repeaters
        );
    }

    /// The exact model `realise_branch_from_with_boundary_policy` certifies
    /// against: the source carries `incoming`, dust loses one per hop, a
    /// repeater restores the full strength.
    fn modelled_strengths(incoming: u8, laid: &LaidBranch) -> Vec<u8> {
        let mut carried = incoming;
        laid.blocks
            .iter()
            .map(|block| {
                carried = if block.kind == BlockKind::Repeater {
                    MAX_SIGNAL_STRENGTH
                } else {
                    carried.saturating_sub(1)
                };
                carried
            })
            .collect()
    }

    /// Build the laid branch in a real world -- a redstone block feeding a
    /// straight lead-in whose last cell is `source` and reads exactly
    /// `incoming` -- and settle it.
    fn settle_laid_branch(
        source: Anchor,
        incoming: u8,
        cells: &[Anchor],
        laid: &LaidBranch,
    ) -> crate::redstone::simulator::Simulator {
        use crate::redstone::simulator::Simulator;
        use crate::redstone::world::storage::World;

        let lead_in = i32::from(MAX_SIGNAL_STRENGTH) + 1 - i32::from(incoming);
        let block_x = source.x - lead_in;
        assert!(block_x >= 1, "the lead-in must fit west of the source");
        let mut world = World::new(90, 10, 30);
        world.set(
            block_x,
            source.y,
            source.z,
            crate::compile::redstone_block(),
        );
        world.set(block_x, source.y - 1, source.z, stone());
        for x in block_x + 1..=source.x {
            world.set(x, source.y, source.z, dust());
            world.set(x, source.y - 1, source.z, stone());
        }
        for (cell, (block, floor)) in cells.iter().zip(laid.blocks.iter().zip(&laid.floors)) {
            world.set(cell.x, cell.y, cell.z, block.clone());
            world.set(cell.x, cell.y - 1, cell.z, floor.clone());
        }
        let mut simulator = Simulator::new(world);
        simulator
            .run_until_stable(4_000)
            .expect("a dust-and-repeater branch settles");
        assert_eq!(
            simulator.world().get(source.x, source.y, source.z).power,
            incoming,
            "the lead-in delivers exactly the modelled incoming strength"
        );
        simulator
    }

    /// Every cell of a laid branch, checked in the settled world against the
    /// model: dust alive and no weaker than modelled, repeaters lit and
    /// standing only where `route_step_is_legal` accepts one.
    fn assert_branch_alive_in_simulation(
        source: Anchor,
        incoming: u8,
        cells: &[Anchor],
        laid: &LaidBranch,
    ) {
        let simulator = settle_laid_branch(source, incoming, cells, laid);
        let modelled = modelled_strengths(incoming, laid);
        for (index, cell) in cells.iter().enumerate() {
            let state = simulator.world().get(cell.x, cell.y, cell.z);
            let previous = if index == 0 { source } else { cells[index - 1] };
            match state.kind {
                BlockKind::Repeater => {
                    // A refresh on the last cell is the mandatory terminal
                    // repeater; its support continues straight on.
                    let next = cells.get(index + 1).copied().unwrap_or(at(
                        cell.x + (cell.x - previous.x),
                        cell.y,
                        cell.z + (cell.z - previous.z),
                    ));
                    assert!(
                        route_step_is_legal(previous, *cell, next, state),
                        "repeater at index {index} ({cell:?}) is not a legal straight flat refresh"
                    );
                    assert!(state.lit, "repeater at index {index} ({cell:?}) is not lit");
                }
                BlockKind::RedstoneWire => {
                    assert!(
                        state.power > 0,
                        "dust at index {index} ({cell:?}) is dead in simulation"
                    );
                    assert!(
                        state.power >= modelled[index],
                        "dust at index {index} ({cell:?}) reads {} but the model promised {}",
                        state.power,
                        modelled[index]
                    );
                }
                other => panic!("unexpected {other:?} at index {index}"),
            }
        }
        let terminal_index = cells.len() - 1;
        assert_eq!(
            modelled[terminal_index - 1],
            laid.strength_before_terminal,
            "strength_before_terminal is the modelled strength of the penultimate cell"
        );
    }

    /// East 10, two steps up, east 5, south 7, east 13, two steps down,
    /// east 18: turns and both staircase directions on one fixed route.
    fn stepped_bent_path(source: Anchor) -> Vec<Anchor> {
        let mut cells = Vec::new();
        let mut cursor = source;
        let mut push = |cursor: &mut Anchor, dx: i32, dy: i32, dz: i32| {
            *cursor = at(cursor.x + dx, cursor.y + dy, cursor.z + dz);
            cells.push(*cursor);
        };
        for _ in 0..10 {
            push(&mut cursor, 1, 0, 0);
        }
        for _ in 0..2 {
            push(&mut cursor, 1, 1, 0);
        }
        for _ in 0..5 {
            push(&mut cursor, 1, 0, 0);
        }
        for _ in 0..7 {
            push(&mut cursor, 0, 0, 1);
        }
        for _ in 0..13 {
            push(&mut cursor, 1, 0, 0);
        }
        for _ in 0..2 {
            push(&mut cursor, 1, -1, 0);
        }
        for _ in 0..18 {
            push(&mut cursor, 1, 0, 0);
        }
        cells
    }

    /// East 6, north 3, east 8, south 3, east 20: a flat detour around an
    /// obstacle, four turns.
    fn detour_path(source: Anchor) -> Vec<Anchor> {
        let mut cells = Vec::new();
        let mut cursor = source;
        let mut push = |cursor: &mut Anchor, dx: i32, dz: i32| {
            *cursor = at(cursor.x + dx, cursor.y, cursor.z + dz);
            cells.push(*cursor);
        };
        for _ in 0..6 {
            push(&mut cursor, 1, 0);
        }
        for _ in 0..3 {
            push(&mut cursor, 0, -1);
        }
        for _ in 0..8 {
            push(&mut cursor, 1, 0);
        }
        for _ in 0..3 {
            push(&mut cursor, 0, 1);
        }
        for _ in 0..20 {
            push(&mut cursor, 1, 0);
        }
        cells
    }

    #[test]
    fn latest_legal_refreshes_keep_bent_and_stepped_branches_alive_in_simulation() {
        let source = at(20, 1, 5);
        for (name, cells) in [
            ("stepped_bent", stepped_bent_path(source)),
            ("detour", detour_path(source)),
        ] {
            for incoming in [2u8, 3, 5, 9, 14, MAX_SIGNAL_STRENGTH] {
                let laid = realise_branch_from_with_boundary_policy(
                    source,
                    incoming,
                    &cells,
                    true,
                    ReservePolicy::LatestLegalCell,
                );
                assert!(laid.carries, "{name} at incoming {incoming} must carry");
                assert_branch_alive_in_simulation(source, incoming, &cells, &laid);
            }
        }
    }

    #[test]
    fn latest_legal_refreshes_stand_exactly_where_the_wire_would_die() {
        let source = at(20, 1, 5);
        let cells = (1..=40).map(|dx| at(20 + dx, 1, 5)).collect::<Vec<_>>();
        let refreshes = |incoming: u8| {
            let laid = realise_branch_from_with_boundary_policy(
                source,
                incoming,
                &cells,
                true,
                ReservePolicy::LatestLegalCell,
            );
            assert!(laid.carries);
            assert_branch_alive_in_simulation(source, incoming, &cells, &laid);
            laid.blocks
                .iter()
                .enumerate()
                .filter(|(_, block)| block.kind == BlockKind::Repeater)
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        // A full source: cell 13 is the fourteenth hop (modelled 1), cell 14
        // would be dead, so the refresh stands on 14.
        assert_eq!(refreshes(MAX_SIGNAL_STRENGTH), vec![14, 29]);
        // Arriving at 6: five hops are left, so the refresh stands on 5.
        assert_eq!(refreshes(6), vec![5, 20, 35]);
        // Arriving at 1: nothing is left, so the refresh is the first cell.
        assert_eq!(refreshes(1), vec![0, 15, 30]);
    }

    #[test]
    fn a_stepped_branch_no_longer_pays_a_global_reserve_for_one_climb() {
        // Before: a two-step climb held two cells back from *every* cycle.
        // Now the climb is paid for exactly once, by the walk-back.
        let source = at(20, 1, 5);
        let cells = stepped_bent_path(source);
        let refreshes = |incoming: u8| {
            let laid = realise_branch_from_with_boundary_policy(
                source,
                incoming,
                &cells,
                true,
                ReservePolicy::LatestLegalCell,
            );
            assert!(laid.carries);
            assert_branch_alive_in_simulation(source, incoming, &cells, &laid);
            laid.blocks
                .iter()
                .enumerate()
                .filter(|(_, block)| block.kind == BlockKind::Repeater)
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        // Arriving at 11 the wire would die on 10, the first climbing step;
        // 9 exits upward, so the refresh stands on 8. Then 23 is the turn out
        // of the southward run (22), 37 the first descending step with 36
        // exiting downward (35), then 50.
        assert_eq!(refreshes(11), vec![8, 22, 35, 50]);
        // Arriving full, every demand happens to fall on a flat straight
        // cell, and nothing is refreshed any earlier for the climb's sake.
        assert_eq!(refreshes(MAX_SIGNAL_STRENGTH), vec![14, 29, 44]);
    }

    #[test]
    fn an_ineligible_run_longer_than_the_budget_is_refused_not_bridged() {
        // Two flat cells, then a sixteen-step climb, then flat: after the
        // refresh on cell 0 nothing until cell 19 may hold a repeater, and
        // the wire dies on the climb. The branch must say so.
        let source = at(20, 1, 5);
        let mut cells = Vec::new();
        let mut cursor = source;
        for _ in 0..2 {
            cursor = at(cursor.x + 1, cursor.y, cursor.z);
            cells.push(cursor);
        }
        for _ in 0..16 {
            cursor = at(cursor.x + 1, cursor.y + 1, cursor.z);
            cells.push(cursor);
        }
        for _ in 0..5 {
            cursor = at(cursor.x + 1, cursor.y, cursor.z);
            cells.push(cursor);
        }
        let laid = realise_branch_from_with_boundary_policy(
            source,
            MAX_SIGNAL_STRENGTH,
            &cells,
            true,
            ReservePolicy::LatestLegalCell,
        );
        assert!(
            !laid.carries,
            "a climb longer than the dust budget cannot be certified"
        );
        for (index, block) in laid.blocks.iter().enumerate() {
            if block.kind == BlockKind::Repeater {
                let previous = if index == 0 { source } else { cells[index - 1] };
                assert!(
                    route_step_is_legal(previous, cells[index], cells[index + 1], block),
                    "refusal must not fabricate an illegal repeater at {index}"
                );
            }
        }
        // And the refusal is physical, not a modelling artefact: the settled
        // world has dead dust on the climb.
        let mut world = crate::redstone::world::storage::World::new(90, 24, 30);
        world.set(
            source.x - 1,
            source.y,
            source.z,
            crate::compile::redstone_block(),
        );
        world.set(source.x - 1, source.y - 1, source.z, stone());
        world.set(source.x, source.y, source.z, dust());
        world.set(source.x, source.y - 1, source.z, stone());
        for (cell, (block, floor)) in cells.iter().zip(laid.blocks.iter().zip(&laid.floors)) {
            world.set(cell.x, cell.y, cell.z, block.clone());
            world.set(cell.x, cell.y - 1, cell.z, floor.clone());
        }
        let mut simulator = crate::redstone::simulator::Simulator::new(world);
        simulator.run_until_stable(4_000).expect("settles");
        let dead = cells.iter().enumerate().find(|(_, cell)| {
            let state = simulator.world().get(cell.x, cell.y, cell.z);
            state.kind == BlockKind::RedstoneWire && state.power == 0
        });
        assert!(
            dead.is_some(),
            "the model refused a branch the simulation carries"
        );
    }

    #[test]
    fn a_staircase_step_and_the_cell_before_it_are_walked_over_but_the_landing_is_not() {
        // Thirteen flat cells, one step up at index 13, flat again. The step
        // is entered from below and the cell before it exits upward: neither
        // may refresh. The landing after the step is entered flat and may.
        let source = at(20, 1, 5);
        let mut cells = (1..=13).map(|dx| at(20 + dx, 1, 5)).collect::<Vec<_>>();
        cells.push(at(34, 2, 5));
        cells.extend((35..=50).map(|x| at(x, 2, 5)));
        let first_refresh = |incoming: u8| {
            let laid = realise_branch_from_with_boundary_policy(
                source,
                incoming,
                &cells,
                true,
                ReservePolicy::LatestLegalCell,
            );
            assert!(laid.carries);
            assert_branch_alive_in_simulation(source, incoming, &cells, &laid);
            laid.blocks
                .iter()
                .position(|block| block.kind == BlockKind::Repeater)
                .expect("a 30-cell branch is refreshed")
        };
        // Demand on the landing: legal, taken as is.
        assert_eq!(first_refresh(MAX_SIGNAL_STRENGTH), 14);
        // Demand on the step: walked back over the step and its approach.
        assert_eq!(first_refresh(14), 11);
        // Demand on the approach cell: walked back one.
        assert_eq!(first_refresh(13), 11);
    }

    /// The branch handed to the planner ends on the sink's own cell. A
    /// contract that cannot host a repeater there (`DirectedDust`) must have
    /// a refresh the budget demands on that cell walked back onto the
    /// branch, so the emitted terminal dust is alive in simulation and the
    /// branch's repeater count is what was actually laid. A contract that
    /// can (`Repeater`) keeps the terminal refresh, lit.
    #[test]
    fn a_demanded_refresh_on_a_dust_terminal_moves_onto_the_branch() {
        use crate::redstone::simulator::Simulator;
        use crate::redstone::world::storage::World;

        let source = at(2, 1, 5);

        // Exhausted arrival on a one-cell branch: a dust terminal has nowhere
        // for the refresh to stand, so the branch is refused, not bridged
        // with a repeater the terminal would overwrite. A terminal that may
        // host the refresh keeps it.
        let exhausted = realise_branch_cells(
            source,
            1,
            &[at(3, 1, 5)],
            true,
            ReservePolicy::LatestLegalCell,
            false,
        );
        assert!(
            !exhausted.carries,
            "an exhausted dust terminal cannot carry"
        );
        assert_eq!(
            exhausted.repeaters, 0,
            "the refusal must not count a phantom refresh"
        );
        let hosted = realise_branch_cells(
            source,
            1,
            &[at(3, 1, 5)],
            true,
            ReservePolicy::LatestLegalCell,
            true,
        );
        assert!(hosted.carries);
        assert_eq!(hosted.blocks[0].kind, BlockKind::Repeater);

        // Straight strict routes over every length around the one where the
        // budget's demand lands exactly on the terminal cell.
        let route = RouteId(77);
        let source_endpoint = RouteEndpoint {
            anchor: source,
            allowed_exit: Facing::East,
            ..endpoint(route)
        };
        let mut walked_back_onto_the_branch = false;
        for distance in 12..=17 {
            for requirement in [
                TerminalRequirement::DirectedDust,
                TerminalRequirement::Repeater,
            ] {
                let terminal = at(source.x + distance, 1, 5);
                let connection = connection(30, 0);
                let typed_sink = RouteSink {
                    id: RoutedSinkId { route, ordinal: 0 },
                    endpoint: PhysicalEndpointId::Landing(connection),
                    anchor: terminal,
                    allowed_entry: Facing::West,
                    terminal: TerminalContract::Sink {
                        target: RouteTarget::Connection(connection),
                        support: at(terminal.x + 1, 1, 5),
                        requirement,
                    },
                };
                let sinks = NonEmptyRouteSinks::new(vec![typed_sink]).unwrap();
                let reservations = PhysicalReservations::new();
                let tree = route_strict_with_policy(
                    RouteRequest {
                        id: route,
                        source: source_endpoint.clone(),
                        sinks: &sinks,
                        reservations: &reservations,
                        limits: RouterLimits {
                            max_node_expansions: 100_000,
                            max_queue_entries: 200_000,
                        },
                    },
                    RoutingJoinPolicy::Off,
                    |_| 0,
                    |_, _, _| {},
                )
                .unwrap_or_else(|error| {
                    panic!("{requirement:?} terminal at distance {distance}: {error:?}")
                });
                let branch = &tree.branches[0];
                let laid_repeaters = tree
                    .cells
                    .iter()
                    .filter(|cell| {
                        branch.path.contains(&cell.at) && cell.state.kind == BlockKind::Repeater
                    })
                    .count() as u64;
                let terminal_state = &tree
                    .cells
                    .iter()
                    .find(|cell| cell.at == terminal)
                    .expect("the terminal cell is laid")
                    .state;
                let penultimate = branch.path[branch.path.len() - 2];
                let penultimate_is_repeater = tree
                    .cells
                    .iter()
                    .any(|cell| cell.at == penultimate && cell.state.kind == BlockKind::Repeater);
                match requirement {
                    TerminalRequirement::DirectedDust => {
                        assert_eq!(terminal_state.kind, BlockKind::RedstoneWire);
                        // A dust terminal is never a refresh, so the branch's
                        // count must be exactly the repeaters on the path.
                        assert_eq!(
                            branch.terminal.repeaters, laid_repeaters,
                            "dust terminal at distance {distance}: the counted repeaters must be the laid ones"
                        );
                        walked_back_onto_the_branch |= penultimate_is_repeater;
                    }
                    _ => assert_eq!(terminal_state.kind, BlockKind::Repeater),
                }

                // The emitted physical signal: a redstone block drives the
                // source cell at full strength, the tree is laid as emitted.
                let mut world = World::new(30, 4, 10);
                world.set(source.x - 1, 1, 5, crate::compile::redstone_block());
                world.set(source.x - 1, 0, 5, stone());
                world.set(source.x, 1, 5, dust());
                world.set(source.x, 0, 5, stone());
                for block in tree.owned_blocks() {
                    world.set(block.at.x, block.at.y, block.at.z, block.state);
                }
                let mut simulator = Simulator::new(world);
                simulator
                    .run_until_stable(4_000)
                    .expect("a straight route settles");
                let settled = simulator.world().get(terminal.x, terminal.y, terminal.z);
                match settled.kind {
                    BlockKind::RedstoneWire => assert!(
                        settled.power > 0,
                        "{requirement:?} at distance {distance}: the terminal dust is dead"
                    ),
                    BlockKind::Repeater => assert!(
                        settled.lit,
                        "{requirement:?} at distance {distance}: the terminal repeater is unlit"
                    ),
                    other => panic!("unexpected {other:?} at the terminal"),
                }
            }
        }
        assert!(
            walked_back_onto_the_branch,
            "one distance must put the demand on the dust terminal and walk it back"
        );
    }

    #[test]
    fn the_legacy_reserve_is_unchanged() {
        // The reference the pinned layouts are measured against: total stairs,
        // capped. Pinned here as exact numbers so the split cannot drift.
        let source = Anchor { x: 0, y: 1, z: 0 };
        for (len, stair_every) in [(180usize, 20usize), (60, 10), (30, 29)] {
            let cells = climbing_path(len, stair_every);
            let stairs = cells
                .iter()
                .scan(source, |previous, cell| {
                    let climbed = cell.y != previous.y;
                    *previous = *cell;
                    Some(climbed)
                })
                .filter(|climbed| *climbed)
                .count();
            let expected_reserve = (stairs as i32).min(MAX_DUST_RUN - 2);
            let laid = realise_branch_from(source, MAX_SIGNAL_STRENGTH, &cells);
            let planned = crate::compile::plan_bent_path(
                cells.len(),
                &bend_indices_of(source, &cells),
                MAX_SIGNAL_STRENGTH,
                expected_reserve,
            );
            assert_eq!(
                laid.repeaters,
                planned.0.iter().filter(|placed| **placed).count() as u64,
                "legacy reserve for {len} cells every {stair_every}"
            );
        }
    }

    /// The bend set `realise_branch_from` derives, rebuilt for the test above.
    fn bend_indices_of(source: Anchor, cells: &[Anchor]) -> std::collections::BTreeSet<usize> {
        let mut bends: std::collections::BTreeSet<usize> = cells
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
        bends
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
            access_half_width: 0,
            preferred_y: None,
            access_y: None,
            hard: false,
            penalty_per_block: 3,
        };

        assert_eq!(guidance.penalty(at(20, 1, 4)), 0);
        assert_eq!(guidance.penalty(at(-20, 5, 8)), 0);
        assert_eq!(guidance.penalty(at(0, 1, 10)), 3);
        assert_eq!(guidance.penalty(at(0, 1, 0)), 3);
    }

    /// Off the track the wanted height is `access_y`, on it `preferred_y`;
    /// each is one block of penalty, on top of the track penalty itself.
    #[test]
    fn guidance_wants_the_lane_height_on_track_and_the_access_height_off_it() {
        let guidance = RouteGuidance {
            origin: at(0, 0, 0),
            lateral: Facing::South,
            track: 6,
            half_width: 0,
            access_half_width: 3,
            preferred_y: Some(3),
            access_y: Some(1),
            hard: true,
            penalty_per_block: 8,
        };

        assert_eq!(guidance.penalty(at(10, 3, 6)), 0);
        assert_eq!(guidance.penalty(at(10, 1, 6)), 8);
        assert_eq!(guidance.penalty(at(10, 1, 9)), 8);
        assert_eq!(guidance.penalty(at(10, 3, 9)), 16);
        assert_eq!(guidance.penalty(at(10, 2, 9)), 16);

        // Without an access height the lane height is wanted everywhere.
        let flat = RouteGuidance {
            access_y: None,
            ..guidance
        };
        assert_eq!(flat.penalty(at(10, 3, 9)), 8);
        assert_eq!(flat.penalty(at(10, 1, 9)), 16);
    }

    #[test]
    fn hard_guidance_admits_only_its_track_and_endpoint_access_columns() {
        let guidance = RouteGuidance {
            origin: at(0, 1, 0),
            lateral: Facing::South,
            track: 6,
            half_width: 0,
            access_half_width: 2,
            preferred_y: Some(3),
            access_y: None,
            hard: true,
            penalty_per_block: 8,
        };
        let source = at(2, 1, 20);
        let sink = at(30, 1, 20);

        assert!(guidance.allows(at(18, 3, 6), source, sink));
        assert!(guidance.allows(at(4, 2, 14), source, sink));
        assert!(guidance.allows(at(28, 4, 14), source, sink));
        assert!(!guidance.allows(at(18, 3, 14), source, sink));
    }

    /// The absolute plane a guidance names, on the axis its `lateral` runs.
    ///
    /// `track` is a distance from `origin` along `lateral`, so the sign flips
    /// between north and south and between east and west; a vertical
    /// `lateral` names no plane at all, because [`RouteGuidance::relative`] is
    /// zero there and [`RouteGuidance::allows`] admits everything.
    #[test]
    fn a_guidance_track_resolves_to_one_absolute_plane_per_lateral() {
        let guidance = |lateral, half_width| RouteGuidance {
            origin: at(100, 1, 200),
            lateral,
            track: 6,
            half_width,
            access_half_width: 2,
            preferred_y: None,
            access_y: None,
            hard: true,
            penalty_per_block: 8,
        };

        assert_eq!(guidance(Facing::South, 0).track_span(), Some((206, 206)));
        assert_eq!(guidance(Facing::North, 0).track_span(), Some((194, 194)));
        assert_eq!(guidance(Facing::East, 0).track_span(), Some((106, 106)));
        assert_eq!(guidance(Facing::West, 0).track_span(), Some((94, 94)));
        assert_eq!(guidance(Facing::South, 2).track_span(), Some((204, 208)));
        assert_eq!(guidance(Facing::Up, 0).track_span(), None);
        assert_eq!(guidance(Facing::Down, 0).track_span(), None);
    }

    /// A hard guidance may name a plane the branch's own endpoints do not
    /// bracket, and the search box has to contain it.
    ///
    /// The parent owns the corridor and hands every trunk a lane. A trunk
    /// whose two ends sit a few cells apart on the portal row can be handed a
    /// lane far deeper than the straight-line margin between those ends
    /// reaches -- and then every cell the guidance admits outside the two
    /// access columns lies past the box wall, so the columns dead-end and the
    /// search reports no route through a corridor that is entirely empty.
    ///
    /// Measured in `seven_segment` composed through recursive contracts as
    /// trunk `g32`: (623,1,117) -> (651,1,116), margin 31, box floor z = 85,
    /// lane z = 72. This is that shape at a size a unit test can hold: ends
    /// eight cells apart on z = 40, lane thirty cells north of them.
    #[test]
    fn a_hard_guidance_track_beyond_the_endpoint_margin_is_still_reachable() {
        let route = RouteId(0);
        let (source_anchor, sink_anchor, lane) = (at(20, 1, 40), at(28, 1, 40), 10);
        let source = RouteEndpoint {
            id: PhysicalEndpointId::PrimaryInput(PortId(0)),
            anchor: source_anchor,
            allowed_exit: Facing::North,
            terminal: TerminalContract::Source {
                signal_strength: 15,
            },
        };
        let connection = connection(1, 0);
        let sinks = NonEmptyRouteSinks::new(vec![RouteSink {
            id: RoutedSinkId { route, ordinal: 0 },
            endpoint: PhysicalEndpointId::Landing(connection),
            anchor: sink_anchor,
            allowed_entry: Facing::North,
            terminal: TerminalContract::Sink {
                target: RouteTarget::Connection(connection),
                support: at(sink_anchor.x, sink_anchor.y, sink_anchor.z + 1),
                requirement: TerminalRequirement::Repeater,
            },
        }])
        .unwrap();
        let guidance = RouteGuidance {
            origin: at(0, 0, 0),
            lateral: Facing::South,
            track: lane,
            half_width: 0,
            access_half_width: 3,
            preferred_y: Some(3),
            access_y: None,
            hard: true,
            penalty_per_block: 8,
        };

        let tree = DurablePhysicalRouter
            .route_guided(
                RouteRequest {
                    id: route,
                    source,
                    sinks: &sinks,
                    reservations: &PhysicalReservations::new(),
                    limits: RouterLimits {
                        max_node_expansions: 262_144,
                        max_queue_entries: 262_144,
                    },
                },
                Some(guidance),
            )
            .expect("the lane is empty and it is the only row joining the two columns");

        assert!(
            tree.cells.iter().any(|block| block.at.z == lane),
            "the route reaches the lane it was given, not just its own access column"
        );
        for block in &tree.cells {
            let on_track = block.at.z == lane;
            let in_access = block.at.x.abs_diff(source_anchor.x) <= 3
                || block.at.x.abs_diff(sink_anchor.x) <= 3;
            assert!(
                on_track || in_access,
                "a widened box may not widen what the guidance admits: {:?}",
                block.at
            );
        }
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

        let laid = realise_branch_from_with_boundary_policy(
            source,
            2,
            &cells,
            true,
            ReservePolicy::LatestLegalCell,
        );

        assert_ne!(
            laid.blocks[0].kind,
            BlockKind::Repeater,
            "a refresh at the first suffix cell would enter from east and leave south"
        );
    }

    #[test]
    fn shared_trunk_refresh_chooses_the_last_straight_dust_and_rejects_bad_shapes() {
        let route = RouteId(41);
        let source = endpoint(route);
        let terminal = sink(route, 0, at(3, 1, 0));
        let sinks = NonEmptyRouteSinks::new(vec![terminal.clone()]).unwrap();
        let reservations = PhysicalReservations::new();
        let request = RouteRequest {
            id: route,
            source: source.clone(),
            sinks: &sinks,
            reservations: &reservations,
            limits: RouterLimits {
                max_node_expansions: 100,
                max_queue_entries: 100,
            },
        };
        let straight = vec![source.anchor, at(1, 1, 0), at(2, 1, 0), terminal.anchor];
        let states = straight
            .iter()
            .copied()
            .map(|cell| (cell, dust()))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(
            straight[..3].iter().rev().find_map(|candidate| {
                shared_trunk_refresh_direction(
                    &request,
                    *candidate,
                    &straight,
                    &[],
                    &states,
                    &reservations,
                )
            }),
            Some(Facing::East),
            "the suffix-nearest legal dust is selected deterministically"
        );

        let mut fanout = states.clone();
        fanout.insert(at(1, 1, 1), dust());
        let bent = vec![source.anchor, at(1, 1, 0), at(1, 1, 1), terminal.anchor];
        let bent_states = bent
            .iter()
            .copied()
            .map(|cell| (cell, dust()))
            .collect::<BTreeMap<_, _>>();
        let terminal_candidate_sinks =
            NonEmptyRouteSinks::new(vec![sink(route, 0, at(1, 1, 0))]).unwrap();
        let terminal_at_candidate = RouteRequest {
            id: route,
            source: source.clone(),
            sinks: &terminal_candidate_sinks,
            reservations: &reservations,
            limits: request.limits,
        };
        for (label, request, path, states) in [
            ("fanout", &request, &straight, &fanout),
            ("bend", &request, &bent, &bent_states),
            ("terminal", &terminal_at_candidate, &straight, &states),
        ] {
            assert_eq!(
                shared_trunk_refresh_direction(
                    request,
                    at(1, 1, 0),
                    path,
                    &[],
                    states,
                    &reservations,
                ),
                None,
                "{label} may not become a shared repeater"
            );
        }
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

    /// The two ends of one request, far enough apart that the cheapest route
    /// between them turns at the source's exit and reaches the sink's approach
    /// from the side -- so a forced runway has something to change.
    fn runway_fixture(route: RouteId) -> (RouteEndpoint, RouteSink) {
        let source = RouteEndpoint {
            anchor: at(6, 1, 0),
            ..endpoint(route)
        };
        (source, sink(route, 0, at(6, 1, 8)))
    }

    fn route_runway_fixture(
        route: RouteId,
        runways: ForcedTerminalRunways,
        reservations: &PhysicalReservations,
    ) -> Result<RealisedRouteTree, RouterFailure> {
        let (source, sink) = runway_fixture(route);
        let sinks = NonEmptyRouteSinks::new(vec![sink]).unwrap();
        DurablePhysicalRouter.route_with_runways(
            RouteRequest {
                id: route,
                source,
                sinks: &sinks,
                reservations,
                limits: RouterLimits {
                    max_node_expansions: 100_000,
                    max_queue_entries: 500_000,
                },
            },
            runways,
        )
    }

    #[test]
    fn a_forced_source_runway_moves_the_first_turn_off_the_exit() {
        let route = RouteId(21);
        let reservations = PhysicalReservations::new();
        let (source, _) = runway_fixture(route);
        let exit = step(source.anchor, source.allowed_exit);
        let runway = step(exit, source.allowed_exit);

        // The default contract is the prior semantics: seed rules fix the
        // first step and nothing after it, so the cheapest route turns as soon
        // as it has left -- carrying on to `runway` costs two more cells than
        // the sink is worth.
        let free = route_runway_fixture(route, ForcedTerminalRunways::default(), &reservations)
            .expect("the open fixture routes");
        let free_path = &free.branches[0].path;
        assert_eq!(free_path[0], source.anchor);
        assert_eq!(free_path[1], exit);
        assert_ne!(
            free_path[2], runway,
            "the fixture must be one whose cheapest route turns at the exit"
        );

        let forced = route_runway_fixture(
            route,
            ForcedTerminalRunways {
                source: TerminalRunway::Forced { cells: 2 },
                sinks: TerminalRunway::Free,
            },
            &reservations,
        )
        .expect("the forced prefix is routable in open space");
        let path = &forced.branches[0].path;
        assert_eq!(
            &path[..3],
            [source.anchor, exit, runway],
            "a forced source must traverse anchor -> exit -> runway before turning"
        );
        assert_ne!(path[3], runway, "the runway is traversed, not revisited");

        // Traversed, not merely permitted: the runway is realised exactly once
        // as an ordinary conductor of this tree, with at most its own floor.
        for cell in [exit, runway] {
            assert_eq!(path.iter().filter(|at| **at == cell).count(), 1);
            assert_eq!(forced.cells.iter().filter(|at| at.at == cell).count(), 1);
            assert!(forced.floors.iter().filter(|at| at.at == cell).count() <= 1);
            let laid = forced.cells.iter().find(|block| block.at == cell).unwrap();
            assert!(matches!(
                laid.state.kind,
                BlockKind::RedstoneWire | BlockKind::Repeater
            ));
        }
    }

    #[test]
    fn a_forced_sink_runway_moves_the_approach_onto_the_mirror_suffix() {
        let route = RouteId(22);
        let reservations = PhysicalReservations::new();
        let (_, sink) = runway_fixture(route);
        let approach = step(sink.anchor, sink.allowed_entry);
        let runway = step(approach, sink.allowed_entry);

        let free = route_runway_fixture(route, ForcedTerminalRunways::default(), &reservations)
            .expect("the open fixture routes");
        let free_path = &free.branches[0].path;
        assert_eq!(free_path[free_path.len() - 1], sink.anchor);
        assert_ne!(
            free_path[free_path.len() - 3],
            runway,
            "the fixture must be one whose cheapest route arrives from the side"
        );

        let forced = route_runway_fixture(
            route,
            ForcedTerminalRunways {
                source: TerminalRunway::Free,
                sinks: TerminalRunway::Forced { cells: 2 },
            },
            &reservations,
        )
        .expect("the forced suffix is routable in open space");
        let path = &forced.branches[0].path;
        assert_eq!(
            &path[path.len() - 3..],
            [runway, approach, sink.anchor],
            "a forced sink is reached through runway -> exit -> anchor"
        );
        for cell in [runway, approach] {
            assert_eq!(path.iter().filter(|at| **at == cell).count(), 1);
            assert_eq!(forced.cells.iter().filter(|at| at.at == cell).count(), 1);
        }
    }

    /// The contract does not make a runway cell exempt from the reservation
    /// map: a forced step into a cell somebody else owns has no route, and the
    /// router says so with the same typed refusal it always did rather than
    /// laying through the claim.
    #[test]
    fn an_obstructed_forced_runway_is_a_typed_no_local_route() {
        let route = RouteId(23);
        let (source, _) = runway_fixture(route);
        let runway = step(
            step(source.anchor, source.allowed_exit),
            source.allowed_exit,
        );
        let mut reservations = PhysicalReservations::new();
        // A keep-out rather than a conductor: a conductor would also take the
        // exit out of its own twelve-cell ring, and then the fixture would be
        // unroutable with or without the contract, which proves nothing.
        reservations.reserve(
            runway,
            PhysicalReservationOwner::KeepOut(7),
            PhysicalReservationKind::KeepOut,
        );

        // Without the contract the same map routes: the cheapest path never
        // wanted that cell, so this is the forced prefix being refused and not
        // the fixture being unroutable.
        assert!(
            route_runway_fixture(route, ForcedTerminalRunways::default(), &reservations).is_ok()
        );
        assert!(matches!(
            route_runway_fixture(
                route,
                ForcedTerminalRunways {
                    source: TerminalRunway::Forced { cells: 2 },
                    sinks: TerminalRunway::Free,
                },
                &reservations,
            ),
            Err(RouterFailure::NoLocalRoute { route: refused, .. }) if refused == route
        ));
    }

    /// `cells: 0` is `Free` written the long way, and the default contract is
    /// what every caller that does not ask for one gets: identical trees.
    #[test]
    fn an_empty_runway_contract_is_the_prior_route() {
        let route = RouteId(24);
        let reservations = PhysicalReservations::new();
        let baseline = route_runway_fixture(route, ForcedTerminalRunways::default(), &reservations)
            .expect("the open fixture routes");
        let zero = route_runway_fixture(
            route,
            ForcedTerminalRunways {
                source: TerminalRunway::Forced { cells: 0 },
                sinks: TerminalRunway::Forced { cells: 0 },
            },
            &reservations,
        )
        .expect("a zero-cell runway constrains nothing");
        assert_eq!(baseline, zero);

        let (source, sink) = runway_fixture(route);
        let sinks = NonEmptyRouteSinks::new(vec![sink]).unwrap();
        assert_eq!(
            baseline,
            DurablePhysicalRouter
                .route(RouteRequest {
                    id: route,
                    source,
                    sinks: &sinks,
                    reservations: &reservations,
                    limits: RouterLimits {
                        max_node_expansions: 100_000,
                        max_queue_entries: 500_000,
                    },
                })
                .unwrap(),
            "the default trait method is the prior `route`"
        );
    }

    /// Every branch of a fanout leaves through the same source runway and
    /// arrives on its own mirror suffix, including the branch that re-roots on
    /// an already laid trunk cell.
    #[test]
    fn a_forced_runway_holds_for_every_branch_of_a_fanout() {
        let route = RouteId(25);
        let source = RouteEndpoint {
            anchor: at(6, 1, 0),
            ..endpoint(route)
        };
        let exit = step(source.anchor, source.allowed_exit);
        let runway = step(exit, source.allowed_exit);
        let first = sink(route, 0, at(6, 1, 8));
        let second = sink(route, 1, at(6, 1, 12));
        let sinks = NonEmptyRouteSinks::new(vec![first.clone(), second.clone()]).unwrap();
        let reservations = PhysicalReservations::new();

        let tree = DurablePhysicalRouter
            .route_with_runways(
                RouteRequest {
                    id: route,
                    source: source.clone(),
                    sinks: &sinks,
                    reservations: &reservations,
                    limits: RouterLimits {
                        max_node_expansions: 200_000,
                        max_queue_entries: 1_000_000,
                    },
                },
                ForcedTerminalRunways {
                    source: TerminalRunway::Forced { cells: 2 },
                    sinks: TerminalRunway::Forced { cells: 2 },
                },
            )
            .expect("a forced fanout routes in open space");

        assert_eq!(tree.branches.len(), 2);
        for (branch, sink) in tree.branches.iter().zip([first, second]) {
            let path = &branch.path;
            let approach = step(sink.anchor, sink.allowed_entry);
            let sink_runway = step(approach, sink.allowed_entry);
            assert_eq!(
                &path[path.len() - 3..],
                [sink_runway, approach, sink.anchor],
                "branch {:?} must arrive on its own mirror suffix",
                branch.sink
            );
            // A re-rooted branch starts inside the trunk rather than at the
            // source, so what must hold for it is that it never steps off the
            // prefix mid-runway -- checked by walking the cells it does have.
            for pair in path.windows(2) {
                if pair[0] == source.anchor {
                    assert_eq!(pair[1], exit);
                }
                if pair[0] == exit {
                    assert_eq!(pair[1], runway);
                }
                if pair[1] == approach {
                    assert_eq!(pair[0], sink_runway);
                }
            }
        }
        // One conductor per cell across the whole tree: a shared runway is
        // shared, not laid twice.
        let mut seen = BTreeSet::new();
        assert!(tree.cells.iter().all(|block| seen.insert(block.at)));
    }

    #[test]
    fn fanout_keeps_later_sink_guarded_until_its_branch() {
        let route = RouteId(13);
        let source = endpoint(route);
        let first = sink(route, 0, at(8, 1, 0));
        let sibling = sink(route, 1, at(4, 1, 0));
        let sinks = NonEmptyRouteSinks::new(vec![first.clone(), sibling.clone()]).unwrap();
        let mut reservations = PhysicalReservations::new();
        for x in 3..=5 {
            for y in 0..=4 {
                reservations.reserve(
                    at(x, y, sibling.anchor.z),
                    PhysicalReservationOwner::Endpoint(sibling.endpoint),
                    PhysicalReservationKind::KeepOut,
                );
            }
        }
        let first_approach = step(first.anchor, Facing::West);
        for at in std::iter::once(first.anchor)
            .chain(neighbours(first.anchor))
            .chain(neighbours(first_approach))
        {
            reservations.reserve(
                at,
                PhysicalReservationOwner::Endpoint(first.endpoint),
                PhysicalReservationKind::KeepOut,
            );
        }

        let tree = DurablePhysicalRouter
            .route(RouteRequest {
                id: route,
                source,
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 100_000,
                    max_queue_entries: 200_000,
                },
            })
            .unwrap_or_else(|failure| {
                panic!("the guarded sibling must leave a detour for the first branch: {failure:?}")
            });

        assert_eq!(tree.branches.len(), 2);
        assert!(
            !tree.branches[0].path.contains(&sibling.anchor),
            "first branch crossed the still-guarded sibling anchor"
        );
        assert!(tree.branches[1].path.contains(&sibling.anchor));
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
                access_half_width: 0,
                preferred_y: None,
                access_y: None,
                hard: false,
                penalty_per_block: 2,
            }),
        );

        assert!(
            result.is_ok(),
            "the literal eight-sink segment tree must grow from its trunk without restarting every search at the source: {result:?}"
        );
    }

    #[test]
    fn torch_isolation_blocks_foreign_dust_on_support_top_sides_and_underside_only() {
        // East-facing torch: support (25,1,40), torch (26,1,40), front /
        // route anchor (27,1,40).  Mirrors `seed::reserve_torch_isolation`.
        let torch_owner = PhysicalReservationOwner::KeepOut(7);
        let mut reservations = PhysicalReservations::new();
        for cell in [at(26, 2, 40), at(25, 2, 40), at(26, 1, 39), at(26, 1, 41)] {
            reservations.reserve(cell, torch_owner, PhysicalReservationKind::MandatoryAir);
        }
        reservations.reserve(at(26, 0, 40), torch_owner, PhysicalReservationKind::KeepOut);

        let foreign = RouteId(3);
        let foreign_start = at(10, 1, 40);
        let foreign_goal = at(40, 1, 40);
        let foreign_support = at(41, 1, 40);
        let foreign_free = |cell: Anchor, reservations: &PhysicalReservations| {
            anchor_is_free_for_typed(
                foreign,
                cell,
                foreign_start,
                foreign_goal,
                foreign_support,
                reservations,
            )
        };
        let blocked = [
            at(26, 2, 40), // above the torch
            at(26, 3, 40), // dust standing on the torch's overhead cell
            at(25, 2, 40), // on top of the support
            at(25, 3, 40), // dust standing on the support's overhead cell
            at(26, 1, 39), // torch side
            at(26, 2, 39), // dust standing on a torch side cell
            at(26, 1, 41), // torch side
            at(26, 2, 41), // dust standing on a torch side cell
            at(26, 0, 40), // below the torch
        ];
        for cell in blocked {
            assert!(foreign_free(cell, &PhysicalReservations::new()), "{cell:?}");
            assert!(!foreign_free(cell, &reservations), "{cell:?}");
        }
        // The front cell and its onward runway stay open to a route that
        // starts there: the own route's exit is never blocked.
        let own = RouteId(4);
        let route_anchor = at(27, 1, 40);
        let runway = at(28, 1, 40);
        assert!(anchor_is_free_for_typed(
            own,
            runway,
            route_anchor,
            foreign_goal,
            foreign_support,
            &reservations,
        ));
        assert!(foreign_free(route_anchor, &reservations));
        assert!(foreign_free(at(27, 3, 40), &reservations));
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
    fn search_step_cost_matches_route_mode() {
        assert_eq!(search_step_cost(false, false, 1, 2, 3), 3);
        assert_eq!(search_step_cost(false, false, 3, 4, 1), 3);
        for (strict_local, seed_rules) in [(true, false), (false, true), (true, true)] {
            assert_eq!(search_step_cost(strict_local, seed_rules, 1, 2, 3), 1);
            assert_eq!(search_step_cost(strict_local, seed_rules, 3, 4, 1), 3);
        }
        assert_eq!(search_step_cost(false, false, 1, 1, 3), 1);
    }

    #[test]
    fn reserve_policies_price_the_same_fixed_path_differently() {
        let source = at(0, 1, 0);
        let cells = climbing_path(180, 12);
        let total = realise_branch_from_with_boundary_policy(
            source,
            MAX_SIGNAL_STRENGTH,
            &cells,
            false,
            ReservePolicy::TotalStairs,
        );
        let longest = realise_branch_from_with_boundary_policy(
            source,
            MAX_SIGNAL_STRENGTH,
            &cells,
            false,
            ReservePolicy::LatestLegalCell,
        );

        assert!(total.carries && longest.carries);
        assert!(
            total.repeaters > longest.repeaters,
            "total-stairs reserve must cost more on this fixed path: {} vs {}",
            total.repeaters,
            longest.repeaters
        );
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
    fn ring_reroute_uses_earliest_charged_entry_even_with_later_repeater() {
        let start = at(0, 1, 0);
        let earlier_entry = at(1, 1, 0);
        let later_repeater = at(2, 1, 0);
        let sink = at(3, 1, 0);

        assert_eq!(
            first_eligible_ring_reroute(
                &[earlier_entry, later_repeater],
                start,
                sink,
                &BTreeSet::new(),
            ),
            Some(earlier_entry),
            "ring reroutes follow charged suffix order, not repeater position"
        );
    }

    #[test]
    fn strict_policy_reroutes_an_initial_ring_and_returns_an_acyclic_tree() {
        let route = RouteId(12);
        let source = RouteEndpoint {
            anchor: at(3, 1, 0),
            allowed_exit: Facing::West,
            ..endpoint(route)
        };
        let connection = connection(12, 0);
        let typed_sink = RouteSink {
            id: RoutedSinkId { route, ordinal: 0 },
            endpoint: PhysicalEndpointId::Landing(connection),
            anchor: at(0, 1, 0),
            allowed_entry: Facing::West,
            terminal: TerminalContract::Sink {
                target: RouteTarget::Connection(connection),
                support: at(1, 1, 0),
                requirement: TerminalRequirement::Repeater,
            },
        };
        let sinks = NonEmptyRouteSinks::new(vec![typed_sink.clone()]).unwrap();
        let reservations = PhysicalReservations::new();
        let mut claims = Vec::new();

        let tree = route_strict_with_policy(
            RouteRequest {
                id: route,
                source,
                sinks: &sinks,
                reservations: &reservations,
                limits: RouterLimits {
                    max_node_expansions: 100_000,
                    max_queue_entries: 200_000,
                },
            },
            RoutingJoinPolicy::Off,
            |_| 0,
            |at, owner, kind| claims.push((at, owner, kind)),
        )
        .expect("strict local routing must reroute the initial ring");

        let states = tree
            .cells
            .iter()
            .map(|cell| (cell.at, cell.state.clone()))
            .collect();
        assert_eq!(
            ring_closed_in_typed(&states, &PhysicalReservations::new()),
            None,
            "the accepted alternate route must not leave a repeater ring"
        );
        assert_eq!(
            tree.branches[0].path[2],
            at(2, 1, -1),
            "the initial straight approach closes a ring; strict routing must take the one-reroute detour"
        );

        let branch = &tree.branches[0];
        let predecessor = branch.path[branch.path.len() - 2];
        let (_, support, _) = typed_sink.terminal.sink_parts().unwrap();
        let mut expected_reservations = PhysicalReservations::new();
        let mut expected_claims = Vec::new();
        reserve_typed_path(
            route,
            &branch.path,
            &mut expected_reservations,
            &mut |at, owner, kind| expected_claims.push((at, owner, kind)),
        );
        reserve_terminal_guard(
            branch.sink,
            predecessor,
            typed_sink.anchor,
            support,
            &mut expected_reservations,
            &mut |at, owner, kind| expected_claims.push((at, owner, kind)),
        );
        assert_eq!(
            claims, expected_claims,
            "strict reroute claims must contain only the accepted branch"
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
