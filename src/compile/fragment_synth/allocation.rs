//! Deterministic parent allocation of child physical contracts.
//!
//! From the root [`Netlist`] and its [`partition::Chunk`]s the parent fixes,
//! before any child is compiled or any route searched: a [`RegionMask`] and
//! one-cell halo per child, a [`PortalWindow`] per child boundary signal, one
//! [`Corridor`] with checked capacity, and one [`Trunk`] per boundary signal
//! carrying its single source and every sink, so fanout is represented once.
//!
//! Global layout runs north to south (increasing `z`), all coordinates
//! nonnegative: the root caller row at `z = caller_row_z` (zero when the
//! allocator places it, otherwise the caller's pinned row), the corridor, then
//! every child's halo and region side by side along `x`.  Each child is
//! compiled in its own **local frame** -- halo corner at the origin, caller row
//! at `z = 0`, region from `z = 1` -- and translated by
//! [`ChildAllocation::origin`] afterwards.
//!
//! Regions are allocation envelopes sized from the planner's own seed layout.
//! They do not clamp the child's search; the leaf compiler must refuse route
//! anchors outside its local world as well as occupied cells outside the
//! region, because out-of-bounds world writes are otherwise invisible.
//!
//! Children are ordered topologically, producers before consumers, with the
//! first gate name and then [`ChunkId`] as stable tie-breakers; signals are
//! ordered by name. Given the canonical chunks produced by
//! [`partition::partition`], nothing depends on slice, hash, or worker order.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::partition::{Chunk, ChunkId};
use crate::compile::geometry::Anchor;
use crate::compile::planner::{
    self, LowerBound, PinRefusal, PlannerError, PortPin, PortPlacements, PortRole,
};
use crate::compile::topology::SignalPolarity;
use crate::compile::Netlist;
use crate::redstone::simulator::position::Position;
use crate::redstone::world::block::Facing;

/// Caller-cell pitch along a face: face-neighbour halos of two portals never
/// share a cell.
pub(crate) const PORTAL_PITCH: i32 = 3;
/// Corridor depth per unit of capacity.
pub(crate) const LANE_PITCH: i32 = 2;
/// Rows reserved at each corridor end for portal access.
pub(crate) const ACCESS_BAND_ROWS: i32 = 2;
const ACCESS_BAND_UNITS: u32 = 2;
/// How far a guided trunk may stray from its access column when it crosses
/// the access band, and the width of the band window each endpoint releases.
///
/// Allocation colours corridor lanes with it and
/// [`compose`](crate::compile::fragment_synth::parent::compose) releases the
/// band with it, so it lives here once: a lane colouring that padded spans by
/// a different width than the band a trunk actually opens would let two
/// same-lane trunks reach into each other's access columns.
pub(crate) const ACCESS_HALF_WIDTH: u32 = 3;
/// Minimum depth needed for non-overlapping terminal access bands.
pub(crate) const MIN_CORRIDOR_DEPTH: i32 = 6;
const MIN_CORRIDOR_UNITS: u32 = ((MIN_CORRIDOR_DEPTH + LANE_PITCH - 1) / LANE_PITCH) as u32;
/// Router ceiling plus its one-cell staircase clearance.
const REGION_TOP: i32 = 8;
/// The plane the planner places ports on.
const PORTAL_Y: i32 = 1;
/// Where every child's region starts in its own frame: column `x = 0` and
/// row `z = 0` are the parent's halo and caller row.
const LOCAL_REGION_MIN: Anchor = Anchor { x: 1, y: 0, z: 1 };
/// Rows of parent-owned space between the southernmost cell a literal root pin
/// reaches and the body this plan builds behind it.
///
/// One row would do for geometry; two leave the body's own caller row free of
/// the pins' handover hardware, so the body is laid out exactly as it would be
/// for an unpinned root.
const ROOT_ACCESS_GAP: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocationLimits {
    pub delay_budget_ticks: u32,
    /// Trunks the corridor may carry.
    pub corridor_capacity: u32,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum AllocationError {
    #[error("allocation needs at least one child chunk")]
    NoChildren,
    #[error("delay budget must be at least one tick")]
    ZeroDelayBudget,
    #[error("corridor capacity must be at least one trunk")]
    ZeroCapacity,
    #[error("corridor holds {capacity} trunks but {needed} boundary signals need one each")]
    CorridorCapacityExceeded { needed: u32, capacity: u32 },
    #[error("chunk {chunk:?} lists {signal} as both a boundary input and output")]
    BoundaryOverlap { chunk: ChunkId, signal: String },
    #[error("signal {signal} has no driver: neither a root input nor any child output")]
    MissingDriver { signal: String },
    #[error("signal {signal} has more than one driver")]
    DuplicateDriver { signal: String },
    #[error("signal {signal} is driven but nothing consumes it")]
    UnconsumedSignal { signal: String },
    #[error("chunk {chunk:?} has no seed layout: {error:?}")]
    Seed { chunk: ChunkId, error: PlannerError },
    #[error("layout coordinates overflow i32")]
    CoordinateOverflow,
    #[error("chunk dependency graph is cyclic")]
    ChunkDependencyCycle,
    #[error("root port {signal} is not pinned in the supplied parent contract")]
    MissingRootPort { signal: String },
    /// The planner refused the root's pins for a reason it did not pin on
    /// one port. Its pin validation names a port for every refusal it makes
    /// today, so this is the arm that keeps a new refusal typed rather than
    /// dropped.
    #[error("the planner refused the root pins: {error}")]
    RootPinsRefused { error: PlannerError },
    #[error("child {chunk:?} could not be sized: {error}")]
    ChildExtent { chunk: ChunkId, error: String },
    /// The planner refused to lay out `chunk` on the ports it was just pinned,
    /// so no extent exists for it.  Named by chunk so the caller that owns the
    /// recursion can repair that one child instead of failing the allocation.
    #[error("child {chunk:?} refused its contract while being sized: {error}")]
    ChildUnplannable { chunk: ChunkId, error: String },
    #[error("root port {signal} is pinned at {at:?} facing {toward:?}, which {reason}")]
    UnsupportedRootPin {
        signal: String,
        at: Anchor,
        toward: Facing,
        reason: &'static str,
    },
    #[error("root port {port} at {at:?} is invalid: {refusal}")]
    InvalidRootPort {
        port: String,
        at: Anchor,
        refusal: PinRefusal,
    },
    #[error("root ports {first} and {second} both need cell {at:?}")]
    RootPinCollision {
        first: String,
        second: String,
        at: Anchor,
    },
    #[error(
        "root ports {first} at {first_at:?} and {second} at {second_at:?} are {apart} cells apart; \
         a landed port needs {minimum}"
    )]
    RootPinsTooClose {
        first: String,
        first_at: Anchor,
        second: String,
        second_at: Anchor,
        apart: i64,
        minimum: i64,
    },
    #[error("trunk {signal} has no endpoint to span")]
    TrunkWithoutEnds { signal: String },
}

/// An axis-aligned inclusive box of cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prism {
    pub min: Anchor,
    pub max: Anchor,
}

impl Prism {
    pub fn contains(&self, at: Anchor) -> bool {
        (self.min.x..=self.max.x).contains(&at.x)
            && (self.min.y..=self.max.y).contains(&at.y)
            && (self.min.z..=self.max.z).contains(&at.z)
    }
}

/// The cells a child may build in.  Only a prism exists today; consumers ask
/// [`RegionMask::contains`] and never assume a box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionMask {
    Prism(Prism),
}

impl RegionMask {
    pub fn contains(&self, at: Anchor) -> bool {
        match self {
            RegionMask::Prism(prism) => prism.contains(at),
        }
    }

    pub fn bounds(&self) -> Prism {
        match self {
            RegionMask::Prism(prism) => *prism,
        }
    }
}

/// Signal contract shared by every portal in a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalContract {
    pub polarity: SignalPolarity,
    pub strength: u8,
    pub delay_budget_ticks: u32,
}

/// One child boundary signal at the child's north face, in global
/// coordinates.  `pin.at` is the parent-owned caller cell in the halo;
/// [`PortalWindow::handover`] is the child's cell inside its region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalWindow {
    pub signal: String,
    /// The child's view: `Input` enters the child, `Output` leaves it.
    pub role: PortRole,
    pub pin: PortPin,
}

impl PortalWindow {
    pub fn handover(&self) -> Anchor {
        self.pin.handover(self.role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildAllocation {
    pub chunk: ChunkId,
    /// Global position of the child's local origin: the halo's minimum
    /// corner.  `global = local + origin`.
    pub origin: Anchor,
    /// Global allocation envelope.
    pub region: RegionMask,
    /// Region plus its parent-owned one-cell halo, global.  No halo lies
    /// below `y = 0`: the world floor is the boundary there.
    pub halo: Prism,
    /// Boundary inputs then outputs, each by signal name.
    pub portals: Vec<PortalWindow>,
}

impl ChildAllocation {
    pub fn in_halo(&self, at: Anchor) -> bool {
        self.halo.contains(at) && !self.region.contains(at)
    }

    pub fn to_local(&self, global: Anchor) -> Anchor {
        Anchor {
            x: global.x - self.origin.x,
            y: global.y - self.origin.y,
            z: global.z - self.origin.z,
        }
    }

    pub fn to_global(&self, local: Anchor) -> Anchor {
        Anchor {
            x: local.x + self.origin.x,
            y: local.y + self.origin.y,
            z: local.z + self.origin.z,
        }
    }

    /// The envelope in the child's own frame.
    pub fn local_region(&self) -> Prism {
        let bounds = self.region.bounds();
        Prism {
            min: self.to_local(bounds.min),
            max: self.to_local(bounds.max),
        }
    }

    /// Every boundary signal pinned in the child's local frame, for its
    /// compiler, under the contract that the caller row and halo column the
    /// pins sit on are this parent's: the child's planner may lift nothing
    /// onto them and route nothing through them.
    pub fn port_placements(&self) -> PortPlacements {
        let mut placements = PortPlacements::default();
        for portal in &self.portals {
            placements.pin(
                portal.signal.clone(),
                self.to_local(portal.pin.at),
                portal.pin.toward,
            );
        }
        let region = self.local_region();
        placements.bound_below(LowerBound {
            x: region.min.x,
            z: region.min.z,
        });
        placements
    }

    pub fn portal(&self, signal: &str, role: PortRole) -> Option<&PortalWindow> {
        self.portals
            .iter()
            .find(|portal| portal.signal == signal && portal.role == role)
    }
}

/// Parent-owned space between the root caller row and the children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Corridor {
    pub region: Prism,
    pub capacity: u32,
}

impl Corridor {
    pub fn access_bands(&self) -> [i32; 4] {
        [
            self.region.min.z,
            self.region.min.z.saturating_add(1),
            self.region.max.z.saturating_sub(1),
            self.region.max.z,
        ]
    }

    pub fn lane_band(&self) -> Option<(i32, i32)> {
        let floor = self.region.min.z.saturating_add(ACCESS_BAND_ROWS);
        let ceiling = self.region.max.z.saturating_sub(ACCESS_BAND_ROWS);
        (floor <= ceiling).then_some((floor, ceiling))
    }

    pub fn lane_capacity(&self) -> u32 {
        let Some((floor, ceiling)) = self.lane_band() else {
            return 0;
        };
        u32::try_from((i64::from(ceiling) - i64::from(floor)) / i64::from(LANE_PITCH))
            .unwrap_or(u32::MAX)
            .saturating_add(1)
    }

    pub fn lane_track(&self, index: u32) -> Option<i32> {
        let (floor, ceiling) = self.lane_band()?;
        let track = i64::from(ceiling) - i64::from(index) * i64::from(LANE_PITCH);
        (track >= i64::from(floor)).then_some(track as i32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrunkOwner {
    Root,
    Child(ChunkId),
}

/// One end of a trunk from its owner's view: a root input or child output is
/// a source, a root output or child input a sink.  Global coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrunkEnd {
    pub owner: TrunkOwner,
    pub pin: PortPin,
    pub role: PortRole,
}

impl TrunkEnd {
    pub fn handover(&self) -> Anchor {
        self.pin.handover(self.role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trunk {
    pub signal: String,
    pub source: TrunkEnd,
    /// Child sinks by `ChunkId`, then the root output if declared.
    pub sinks: Vec<TrunkEnd>,
    /// The corridor lane this trunk crosses on, from the plan's interval
    /// colouring: trunks sharing a lane have disjoint padded `x` spans, so a
    /// lane carries several trunks that can never meet.  Index into
    /// [`Corridor::lane_track`], not a position in [`AllocationPlan::trunks`].
    pub lane: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPort {
    pub signal: String,
    pub role: PortRole,
    pub pin: PortPin,
}

/// How this plan's root ports reach the body it builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootAccess {
    /// Every root port sits on one caller row with its handover on the row
    /// behind it, so the corridor's own access band is all the approach any
    /// root trunk needs.  Unpinned roots and every nested child are this.
    CallerRow,
    /// The caller pinned ports this contract cannot fold onto one row -- more
    /// than one row, or facings other than north and south.  The pins are kept
    /// exactly where the caller put them and `region` is the parent-owned
    /// space, north of the body's caller row, that joins them to the corridor.
    Landed { region: Prism },
}

/// Where the root ports are and how the body sits behind them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPlacement {
    /// The row the body starts behind: everything at or in front of it is the
    /// caller's under [`RootAccess::CallerRow`], and the last parent-owned row
    /// of the access region under [`RootAccess::Landed`].  The corridor always
    /// starts on the row behind it.
    pub caller_row_z: i32,
    pub access: RootAccess,
}

impl RootPlacement {
    /// The access region a landed root owns, if it landed.
    pub fn landed_region(&self) -> Option<Prism> {
        match &self.access {
            RootAccess::CallerRow => None,
            RootAccess::Landed { region } => Some(*region),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationPlan {
    pub contract: SignalContract,
    /// Where the root ports sit and how they reach the body.
    pub root_placement: RootPlacement,
    /// Declared order, inputs then outputs, unused inputs included.
    pub root_ports: Vec<RootPort>,
    /// Producers before consumers, ties broken by the chunk's first gate
    /// name and then its `ChunkId`, so the order is a property of the netlist
    /// and not of how the chunk list arrived.
    pub children: Vec<ChildAllocation>,
    pub corridor: Corridor,
    /// By signal name; one per boundary signal.
    pub trunks: Vec<Trunk>,
}

impl AllocationPlan {
    /// The far corner of everything this plan places, in its own frame: the
    /// corridor and every child halo.
    ///
    /// [`compose`](crate::compile::fragment_synth::parent::compose) sizes its
    /// world to exactly this plus one, which is why it is also what an
    /// enclosing parent has to allocate for a child that will run this plan.
    pub fn local_extent(&self) -> Anchor {
        let far = self.children.iter().map(|child| child.halo.max).fold(
            self.corridor.region.max,
            |far, at| Anchor {
                x: far.x.max(at.x),
                y: far.y.max(at.y),
                z: far.z.max(at.z),
            },
        );
        // A landed root's pins and the access space reaching them stand north
        // of the corridor but can reach further east than anything behind it.
        match self.root_placement.landed_region() {
            None => far,
            Some(region) => Anchor {
                x: far.x.max(region.max.x),
                y: far.y.max(region.max.y),
                z: far.z.max(region.max.z),
            },
        }
    }

    /// The row the body starts behind.
    pub fn caller_row_z(&self) -> i32 {
        self.root_placement.caller_row_z
    }

    pub fn child(&self, chunk: &ChunkId) -> Option<&ChildAllocation> {
        self.children.iter().find(|child| &child.chunk == chunk)
    }

    pub fn trunk(&self, signal: &str) -> Option<&Trunk> {
        self.trunks.iter().find(|trunk| trunk.signal == signal)
    }
}

fn add(a: i32, b: i32) -> Result<i32, AllocationError> {
    a.checked_add(b).ok_or(AllocationError::CoordinateOverflow)
}

fn mul(a: i32, b: i32) -> Result<i32, AllocationError> {
    a.checked_mul(b).ok_or(AllocationError::CoordinateOverflow)
}

fn count(n: usize) -> Result<i32, AllocationError> {
    i32::try_from(n).map_err(|_| AllocationError::CoordinateOverflow)
}

/// One trunk's padded `x` interval across the corridor: every end it has to
/// reach, widened by the access column each of those ends opens plus one cell
/// of clearance, so two trunks with disjoint spans cannot touch even at their
/// widest.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrunkSpan<'a> {
    left: i64,
    right: i64,
    signal: &'a str,
}

impl<'a> TrunkSpan<'a> {
    /// The padded span of `xs`, or the typed refusal that a trunk with no end
    /// to reach cannot be coloured -- an empty span would silently share every
    /// lane with everything.
    fn padded(signal: &'a str, xs: &[i32]) -> Result<Self, AllocationError> {
        let pad = i64::from(ACCESS_HALF_WIDTH) + 1;
        let (min, max) = xs
            .iter()
            .map(|x| i64::from(*x))
            .fold(None, |bounds: Option<(i64, i64)>, x| match bounds {
                None => Some((x, x)),
                Some((lo, hi)) => Some((lo.min(x), hi.max(x))),
            })
            .ok_or_else(|| AllocationError::TrunkWithoutEnds {
                signal: signal.to_owned(),
            })?;
        Ok(Self {
            left: min - pad,
            right: max + pad,
            signal,
        })
    }
}

/// Colour the spans into corridor lanes: sort by `(left, right, signal)`, then
/// give each span the lowest lane whose last span ends strictly before this one
/// begins, opening a new lane when none does.
///
/// This is interval-graph colouring, so it uses the fewest lanes any assignment
/// could, and sorting by a total order on the spans themselves makes the result
/// a property of the plan rather than of the order signals arrived in.
fn colour_lanes<'a>(spans: &mut [TrunkSpan<'a>]) -> (BTreeMap<&'a str, u32>, u32) {
    spans.sort_by(|a, b| {
        a.left
            .cmp(&b.left)
            .then(a.right.cmp(&b.right))
            .then(a.signal.cmp(b.signal))
    });
    let mut last_right: Vec<i64> = Vec::new();
    let mut lanes = BTreeMap::new();
    for span in spans.iter() {
        let lane = match last_right.iter().position(|end| *end < span.left) {
            Some(lane) => lane,
            None => {
                last_right.push(i64::MIN);
                last_right.len() - 1
            }
        };
        last_right[lane] = span.right;
        lanes.insert(span.signal, lane as u32);
    }
    (lanes, last_right.len() as u32)
}

/// Where a root's ports are, and the parent-owned space they need.
///
/// A caller that pinned ports this contract can fold onto one caller row gets
/// exactly the layout it has always got -- [`RootAccess::CallerRow`], the
/// corridor one row behind the pins, nothing else built or reserved.  A caller
/// whose pins this contract cannot fold that way -- more than one row, or a
/// facing other than north and south -- keeps its pins exactly where it put
/// them, and the body moves south of all of them behind a parent-owned access
/// region.
///
/// Only a root is ever landed.  A nested child is pinned by its own parent on
/// the row that parent chose, so every child is [`RootAccess::CallerRow`] and
/// nothing about a nested contract changes.
pub fn root_placement(
    root: &Netlist,
    pins: Option<&PortPlacements>,
) -> Result<RootPlacement, AllocationError> {
    let Some(pins) = pins else {
        return Ok(RootPlacement {
            caller_row_z: 0,
            access: RootAccess::CallerRow,
        });
    };
    // Undeclared and missing ports are refused the same way whichever access
    // the pins turn out to need, so this runs before either branch.
    let declared = declared_pins(root, pins)?;
    placement_of(&declared)
}

/// The placement one list of pinned ports gets: their shared caller row when
/// they fold onto one, otherwise landed behind them.
///
/// The one rule [`root_placement`] and [`normalise_root_pins`] both answer
/// to, so a port the normaliser places is accepted by exactly the check the
/// complete set is then held to.
fn placement_of(
    declared: &[(String, PortRole, PortPin)],
) -> Result<RootPlacement, AllocationError> {
    if let Ok(caller_row_z) = pin_row(declared) {
        return Ok(RootPlacement {
            caller_row_z,
            access: RootAccess::CallerRow,
        });
    }
    landed(declared)
}

/// The pins a root is compiled on: every supplied pin exactly as supplied,
/// plus a deterministic cell for every declared port the caller left out.
///
/// `None` and an empty set are the unpinned root, and stay `None` so the
/// unpinned shapes are reached exactly as before. A set that pins every
/// declared port is handed back as it is, with nothing checked here beyond
/// that its names are declared: [`root_placement`] validates it downstream,
/// exactly as it did before partial sets existed, and the planner's own pin
/// validation runs only where the planner runs. A partial set is checked
/// here, twice. First the supplied pins on their own, against the contract's
/// rule through [`placement_of`] and the planner's through
/// [`planner::validate_port_placements`], which is what knows the caller's
/// [`LowerBound`]; a refusal at this stage names a pin the caller supplied.
/// Then the completed set, against the same two checks, so nothing leaves
/// here that the shapes downstream would refuse; a refusal at this stage may
/// name a port this function placed, or, for a planner refusal that names no
/// port, nothing at all ([`AllocationError::RootPinsRefused`]).
///
/// Completion is one walk, not a search. Missing ports go in declaration
/// order along the supplied row (the northmost supplied row, when the pins do
/// not share one), each one portal pitch east of the last, starting one pitch
/// past every cell a supplied pin's footprint reaches and past the bound's
/// own column, on the `1 + k * PORTAL_PITCH` grid the caller-row rule lays
/// unpinned ports on. Every step is checked arithmetic, and only the cells
/// actually used are computed, so a caller whose pins leave no room east of
/// them is refused as [`AllocationError::CoordinateOverflow`] rather than
/// searched for, while a final port that lands exactly on `i32::MAX` is kept.
pub fn normalise_root_pins(
    root: &Netlist,
    pins: Option<&PortPlacements>,
) -> Result<Option<PortPlacements>, AllocationError> {
    let Some(pins) = pins.filter(|pins| !pins.is_empty()) else {
        return Ok(None);
    };
    refuse_undeclared(root, pins)?;
    let declared = declared_ports(root);
    let supplied = declared
        .iter()
        .filter_map(|(signal, role)| pins.get(signal).map(|pin| (signal.clone(), *role, pin)))
        .collect::<Vec<_>>();
    if supplied.len() == declared.len() {
        return Ok(Some(pins.clone()));
    }
    // The supplied pins on their own: the same refusals the complete set
    // would get, before any cell is chosen around them.
    let placement = placement_of(&supplied)?;
    planner::validate_port_placements(root, pins).map_err(pin_refusal)?;
    let row = match placement {
        RootPlacement {
            caller_row_z,
            access: RootAccess::CallerRow,
        } => caller_row_z,
        RootPlacement {
            access: RootAccess::Landed { .. },
            ..
        } => supplied
            .iter()
            .map(|(_, _, pin)| pin.at.z)
            .min()
            .expect("a partial set has at least one pin"),
    };
    // East of everything the caller's pins reach and of the caller's own
    // column, on the pitch grid.
    let east = supplied
        .iter()
        .flat_map(|(_, role, pin)| port_footprint(pin, *role))
        .map(|at| at.x)
        .chain(pins.lower_bound().map(|bound| bound.x))
        .max()
        .expect("a partial set has at least one pin");
    let first = align_to_pitch(add(east, PORTAL_PITCH)?)?;
    let mut completed = pins.clone();
    let mut all = supplied.clone();
    let missing = declared
        .iter()
        .filter(|(signal, _)| pins.get(signal).is_none());
    // `row_cell` computes each slot from `first`, so the cell after the last
    // port is never formed and a last port on `i32::MAX` is not an overflow.
    for (slot, (signal, role)) in missing.enumerate() {
        let pin = north_pin(row_cell(first, slot, row)?, *role);
        completed.pin(signal.clone(), pin.at, pin.toward);
        all.push((signal.clone(), *role, pin));
    }
    all.sort_by_key(|(signal, _, _)| {
        declared
            .iter()
            .position(|(declared, _)| declared == signal)
            .expect("every completed pin names a declared port")
    });
    placement_of(&all)?;
    planner::validate_port_placements(root, &completed).map_err(pin_refusal)?;
    Ok(Some(completed))
}

/// The smallest `x` at or after `x` on the `1 + k * PORTAL_PITCH` grid.
///
/// The offset is taken from `x`'s own residue, which is bounded by the pitch,
/// so the only arithmetic that can overflow is the final step up, and that is
/// checked.
fn align_to_pitch(x: i32) -> Result<i32, AllocationError> {
    let offset = (1 - x.rem_euclid(PORTAL_PITCH)).rem_euclid(PORTAL_PITCH);
    add(x, offset)
}

/// The planner's own refusal of a pin, as this module's.
fn pin_refusal(error: PlannerError) -> AllocationError {
    match error {
        PlannerError::InvalidPortPin { port, at, refusal } => {
            AllocationError::InvalidRootPort { port, at, refusal }
        }
        other => AllocationError::RootPinsRefused { error: other },
    }
}
/// Every declared port with its role, inputs then outputs, in declaration
/// order.
fn declared_ports(root: &Netlist) -> Vec<(String, PortRole)> {
    root.inputs
        .iter()
        .map(|signal| (signal.clone(), PortRole::Input))
        .chain(
            root.outputs
                .iter()
                .map(|signal| (signal.clone(), PortRole::Output)),
        )
        .collect()
}

/// A pin naming no declared port is refused by type.
fn refuse_undeclared(root: &Netlist, pins: &PortPlacements) -> Result<(), AllocationError> {
    for (port, pin) in pins.iter() {
        if !root
            .inputs
            .iter()
            .chain(&root.outputs)
            .any(|name| name == port)
        {
            return Err(AllocationError::InvalidRootPort {
                port: port.clone(),
                at: pin.at,
                refusal: PinRefusal::UndeclaredPort,
            });
        }
    }
    Ok(())
}

/// Every declared port's pin, in declaration order, with undeclared pins and
/// missing ports refused by type.
fn declared_pins(
    root: &Netlist,
    pins: &PortPlacements,
) -> Result<Vec<(String, PortRole, PortPin)>, AllocationError> {
    refuse_undeclared(root, pins)?;
    declared_ports(root)
        .into_iter()
        .map(|(signal, role)| {
            let pin = pins
                .get(&signal)
                .ok_or_else(|| AllocationError::MissingRootPort {
                    signal: signal.clone(),
                })?;
            Ok((signal, role, pin))
        })
        .collect()
}

/// Every cell one literal root port owns or the router must use for it.
///
/// The caller's own cell, the handover this contract builds in, REDA's first
/// net cell -- and, for an input, the one cell past that the search is obliged
/// to leave through, because a source endpoint may only exit along its own
/// facing.  Leaving that cell out of the geometry is how a landed input ends
/// up routing outside the space the parent reserved for it.
fn port_footprint(pin: &PortPin, role: PortRole) -> Vec<Anchor> {
    let mut cells = vec![pin.at, pin.handover(role), pin.net_cell(role)];
    if role == PortRole::Input {
        // `net_cell` is where the route starts; this is where it must go next.
        cells.push(step_toward(pin.net_cell(role), pin.toward));
    }
    cells
}

/// `at` one cell along `facing`.
fn step_toward(at: Anchor, facing: Facing) -> Anchor {
    let next = Position::new(at.x, at.y, at.z).offset(facing);
    Anchor {
        x: next.x,
        y: next.y,
        z: next.z,
    }
}

/// How far apart two landed ports' footprints must stay.
///
/// The same pitch a caller row demands between its ports, measured in the
/// plane rather than along one row: two cells of clear space either side of
/// every cell a port owns, so no port's dust, runway or guard ring can reach
/// another's.
const LANDED_PIN_PITCH: i64 = PORTAL_PITCH as i64;

/// Chebyshev distance: the number of cells of clear space between two port
/// cells, whichever way they are offset. Ports on one layer measure in the
/// plane exactly as before; ports stacked in one column measure their height
/// apart, not zero.
pub(crate) fn plane_apart(a: Anchor, b: Anchor) -> i64 {
    let dx = (i64::from(a.x) - i64::from(b.x)).abs();
    let dy = (i64::from(a.y) - i64::from(b.y)).abs();
    let dz = (i64::from(a.z) - i64::from(b.z)).abs();
    dx.max(dy).max(dz)
}

/// Keep literal pins and put the body south of all of them.
fn landed(declared: &[(String, PortRole, PortPin)]) -> Result<RootPlacement, AllocationError> {
    // A netlist with no declared port names no row and constrains nothing, so
    // it never needed landing. `pin_row` answers it first and this is
    // unreachable through `root_placement`; it stands so the bounds below can
    // assume at least one cell.
    if declared.is_empty() {
        return Ok(RootPlacement {
            caller_row_z: 0,
            access: RootAccess::CallerRow,
        });
    }
    // Every cell a port owns or the router must use for it. The body clears
    // all of them, no two ports want the same one, and no two ports stand
    // close enough for their hardware to reach each other.
    let mut claimed: BTreeMap<Anchor, String> = BTreeMap::new();
    let mut footprints: Vec<(&String, Vec<Anchor>)> = Vec::with_capacity(declared.len());
    let mut south = i32::MIN;
    for (signal, role, pin) in declared {
        let refuse = |reason| AllocationError::UnsupportedRootPin {
            signal: signal.clone(),
            at: pin.at,
            toward: pin.toward,
            reason,
        };
        if matches!(pin.toward, Facing::Up | Facing::Down) {
            return Err(refuse("faces out of the plane ports are placed on"));
        }
        // Any height a port's own hardware can stand at: the handover's
        // support is one below the pin, and the world floor is y = 0.
        if pin.at.y < PORTAL_Y {
            return Err(refuse("is below the plane ports are placed on"));
        }
        if pin.at.x < 1 {
            return Err(refuse("is in the parent-owned x = 0 column"));
        }
        if pin.at.z < 0 {
            return Err(refuse("is behind the world floor"));
        }
        let footprint = port_footprint(pin, *role);
        for at in &footprint {
            if at.x < 1 || at.z < 0 || at.y < 0 {
                return Err(refuse("reaches out of the world the parent owns"));
            }
            // `add` is the overflow check: a pin near i32::MAX cannot have a
            // body placed behind it.
            add(at.z, ROOT_ACCESS_GAP)?;
            if let Some(first) = claimed.insert(*at, signal.clone()) {
                if &first != signal {
                    return Err(AllocationError::RootPinCollision {
                        first,
                        second: signal.clone(),
                        at: *at,
                    });
                }
                return Err(refuse("needs one of its own cells twice"));
            }
            south = south.max(at.z);
        }
        footprints.push((signal, footprint));
    }
    // Spacing, in the plane rather than along one row: the caller-row contract
    // demands a portal pitch between ports and a landed one demands the same,
    // measured every way a landed port can be offset. Without it two pins on
    // different rows can stand one cell apart, where their source dust and
    // their guard rings share cells and the signals couple.
    for (index, (first, left)) in footprints.iter().enumerate() {
        for (second, right) in &footprints[index + 1..] {
            let closest = left
                .iter()
                .flat_map(|a| right.iter().map(move |b| (plane_apart(*a, *b), *a, *b)))
                .min_by_key(|(apart, _, _)| *apart)
                .expect("every port has a footprint");
            if closest.0 < LANDED_PIN_PITCH {
                return Err(AllocationError::RootPinsTooClose {
                    first: (*first).clone(),
                    first_at: closest.1,
                    second: (*second).clone(),
                    second_at: closest.2,
                    apart: closest.0,
                    minimum: LANDED_PIN_PITCH,
                });
            }
        }
    }
    let caller_row_z = add(south, ROOT_ACCESS_GAP)?;
    let north = claimed
        .keys()
        .map(|at| at.z)
        .min()
        .expect("a declared port");
    let max_x = claimed
        .keys()
        .map(|at| at.x)
        .max()
        .expect("a declared port");
    let high = claimed
        .keys()
        .map(|at| at.y)
        .max()
        .expect("a declared port");
    Ok(RootPlacement {
        caller_row_z,
        access: RootAccess::Landed {
            region: Prism {
                min: Anchor {
                    x: 1,
                    y: 0,
                    z: north,
                },
                max: Anchor {
                    x: max_x,
                    // High enough to reach the highest port's own layer.
                    y: REGION_TOP.max(add(high, 2)?),
                    z: caller_row_z,
                },
            },
        },
    })
}

/// Move one child, whole, `delta` cells south.
///
/// Origin, region, halo and every portal pin shift together: a child whose
/// halo moved but whose portals did not would hand the parent a pin outside
/// the child it belongs to.
fn shift_child_z(child: &mut ChildAllocation, delta: i32) -> Result<(), AllocationError> {
    child.origin.z = add(child.origin.z, delta)?;
    match &mut child.region {
        RegionMask::Prism(prism) => {
            prism.min.z = add(prism.min.z, delta)?;
            prism.max.z = add(prism.max.z, delta)?;
        }
    }
    child.halo.min.z = add(child.halo.min.z, delta)?;
    child.halo.max.z = add(child.halo.max.z, delta)?;
    for portal in &mut child.portals {
        portal.pin.at.z = add(portal.pin.at.z, delta)?;
    }
    Ok(())
}

/// Corridor depth in lane-sized units: the lanes the colouring actually used
/// plus the access bands at either end, with a minimum that keeps terminal
/// runways disjoint.
///
/// Capacity is an admission limit on boundary signals, not a depth: a caller
/// that allows a hundred trunks and uses two gets a corridor for two.
fn corridor_depth_units(lanes_used: u32) -> u32 {
    lanes_used
        .saturating_add(ACCESS_BAND_UNITS)
        .max(MIN_CORRIDOR_UNITS)
}

/// The caller row a parent contract can honour exactly, or the typed refusal
/// that says why it cannot.
///
/// A parent contract is one row of caller cells with the corridor immediately
/// behind it, so the geometry a pin has to agree with is fixed: every root
/// port on one shared `z`, at [`PORTAL_Y`], clear of the parent-owned `x = 0`
/// column, [`PORTAL_PITCH`] apart so no two handover halos share a cell, and
/// facing so that its handover lands in the corridor rather than in front of
/// the caller.
///
/// A pin outside that is refused, never moved. A pin is a specification; a
/// relocated pin is a different circuit wearing the caller's names, and the
/// caller has no way to notice. The refusal names the cell and the reason so
/// the caller can re-pin or route the case elsewhere.
pub fn root_pin_row(root: &Netlist, pins: &PortPlacements) -> Result<i32, AllocationError> {
    pin_row(&declared_pins(root, pins)?)
}

/// [`root_pin_row`] over pins already checked against the declaration.
fn pin_row(declared: &[(String, PortRole, PortPin)]) -> Result<i32, AllocationError> {
    let mut row: Option<i32> = None;
    let mut taken: Vec<i32> = Vec::new();
    for (signal, role, pin) in declared {
        let refuse = |reason| AllocationError::UnsupportedRootPin {
            signal: signal.clone(),
            at: pin.at,
            toward: pin.toward,
            reason,
        };
        if pin.at.y != PORTAL_Y {
            return Err(refuse("is not on the plane ports are placed on"));
        }
        if pin.at.x < 1 {
            return Err(refuse("is in the parent-owned x = 0 column"));
        }
        if pin.at.z < 0 {
            return Err(refuse("is behind the world floor"));
        }
        let handover = pin.handover(*role);
        if handover
            != (Anchor {
                z: add(pin.at.z, 1)?,
                ..pin.at
            })
        {
            return Err(refuse(
                "hands over somewhere other than the corridor behind it",
            ));
        }
        match row {
            Some(row) if row != pin.at.z => {
                return Err(refuse("is not on the same caller row as the other ports"))
            }
            Some(_) => {}
            None => row = Some(pin.at.z),
        }
        if taken
            .iter()
            .any(|used| (i64::from(*used) - i64::from(pin.at.x)).abs() < i64::from(PORTAL_PITCH))
        {
            return Err(refuse("is closer than one portal pitch to another port"));
        }
        taken.push(pin.at.x);
    }
    // A netlist with no declared port names no row, and constrains nothing.
    Ok(row.unwrap_or(0))
}

/// A port on a north-facing row: an input travels south into REDA, an output
/// travels north out of it, so both handovers land one cell south of `at`.
fn north_pin(at: Anchor, role: PortRole) -> PortPin {
    let toward = match role {
        PortRole::Input => Facing::South,
        PortRole::Output => Facing::North,
    };
    PortPin { at, toward }
}

/// Caller cell `slot` along a row: `first`, then every [`PORTAL_PITCH`].
fn row_cell(first: i32, slot: usize, z: i32) -> Result<Anchor, AllocationError> {
    Ok(Anchor {
        x: add(first, mul(count(slot)?, PORTAL_PITCH)?)?,
        y: PORTAL_Y,
        z,
    })
}

/// One boundary signal's single source and every sink that reads it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SignalUse {
    pub sources: Vec<TrunkOwner>,
    pub child_sinks: BTreeSet<ChunkId>,
    pub root_sink: bool,
}

/// One child's contribution to the connectivity of a parent contract.
///
/// Deliberately the boundary lists rather than a whole [`Chunk`]: a packed
/// node knows its children only as certified artifacts, and the connectivity
/// of a parent has never depended on anything else.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ChildBoundary<'a> {
    pub chunk: &'a ChunkId,
    pub inputs: &'a [String],
    pub outputs: &'a [String],
}

/// Every boundary signal of a parent contract, with its driver and its sinks.
///
/// The one place connectivity is derived from a root interface and a set of
/// child boundaries.  Children are read in [`ChunkId`] order and the result is
/// keyed by signal name, so neither the caller's order nor the map's insertion
/// order can reach the answer.  Every signal is validated here: exactly one
/// driver, and at least one reader.
pub(crate) fn boundary_signal_uses(
    root: &Netlist,
    children: &[ChildBoundary<'_>],
) -> Result<BTreeMap<String, SignalUse>, AllocationError> {
    let ordered: BTreeMap<&ChunkId, &ChildBoundary<'_>> =
        children.iter().map(|child| (child.chunk, child)).collect();

    let mut uses: BTreeMap<String, SignalUse> = BTreeMap::new();
    for signal in &root.inputs {
        uses.entry(signal.clone())
            .or_default()
            .sources
            .push(TrunkOwner::Root);
    }
    for child in ordered.values() {
        for signal in child.outputs {
            if child.inputs.contains(signal) {
                return Err(AllocationError::BoundaryOverlap {
                    chunk: child.chunk.clone(),
                    signal: signal.clone(),
                });
            }
            uses.entry(signal.clone())
                .or_default()
                .sources
                .push(TrunkOwner::Child(child.chunk.clone()));
        }
        for signal in child.inputs {
            uses.entry(signal.clone())
                .or_default()
                .child_sinks
                .insert(child.chunk.clone());
        }
    }
    for signal in &root.outputs {
        uses.entry(signal.clone()).or_default().root_sink = true;
    }
    for (signal, use_) in &uses {
        let signal = signal.clone();
        match use_.sources.len() {
            0 => return Err(AllocationError::MissingDriver { signal }),
            1 => {}
            _ => return Err(AllocationError::DuplicateDriver { signal }),
        }
        if use_.child_sinks.is_empty() && !use_.root_sink {
            return Err(AllocationError::UnconsumedSignal { signal });
        }
    }
    Ok(uses)
}

/// How much local space a child chunk needs in its own frame, given the portal
/// row the allocator just pinned for it.
///
/// A chunk the caller will hand to the leaf router needs its seed's envelope.
/// A chunk that will itself become a contract parent needs room for its own
/// corridor and its own children's halos, which is a different and far wider
/// shape than a flat seed of the same gates: measured on an eight-gate chain,
/// `seed_extent` says 34 columns and the layout that chunk actually receives
/// is 294. Sizing a recursive child as a seed is not conservative, it is
/// wrong, and the child escapes its region every time. The allocator cannot
/// know which a caller intends, so the caller that owns the recursion says.
pub trait ChildExtent {
    fn extent(&self, chunk: &Chunk, ports: &PortPlacements) -> Result<Anchor, AllocationError>;
}

/// Every child goes straight to the leaf router.
pub struct SeedExtent;

impl ChildExtent for SeedExtent {
    fn extent(&self, chunk: &Chunk, ports: &PortPlacements) -> Result<Anchor, AllocationError> {
        planner::seed_extent(&chunk.netlist, ports).map_err(|error| AllocationError::Seed {
            chunk: chunk.id.clone(),
            error,
        })
    }
}

pub fn allocate(
    root: &Netlist,
    chunks: &[Chunk],
    limits: AllocationLimits,
) -> Result<AllocationPlan, AllocationError> {
    allocate_with_root_ports(root, chunks, limits, None)
}

/// Allocate a parent contract, optionally preserving the caller row supplied
/// by an enclosing parent.  A nested node must keep the exact portal cells its
/// parent owns; recomputing its root row would make the same logical port land
/// at a different physical cell at each recursion level.
pub fn allocate_with_root_ports(
    root: &Netlist,
    chunks: &[Chunk],
    limits: AllocationLimits,
    fixed_root_ports: Option<&PortPlacements>,
) -> Result<AllocationPlan, AllocationError> {
    allocate_with(root, chunks, limits, fixed_root_ports, &SeedExtent)
}

/// Allocate a parent contract, sizing every child with `extent` instead of
/// assuming it is a leaf.
pub fn allocate_with(
    root: &Netlist,
    chunks: &[Chunk],
    limits: AllocationLimits,
    fixed_root_ports: Option<&PortPlacements>,
    extent: &dyn ChildExtent,
) -> Result<AllocationPlan, AllocationError> {
    if limits.delay_budget_ticks == 0 {
        return Err(AllocationError::ZeroDelayBudget);
    }
    if limits.corridor_capacity == 0 {
        return Err(AllocationError::ZeroCapacity);
    }
    if chunks.is_empty() {
        return Err(AllocationError::NoChildren);
    }
    let by_id: BTreeMap<&ChunkId, &Chunk> = chunks.iter().map(|c| (&c.id, c)).collect();

    // Every signal's single source and all of its sinks.
    let boundaries = by_id
        .values()
        .map(|chunk| ChildBoundary {
            chunk: &chunk.id,
            inputs: &chunk.boundary_inputs,
            outputs: &chunk.boundary_outputs,
        })
        .collect::<Vec<_>>();
    let uses = boundary_signal_uses(root, &boundaries)?;
    let needed = u32::try_from(uses.len()).map_err(|_| AllocationError::CoordinateOverflow)?;
    if needed > limits.corridor_capacity {
        return Err(AllocationError::CorridorCapacityExceeded {
            needed,
            capacity: limits.corridor_capacity,
        });
    }
    let ordered = topological_chunks(chunks)?;

    // Root caller row at `caller_row_z`; corridor starts one row behind it;
    // children's halos follow the corridor. A caller that pinned its ports
    // owns the row they sit on, so the whole contract is built behind that
    // row.
    let placement = root_placement(root, fixed_root_ports)?;
    let caller_row_z = placement.caller_row_z;
    // The corridor's depth is not known yet -- it falls out of the lane
    // colouring, which needs the children's final `x` spans.  Nothing about
    // the `x` layout depends on `z`, so children are laid out against the
    // shallowest possible corridor and shifted south once, together, when the
    // real depth is known.
    let provisional_halo_z = add(caller_row_z, 1)?;
    let halo_z = provisional_halo_z;

    // Each child laid out in its local frame -- halo at the origin, caller
    // row z = 0, region from (1, 0, 1) -- then translated along +x so halos
    // stay disjoint with one parent-owned cell between them.
    //
    // The first halo starts at x = 1, not x = 0, because this frame's own
    // x = 0 column belongs to *this* node's parent. `compose` may lay trunk
    // hardware anywhere in a child halo, so a halo touching column 0 is a
    // nested node placing blocks outside the region it was allocated -- the
    // same escape the leaf router refuses, one level up.
    let mut children = Vec::with_capacity(ordered.len());
    let mut cursor_x = 1;
    for chunk in ordered {
        let mut inputs: Vec<&String> = chunk.boundary_inputs.iter().collect();
        inputs.sort();
        let mut outputs: Vec<&String> = chunk.boundary_outputs.iter().collect();
        outputs.sort();
        let named: Vec<(&String, PortRole)> = inputs
            .into_iter()
            .map(|signal| (signal, PortRole::Input))
            .chain(outputs.into_iter().map(|signal| (signal, PortRole::Output)))
            .collect();

        let mut local = PortPlacements::default();
        let mut local_pins = Vec::with_capacity(named.len());
        for (slot, (signal, role)) in named.iter().enumerate() {
            let pin = north_pin(row_cell(2, slot, 0)?, *role);
            local.pin((*signal).clone(), pin.at, pin.toward);
            local_pins.push(pin);
        }
        // The same keep-out `ChildAllocation::port_placements` hands the
        // child's compiler, so a sizer that plans the child measures the
        // plan the child will actually run.
        local.bound_below(LowerBound {
            x: LOCAL_REGION_MIN.x,
            z: LOCAL_REGION_MIN.z,
        });
        let needs = extent.extent(chunk, &local)?;
        // Region max: past the last portal's halo and past whatever the child
        // itself needs. That height is a maximum rather than the constant,
        // because a nested child's own children's halos sit one cell above
        // its router ceiling.
        let portal_end = local_pins.last().map_or(2, |pin| pin.at.x);
        let local_max = Anchor {
            x: add(portal_end, 1)?.max(needs.x),
            y: needs.y.max(REGION_TOP),
            z: needs.z.max(1),
        };

        let origin = Anchor {
            x: cursor_x,
            y: 0,
            z: halo_z,
        };
        let translate = |local: Anchor| -> Result<Anchor, AllocationError> {
            Ok(Anchor {
                x: add(local.x, origin.x)?,
                y: add(local.y, origin.y)?,
                z: add(local.z, origin.z)?,
            })
        };
        let region = Prism {
            min: translate(LOCAL_REGION_MIN)?,
            max: translate(local_max)?,
        };
        let halo = Prism {
            min: origin,
            max: translate(Anchor {
                x: add(local_max.x, 1)?,
                y: add(local_max.y, 1)?,
                z: add(local_max.z, 1)?,
            })?,
        };
        let portals = named
            .iter()
            .zip(local_pins)
            .map(|((signal, role), pin)| {
                Ok(PortalWindow {
                    signal: (*signal).clone(),
                    role: *role,
                    pin: PortPin {
                        at: translate(pin.at)?,
                        toward: pin.toward,
                    },
                })
            })
            .collect::<Result<Vec<_>, AllocationError>>()?;

        children.push(ChildAllocation {
            chunk: chunk.id.clone(),
            origin,
            region: RegionMask::Prism(region),
            halo,
            portals,
        });
        cursor_x = add(halo.max.x, 2)?;
    }

    let mut root_ports = Vec::with_capacity(root.inputs.len() + root.outputs.len());
    let mut root_xs = BTreeSet::new();
    let declared = root
        .inputs
        .iter()
        .map(|s| (s, PortRole::Input))
        .chain(root.outputs.iter().map(|s| (s, PortRole::Output)));
    for (slot, (signal, role)) in declared.enumerate() {
        let use_ = &uses[signal.as_str()];
        if let Some(fixed) = fixed_root_ports {
            let pin = fixed
                .get(signal)
                .ok_or_else(|| AllocationError::MissingRootPort {
                    signal: signal.clone(),
                })?;
            root_xs.insert(pin.at.x);
            root_ports.push(RootPort {
                signal: signal.clone(),
                role,
                pin,
            });
            continue;
        }
        let mut endpoint_xs = match role {
            PortRole::Input => use_
                .child_sinks
                .iter()
                .filter_map(|chunk| {
                    children
                        .iter()
                        .find(|child| &child.chunk == chunk)
                        .and_then(|child| child.portal(signal, PortRole::Input))
                        .map(|portal| portal.pin.at.x)
                })
                .collect::<Vec<_>>(),
            PortRole::Output => use_
                .sources
                .iter()
                .find_map(|owner| match owner {
                    TrunkOwner::Child(chunk) => children
                        .iter()
                        .find(|child| &child.chunk == chunk)
                        .and_then(|child| child.portal(signal, PortRole::Output))
                        .map(|portal| portal.pin.at.x),
                    TrunkOwner::Root => None,
                })
                .into_iter()
                .collect(),
        };
        endpoint_xs.sort_unstable();
        let preferred = endpoint_xs
            .get(endpoint_xs.len() / 2)
            .copied()
            .unwrap_or(row_cell(1, slot, caller_row_z)?.x);
        let mut x = preferred;
        while root_xs
            .iter()
            .any(|used: &i32| (i64::from(*used) - i64::from(x)).abs() < i64::from(PORTAL_PITCH))
        {
            x = add(x, PORTAL_PITCH)?;
        }
        root_xs.insert(x);
        root_ports.push(RootPort {
            signal: signal.clone(),
            role,
            pin: north_pin(
                Anchor {
                    x,
                    y: PORTAL_Y,
                    z: caller_row_z,
                },
                role,
            ),
        });
    }

    // Lanes: one padded `x` interval per trunk, coloured greedily in a
    // canonical order.  Two trunks share a lane only when their padded spans
    // are disjoint, so a lane is a row several trunks cross without ever
    // meeting, and the corridor is as deep as the colouring needs -- not as
    // deep as the caller's capacity budget.
    // Every `x` a trunk end resolves to, not just its pin's: an east- or
    // west-facing landed pin hands over and starts its net two and three cells
    // along `x`, so a span read off the pin alone would not cover the cells the
    // route is actually laid in.
    let end_xs = |owner: &TrunkOwner, signal: &str, role: PortRole| -> Vec<i32> {
        let pin = match owner {
            TrunkOwner::Root => root_ports
                .iter()
                .find(|port| port.signal == signal && port.role == role)
                .map(|port| port.pin),
            TrunkOwner::Child(chunk) => children
                .iter()
                .find(|child| &child.chunk == chunk)
                .and_then(|child| child.portal(signal, role))
                .map(|portal| portal.pin),
        };
        pin.into_iter()
            .flat_map(|pin| port_footprint(&pin, role))
            .map(|at| at.x)
            .collect()
    };
    let mut spans: Vec<TrunkSpan> = Vec::with_capacity(uses.len());
    for (signal, use_) in &uses {
        let signal = signal.as_str();
        let mut xs = Vec::new();
        xs.extend(end_xs(
            &use_.sources[0],
            signal,
            match use_.sources[0] {
                TrunkOwner::Root => PortRole::Input,
                TrunkOwner::Child(_) => PortRole::Output,
            },
        ));
        for chunk in &use_.child_sinks {
            xs.extend(end_xs(
                &TrunkOwner::Child(chunk.clone()),
                signal,
                PortRole::Input,
            ));
        }
        if use_.root_sink {
            xs.extend(end_xs(&TrunkOwner::Root, signal, PortRole::Output));
        }
        spans.push(TrunkSpan::padded(signal, &xs)?);
    }
    let (lanes, lanes_used) = colour_lanes(&mut spans);
    let corridor_max_z = add(
        caller_row_z,
        mul(
            count(corridor_depth_units(lanes_used) as usize)?,
            LANE_PITCH,
        )?,
    )?;

    // One uniform shift south, now that the corridor's depth is known: every
    // child moves by the same delta, its origin, region, halo and portals
    // together, and the root pins -- which the caller may have fixed -- do not
    // move at all.
    let shift = add(corridor_max_z, 1)? - provisional_halo_z;
    for child in &mut children {
        shift_child_z(child, shift)?;
    }

    // The corridor spans every child halo and every root pin.
    let children_max_x = children.last().expect("non-empty").halo.max.x;
    let root_max_x = root_ports
        .iter()
        .flat_map(|port| port_footprint(&port.pin, port.role))
        .map(|at| at.x)
        .max()
        .unwrap_or(0);
    let corridor = Corridor {
        region: Prism {
            min: Anchor {
                x: 1,
                y: 0,
                z: add(caller_row_z, 1)?,
            },
            max: Anchor {
                x: children_max_x.max(root_max_x),
                y: REGION_TOP,
                z: corridor_max_z,
            },
        },
        capacity: limits.corridor_capacity,
    };
    debug_assert!(corridor.lane_capacity() >= lanes_used);
    if placement.access == RootAccess::CallerRow {
        for port in &root_ports {
            debug_assert!(
                corridor.region.contains(port.pin.handover(port.role)),
                "root handover {:?} outside corridor {:?}",
                port.pin,
                corridor.region
            );
        }
    }

    let root_end = |signal: &str, role: PortRole| TrunkEnd {
        owner: TrunkOwner::Root,
        pin: root_ports
            .iter()
            .find(|port| port.signal == signal && port.role == role)
            .expect("root port declared")
            .pin,
        role,
    };
    let child_end = |chunk: &ChunkId, signal: &str, role: PortRole| TrunkEnd {
        owner: TrunkOwner::Child(chunk.clone()),
        pin: children
            .iter()
            .find(|child| &child.chunk == chunk)
            .and_then(|child| child.portal(signal, role))
            .expect("every boundary signal has a portal")
            .pin,
        role,
    };
    let trunks = uses
        .iter()
        .map(|(signal, use_)| {
            let signal = signal.as_str();
            let source = match &use_.sources[0] {
                TrunkOwner::Root => root_end(signal, PortRole::Input),
                TrunkOwner::Child(chunk) => child_end(chunk, signal, PortRole::Output),
            };
            let mut sinks: Vec<TrunkEnd> = use_
                .child_sinks
                .iter()
                .map(|chunk| child_end(chunk, signal, PortRole::Input))
                .collect();
            if use_.root_sink {
                sinks.push(root_end(signal, PortRole::Output));
            }
            let source_at = source.handover();
            sinks.sort_by_key(|sink| {
                let at = sink.handover();
                (
                    Reverse(
                        (i64::from(at.x) - i64::from(source_at.x)).abs()
                            + (i64::from(at.y) - i64::from(source_at.y)).abs()
                            + (i64::from(at.z) - i64::from(source_at.z)).abs(),
                    ),
                    at.z,
                    at.x,
                    at.y,
                )
            });
            Trunk {
                signal: signal.to_owned(),
                source,
                sinks,
                lane: lanes[signal],
            }
        })
        .collect();

    Ok(AllocationPlan {
        contract: SignalContract {
            polarity: SignalPolarity::Positive,
            strength: 15,
            delay_budget_ticks: limits.delay_budget_ticks,
        },
        root_placement: placement,
        root_ports,
        children,
        corridor,
        trunks,
    })
}

fn topological_chunks(chunks: &[Chunk]) -> Result<Vec<&Chunk>, AllocationError> {
    let by_id = chunks
        .iter()
        .map(|chunk| (&chunk.id, chunk))
        .collect::<BTreeMap<_, _>>();
    let producers = chunks
        .iter()
        .flat_map(|chunk| {
            chunk
                .boundary_outputs
                .iter()
                .map(move |signal| (signal.as_str(), &chunk.id))
        })
        .collect::<BTreeMap<_, _>>();
    let mut dependencies = chunks
        .iter()
        .map(|chunk| {
            let producers = chunk
                .boundary_inputs
                .iter()
                .filter_map(|signal| producers.get(signal.as_str()).copied())
                .filter(|producer| *producer != &chunk.id)
                .collect::<BTreeSet<_>>();
            (&chunk.id, producers)
        })
        .collect::<BTreeMap<_, _>>();
    let mut consumers: BTreeMap<&ChunkId, BTreeSet<&ChunkId>> = BTreeMap::new();
    for (&consumer, producers) in &dependencies {
        for &producer in producers {
            consumers.entry(producer).or_default().insert(consumer);
        }
    }
    let key = |id: &ChunkId| {
        let chunk = by_id[id];
        (
            chunk
                .netlist
                .gates
                .iter()
                .map(|gate| gate.output.as_str())
                .min()
                .unwrap_or("")
                .to_owned(),
            id.clone(),
        )
    };
    let mut ready = dependencies
        .iter()
        .filter(|(_, dependencies)| dependencies.is_empty())
        .map(|(&id, _)| key(id))
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(chunks.len());
    while let Some((_, id)) = ready.pop_first() {
        ordered.push(by_id[&id]);
        for consumer in consumers.get(&id).into_iter().flatten() {
            let remaining = dependencies.get_mut(consumer).expect("known consumer");
            remaining.remove(&id);
            if remaining.is_empty() {
                ready.insert(key(consumer));
            }
        }
    }
    (ordered.len() == chunks.len())
        .then_some(ordered)
        .ok_or(AllocationError::ChunkDependencyCycle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A partial set is completed by the validator's own rule: on the
    /// supplied row when the pins fold onto one, landed behind them when they
    /// do not, and never by moving a supplied pin.
    #[test]
    fn partial_pins_complete_to_a_placement_the_validator_accepts() {
        let net = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x", "y"]), Gate::nor("b", &["a"])],
        };
        assert!(matches!(normalise_root_pins(&net, None), Ok(None)));
        assert!(matches!(
            normalise_root_pins(&net, Some(&PortPlacements::default())),
            Ok(None)
        ));

        // One row: the missing ports walk east along it, one pitch apart,
        // skipping the caller's own cell.
        let mut row = PortPlacements::default();
        row.pin("y", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        let completed = normalise_root_pins(&net, Some(&row)).unwrap().unwrap();
        assert_eq!(completed.get("y"), row.get("y"));
        assert_eq!(
            completed.get("x"),
            Some(north_pin(Anchor { x: 4, y: 1, z: 4 }, PortRole::Input))
        );
        assert_eq!(
            completed.get("b"),
            Some(north_pin(Anchor { x: 7, y: 1, z: 4 }, PortRole::Output))
        );
        assert_eq!(root_pin_row(&net, &completed), Ok(4));

        // A complete set is handed back untouched.
        let full = normalise_root_pins(&net, Some(&completed))
            .unwrap()
            .unwrap();
        for (signal, pin) in completed.iter() {
            assert_eq!(full.get(signal), Some(*pin));
        }

        // Not one row: a south-facing output hands over in front of itself,
        // so the set lands, and the missing input is placed where the landed
        // rule accepts it -- on the northmost supplied row, clear of every
        // supplied footprint.
        let mut landed_set = PortPlacements::default();
        landed_set.pin("b", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        landed_set.pin("y", Anchor { x: 5, y: 1, z: 6 }, Facing::South);
        let completed = normalise_root_pins(&net, Some(&landed_set))
            .unwrap()
            .unwrap();
        assert_eq!(completed.get("b"), landed_set.get("b"));
        assert_eq!(completed.get("y"), landed_set.get("y"));
        // One pitch past the easternmost supplied cell (`y` at x = 5), on the
        // pitch grid: 8 aligned up to 10.
        let x = completed.get("x").expect("x was placed");
        assert_eq!(
            x,
            north_pin(Anchor { x: 10, y: 1, z: 4 }, PortRole::Input),
            "landed completion moved"
        );
        assert!(matches!(
            root_placement(&net, Some(&completed)),
            Ok(RootPlacement {
                access: RootAccess::Landed { .. },
                ..
            })
        ));
        assert_eq!(
            normalise_root_pins(&net, Some(&landed_set))
                .unwrap()
                .unwrap()
                .get("x"),
            Some(x)
        );

        // The supplied pins are refused on their own, by type, before any
        // cell is chosen around them.
        let mut off_plane = PortPlacements::default();
        off_plane.pin("b", Anchor { x: 1, y: 0, z: 4 }, Facing::North);
        assert!(matches!(
            normalise_root_pins(&net, Some(&off_plane)),
            Err(AllocationError::UnsupportedRootPin { signal, .. }) if signal == "b"
        ));
        let mut undeclared = PortPlacements::default();
        undeclared.pin("q", Anchor { x: 1, y: 1, z: 4 }, Facing::North);
        assert!(matches!(
            normalise_root_pins(&net, Some(&undeclared)),
            Err(AllocationError::InvalidRootPort {
                refusal: PinRefusal::UndeclaredPort,
                ..
            })
        ));
    }

    /// The caller's lower bound is a contract on the completion too: placed
    /// ports start east of the bound's column, the bound itself travels with
    /// the completed set, and a supplied pin whose hardware would stand below
    /// the bound is the planner's own typed refusal, not something placed
    /// around.
    #[test]
    fn partial_pins_respect_the_callers_lower_bound() {
        let net = Netlist {
            inputs: vec!["x".into(), "y".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x", "y"]), Gate::nor("b", &["a"])],
        };
        // The bound's column is the supplied pin's own: a supplied handover
        // west of the bound would be refused below, so the bound never
        // exceeds a supplied cell, and the walk starts east of both.
        let bound = LowerBound { x: 20, z: 4 };
        let mut pins = PortPlacements::default();
        pins.pin("y", Anchor { x: 20, y: 1, z: 4 }, Facing::South);
        pins.bound_below(bound);
        let completed = normalise_root_pins(&net, Some(&pins)).unwrap().unwrap();
        assert_eq!(completed.lower_bound(), Some(bound));
        assert_eq!(completed.get("y"), pins.get("y"));
        // 20 + 3 = 23, aligned up to the pitch grid: 25, then 28.
        assert_eq!(
            completed.get("x"),
            Some(north_pin(Anchor { x: 25, y: 1, z: 4 }, PortRole::Input))
        );
        assert_eq!(
            completed.get("b"),
            Some(north_pin(Anchor { x: 28, y: 1, z: 4 }, PortRole::Output))
        );
        assert_eq!(planner::validate_port_placements(&net, &completed), Ok(()));

        // A south-facing input on row 4 hands over on row 5, which a bound at
        // z = 6 forbids: refused by the planner, naming the caller's pin.
        let mut below = PortPlacements::default();
        below.pin("y", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        below.bound_below(LowerBound { x: 1, z: 6 });
        assert!(matches!(
            normalise_root_pins(&net, Some(&below)),
            Err(AllocationError::InvalidRootPort {
                port,
                refusal: PinRefusal::BelowLowerBound { .. },
                ..
            }) if port == "y"
        ));
    }

    /// Completion is one checked walk east of the caller's pins: a pin with
    /// room past it completes at once, one with exactly one grid cell of room
    /// finishes on `i32::MAX`, and one with none is refused as overflow at
    /// once, never searched for.
    #[test]
    fn partial_pins_near_the_coordinate_limit_complete_or_refuse_promptly() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        };

        // `i32::MAX` is on the grid, and one pitch past `i32::MAX - 3` is
        // exactly it: the last missing port lands there and nothing past it
        // is ever computed.
        let mut last_cell = PortPlacements::default();
        last_cell.pin(
            "x",
            Anchor {
                x: i32::MAX - PORTAL_PITCH,
                y: 1,
                z: 4,
            },
            Facing::South,
        );
        let completed = normalise_root_pins(&net, Some(&last_cell))
            .expect("a port that fits on i32::MAX is not an overflow")
            .unwrap();
        assert_eq!(
            completed.get("b"),
            Some(north_pin(
                Anchor {
                    x: i32::MAX,
                    y: 1,
                    z: 4
                },
                PortRole::Output
            ))
        );

        let far = i32::MAX - 100;
        let mut pins = PortPlacements::default();
        pins.pin("x", Anchor { x: far, y: 1, z: 4 }, Facing::South);
        let completed = normalise_root_pins(&net, Some(&pins)).unwrap().unwrap();
        let expected = align_to_pitch(far + PORTAL_PITCH).unwrap();
        assert!(expected > far && expected - far <= 2 * PORTAL_PITCH);
        assert_eq!(
            completed.get("b"),
            Some(north_pin(
                Anchor {
                    x: expected,
                    y: 1,
                    z: 4
                },
                PortRole::Output
            ))
        );

        let mut edge = PortPlacements::default();
        edge.pin(
            "x",
            Anchor {
                x: i32::MAX - 2,
                y: 1,
                z: 4,
            },
            Facing::South,
        );
        assert_eq!(
            normalise_root_pins(&net, Some(&edge)).err(),
            Some(AllocationError::CoordinateOverflow)
        );
    }

    #[test]
    fn pitch_alignment_rounds_up_onto_the_caller_row_grid() {
        for (x, aligned) in [(1, 1), (2, 4), (3, 4), (4, 4), (8, 10), (23, 25)] {
            assert_eq!(align_to_pitch(x), Ok(aligned), "align {x}");
        }
        // `i32::MAX` is itself on the grid, so alignment alone never
        // overflows; the step before it, `add(east, PORTAL_PITCH)`, is what
        // refuses a pin with no room east of it.
        assert_eq!(align_to_pitch(i32::MAX - 2), Ok(i32::MAX));
        assert_eq!(align_to_pitch(i32::MAX), Ok(i32::MAX));
        // The residue is what is aligned, so the far end of the range is
        // arithmetic like any other, not a panic.
        assert_eq!(align_to_pitch(i32::MIN), Ok(i32::MIN));
    }

    #[test]
    fn corridor_sizing_covers_every_lane_without_using_access_bands() {
        for lanes in [0u32, 1, 3, 7] {
            let units = corridor_depth_units(lanes);
            let corridor = Corridor {
                region: Prism {
                    min: Anchor { x: 0, y: 0, z: 1 },
                    max: Anchor {
                        x: 1,
                        y: REGION_TOP,
                        z: units as i32 * LANE_PITCH,
                    },
                },
                capacity: 64,
            };
            assert!(corridor.lane_capacity() >= lanes);
            for lane in 0..lanes {
                let track = corridor.lane_track(lane).unwrap();
                assert!(!corridor.access_bands().contains(&track));
            }
        }
    }

    #[test]
    fn corridor_depth_follows_the_lanes_used_and_the_minimum() {
        assert_eq!(corridor_depth_units(0), MIN_CORRIDOR_UNITS);
        assert_eq!(corridor_depth_units(1), MIN_CORRIDOR_UNITS);
        assert_eq!(corridor_depth_units(7), 9);
        assert_eq!(corridor_depth_units(u32::MAX), u32::MAX);
    }

    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::Gate;
    use crate::redstone::simulator::position::HORIZONTAL;

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

    fn plan(net: &Netlist, max_gates: usize) -> (Vec<Chunk>, AllocationPlan) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), max_gates).unwrap();
        let plan = allocate(net, &chunks, LIMITS).unwrap();
        (chunks, plan)
    }

    fn chunk_with_output<'a>(chunks: &'a [Chunk], signal: &str) -> &'a Chunk {
        chunks
            .iter()
            .find(|c| c.boundary_outputs.iter().any(|s| s == signal))
            .unwrap()
    }

    fn intersects(a: &Prism, b: &Prism) -> bool {
        a.min.x <= b.max.x
            && b.min.x <= a.max.x
            && a.min.y <= b.max.y
            && b.min.y <= a.max.y
            && a.min.z <= b.max.z
            && b.min.z <= a.max.z
    }

    fn nonnegative(at: Anchor) -> bool {
        at.x >= 0 && at.y >= 0 && at.z >= 0
    }

    /// Horizontal, nonnegative, caller cell parent-owned, handover inside
    /// the owner's own space.
    fn assert_end_valid(end: &TrunkEnd, plan: &AllocationPlan) {
        assert!(HORIZONTAL.contains(&end.pin.toward));
        assert!(nonnegative(end.pin.at), "{:?}", end.pin);
        match &end.owner {
            TrunkOwner::Child(chunk) => {
                let child = plan.child(chunk).unwrap();
                assert!(child.in_halo(end.pin.at));
                assert!(child.region.contains(end.handover()));
            }
            TrunkOwner::Root => {
                assert_eq!(end.pin.at.z, 0);
                assert!(plan.corridor.region.contains(end.handover()));
            }
        }
    }

    #[test]
    fn shuffled_gates_and_chunk_slices_allocate_identically() {
        let gates = vec![
            Gate::nor("a", &["x"]),
            Gate::nor("b", &["y"]),
            Gate::nor("c", &["a", "b"]),
            Gate::nor("d", &["z"]),
        ];
        let mut shuffled = gates.clone();
        shuffled.reverse();
        shuffled.swap(0, 2);
        let ordered = netlist(&["x", "y", "z", "unused"], &["c", "d"], gates);
        let shuffled = netlist(&["x", "y", "z", "unused"], &["c", "d"], shuffled);

        let (chunks, lhs) = plan(&ordered, 2);
        let (_, rhs) = plan(&shuffled, 2);
        assert_eq!(lhs, rhs);
        let mut reversed = chunks.clone();
        reversed.reverse();
        assert_eq!(allocate(&ordered, &reversed, LIMITS).unwrap(), lhs);

        let names: Vec<&str> = lhs.root_ports.iter().map(|p| p.signal.as_str()).collect();
        assert_eq!(names, ["x", "y", "z", "unused", "c", "d"]);
        assert_eq!(lhs.trunk("unused").unwrap().sinks.len(), 1);
        assert_eq!(lhs.trunks.len(), 8);
        assert_eq!(lhs.contract.delay_budget_ticks, LIMITS.delay_budget_ticks);
        assert_eq!(lhs.contract.strength, 15);
        assert_eq!(lhs.contract.polarity, SignalPolarity::Positive);
    }

    /// The padded span the colouring gave one trunk, rebuilt from the plan.
    fn span_of(trunk: &Trunk) -> TrunkSpan<'_> {
        let xs: Vec<i32> = std::iter::once(&trunk.source)
            .chain(&trunk.sinks)
            .map(|end| end.pin.at.x)
            .collect();
        TrunkSpan::padded(&trunk.signal, &xs).expect("a planned trunk has ends")
    }

    #[test]
    fn trunks_sharing_a_lane_have_disjoint_padded_spans() {
        // Four independent inverters: every trunk is short and far from the
        // others, so the colouring has real room to share lanes.
        let wide = netlist(
            &["p", "q", "r", "s"],
            &["np", "nq", "nr", "ns"],
            vec![
                Gate::nor("np", &["p"]),
                Gate::nor("nq", &["q"]),
                Gate::nor("nr", &["r"]),
                Gate::nor("ns", &["s"]),
            ],
        );
        let (_, plan) = plan(&wide, 1);
        let lanes: BTreeSet<u32> = plan.trunks.iter().map(|trunk| trunk.lane).collect();
        assert!(
            lanes.len() < plan.trunks.len(),
            "{} trunks used {} lanes: the fixture must share at least one",
            plan.trunks.len(),
            lanes.len()
        );
        for (i, left) in plan.trunks.iter().enumerate() {
            for right in &plan.trunks[i + 1..] {
                if left.lane != right.lane {
                    continue;
                }
                let (a, b) = (span_of(left), span_of(right));
                assert!(
                    a.right < b.left || b.right < a.left,
                    "{} {:?} and {} {:?} share lane {}",
                    left.signal,
                    (a.left, a.right),
                    right.signal,
                    (b.left, b.right),
                    left.lane
                );
            }
        }
    }

    #[test]
    fn every_lane_is_a_real_track_clear_of_the_access_bands() {
        let chain = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("b", &["a"]), Gate::nor("a", &["x"])],
        );
        let (_, plan) = plan(&chain, 1);
        assert!(
            plan.trunks
                .iter()
                .any(|trunk| matches!(trunk.source.owner, TrunkOwner::Root)
                    || trunk
                        .sinks
                        .iter()
                        .any(|sink| matches!(sink.owner, TrunkOwner::Root))),
            "the fixture must carry a root-ended trunk"
        );
        for trunk in &plan.trunks {
            let track = plan
                .corridor
                .lane_track(trunk.lane)
                .expect("a coloured lane is a track the corridor has");
            assert!(
                !plan.corridor.access_bands().contains(&track),
                "lane {} of trunk {} lands on an access band",
                trunk.lane,
                trunk.signal
            );
            assert!(plan.corridor.region.contains(Anchor {
                x: plan.corridor.region.min.x,
                y: PORTAL_Y,
                z: track,
            }));
        }
    }

    #[test]
    fn corridor_capacity_does_not_inflate_the_corridor() {
        let chain = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("b", &["a"]), Gate::nor("a", &["x"])],
        );
        let chunks = partition(&chain, &root_chunk_id(&chain).unwrap(), 1).unwrap();
        let tight = allocate(
            &chain,
            &chunks,
            AllocationLimits {
                corridor_capacity: 3,
                ..LIMITS
            },
        )
        .unwrap();
        let generous = allocate(
            &chain,
            &chunks,
            AllocationLimits {
                corridor_capacity: 64,
                ..LIMITS
            },
        )
        .unwrap();
        // Capacity is an admission limit on boundary signals; the physical
        // contract is whatever the lanes needed.
        assert_eq!(tight.corridor.region, generous.corridor.region);
        assert_eq!(tight.local_extent(), generous.local_extent());
        assert_eq!(tight.children, generous.children);
        assert_eq!(tight.trunks, generous.trunks);
    }

    #[test]
    fn colouring_is_the_same_whatever_order_the_spans_arrive_in() {
        let mut spans = vec![
            TrunkSpan {
                left: 0,
                right: 5,
                signal: "a",
            },
            TrunkSpan {
                left: 6,
                right: 9,
                signal: "b",
            },
            TrunkSpan {
                left: 2,
                right: 7,
                signal: "c",
            },
            TrunkSpan {
                left: 20,
                right: 24,
                signal: "d",
            },
        ];
        let (lanes, used) = colour_lanes(&mut spans.clone());
        // `a` and `b` are disjoint and share lane 0; `c` overlaps both and
        // opens lane 1; `d` is clear of everything and rejoins lane 0.
        assert_eq!(used, 2);
        assert_eq!(lanes["a"], 0);
        assert_eq!(lanes["b"], 0);
        assert_eq!(lanes["c"], 1);
        assert_eq!(lanes["d"], 0);
        spans.reverse();
        assert_eq!(colour_lanes(&mut spans), (lanes, used));
    }

    #[test]
    fn cross_chunk_chain_has_matching_trunk_ends() {
        let chain = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("b", &["a"]), Gate::nor("a", &["x"])],
        );
        let (chunks, plan) = plan(&chain, 1);
        let producer = &chunk_with_output(&chunks, "a").id;
        let consumer = &chunk_with_output(&chunks, "b").id;

        let a = plan.trunk("a").unwrap();
        assert_eq!(a.source.owner, TrunkOwner::Child(producer.clone()));
        assert_eq!(a.source.role, PortRole::Output);
        assert_eq!(a.sinks.len(), 1);
        assert_eq!(a.sinks[0].owner, TrunkOwner::Child(consumer.clone()));
        assert_eq!(a.sinks[0].role, PortRole::Input);
        let b = plan.trunk("b").unwrap();
        assert_eq!(b.source.owner, TrunkOwner::Child(consumer.clone()));
        assert_eq!(b.sinks[0].owner, TrunkOwner::Root);
        for trunk in &plan.trunks {
            assert_end_valid(&trunk.source, &plan);
            trunk.sinks.iter().for_each(|s| assert_end_valid(s, &plan));
        }

        let child = plan.child(consumer).unwrap();
        let local = child.port_placements().get("a").unwrap();
        assert_eq!(child.to_global(local.at), a.sinks[0].pin.at);
        assert_eq!(local.toward, a.sinks[0].pin.toward);
    }

    #[test]
    fn fanout_to_two_chunks_is_one_trunk_with_two_sinks() {
        let net = netlist(
            &["x"],
            &["b", "c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["a"]),
                Gate::nor("c", &["a"]),
            ],
        );
        let (chunks, plan) = plan(&net, 1);
        let a = plan.trunk("a").unwrap();
        assert_eq!(
            a.source.owner,
            TrunkOwner::Child(chunk_with_output(&chunks, "a").id.clone())
        );
        let expected = BTreeSet::from([
            chunk_with_output(&chunks, "b").id.clone(),
            chunk_with_output(&chunks, "c").id.clone(),
        ]);
        let sinks = a
            .sinks
            .iter()
            .map(|sink| match &sink.owner {
                TrunkOwner::Child(chunk) => chunk.clone(),
                TrunkOwner::Root => panic!("fanout sinks are child inputs"),
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(sinks, expected);
        let source = a.source.handover();
        let distances = a.sinks.iter().map(|sink| {
            let at = sink.handover();
            (i64::from(at.x) - i64::from(source.x)).abs()
                + (i64::from(at.y) - i64::from(source.y)).abs()
                + (i64::from(at.z) - i64::from(source.z)).abs()
        });
        assert!(distances.clone().is_sorted_by(|left, right| left >= right));
        assert_eq!(plan.trunks.len(), 4);
    }

    #[test]
    fn regions_and_halos_are_disjoint_and_local_frames_hold_the_seed() {
        let net = netlist(
            &["x", "y"],
            &["c"],
            vec![
                Gate::nor("a", &["x"]),
                Gate::nor("b", &["y"]),
                Gate::nor("c", &["a", "b"]),
            ],
        );
        let (chunks, plan) = plan(&net, 1);
        assert_eq!(plan.children.len(), 3);
        for (i, child) in plan.children.iter().enumerate() {
            assert!(nonnegative(child.halo.min));
            assert!(child.halo.min.z > plan.corridor.region.max.z);
            assert!(child.halo.contains(child.region.bounds().min));
            for other in &plan.children[i + 1..] {
                assert!(!intersects(&child.halo, &other.halo));
            }

            let local_region = child.local_region();
            assert_eq!(local_region.min, Anchor { x: 1, y: 0, z: 1 });
            let placements = child.port_placements();
            for (j, portal) in child.portals.iter().enumerate() {
                assert!(child.in_halo(portal.pin.at));
                assert!(child.region.contains(portal.handover()));
                let local = placements.get(&portal.signal).unwrap();
                assert!(nonnegative(local.at));
                assert_eq!(local.at.z, 0);
                assert!(local_region.contains(local.handover(portal.role)));
                for other in &child.portals[j + 1..] {
                    assert!((portal.pin.at.x - other.pin.at.x).abs() >= PORTAL_PITCH);
                }
            }

            // The planner's own seed, pinned locally, starts inside the
            // envelope; only pinned terminals sit in the caller row.
            let chunk = chunks.iter().find(|c| c.id == child.chunk).unwrap();
            let local_halo = Prism {
                min: Anchor { x: 0, y: 0, z: 0 },
                max: child.to_local(child.halo.max),
            };
            for anchor in planner::starting_layout(&chunk.netlist, &placements).unwrap() {
                assert!(
                    local_region.contains(anchor) || (anchor.z == 0 && local_halo.contains(anchor)),
                    "{anchor:?} escapes {local_region:?}"
                );
            }
        }
        for port in &plan.root_ports {
            assert!(plan.corridor.region.contains(port.pin.handover(port.role)));
        }
    }

    /// Every way a caller row can disagree with the contract is refused with
    /// the offending pin and the reason, and a row the contract can honour is
    /// reported as the row it is -- nonzero included.
    #[test]
    fn unsupported_root_pins_are_refused_by_type_naming_the_cell_and_reason() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let honoured = |z| {
            let mut pins = PortPlacements::default();
            pins.pin("x", Anchor { x: 1, y: 1, z }, Facing::South);
            pins.pin("b", Anchor { x: 4, y: 1, z }, Facing::North);
            pins
        };
        assert_eq!(root_pin_row(&net, &honoured(0)), Ok(0));
        assert_eq!(root_pin_row(&net, &honoured(7)), Ok(7));

        let mut extra = honoured(0);
        extra.pin("not_declared", Anchor { x: 7, y: 1, z: 0 }, Facing::North);
        assert_eq!(
            root_pin_row(&net, &extra),
            Err(AllocationError::InvalidRootPort {
                port: "not_declared".into(),
                at: Anchor { x: 7, y: 1, z: 0 },
                refusal: PinRefusal::UndeclaredPort,
            })
        );

        let refused = |at, toward, reason| {
            Err(AllocationError::UnsupportedRootPin {
                signal: "b".into(),
                at,
                toward,
                reason,
            })
        };
        let repinned = |at, toward| {
            let mut pins = honoured(4);
            pins.pin("b", at, toward);
            pins
        };
        assert_eq!(
            root_pin_row(&net, &repinned(Anchor { x: 4, y: 3, z: 4 }, Facing::North)),
            refused(
                Anchor { x: 4, y: 3, z: 4 },
                Facing::North,
                "is not on the plane ports are placed on"
            )
        );
        assert_eq!(
            root_pin_row(&net, &repinned(Anchor { x: 0, y: 1, z: 4 }, Facing::North)),
            refused(
                Anchor { x: 0, y: 1, z: 4 },
                Facing::North,
                "is in the parent-owned x = 0 column"
            )
        );
        assert_eq!(
            root_pin_row(&net, &repinned(Anchor { x: 4, y: 1, z: -1 }, Facing::North)),
            refused(
                Anchor { x: 4, y: 1, z: -1 },
                Facing::North,
                "is behind the world floor"
            )
        );
        assert_eq!(
            root_pin_row(&net, &repinned(Anchor { x: 4, y: 1, z: 4 }, Facing::South)),
            refused(
                Anchor { x: 4, y: 1, z: 4 },
                Facing::South,
                "hands over somewhere other than the corridor behind it"
            )
        );
        assert_eq!(
            root_pin_row(&net, &repinned(Anchor { x: 4, y: 1, z: 9 }, Facing::North)),
            refused(
                Anchor { x: 4, y: 1, z: 9 },
                Facing::North,
                "is not on the same caller row as the other ports"
            )
        );
        assert_eq!(
            root_pin_row(&net, &repinned(Anchor { x: 2, y: 1, z: 4 }, Facing::North)),
            refused(
                Anchor { x: 2, y: 1, z: 4 },
                Facing::North,
                "is closer than one portal pitch to another port"
            )
        );

        let mut unpinned = PortPlacements::default();
        unpinned.pin("x", Anchor { x: 1, y: 1, z: 4 }, Facing::South);
        assert_eq!(
            root_pin_row(&net, &unpinned),
            Err(AllocationError::MissingRootPort { signal: "b".into() })
        );

        // The typed refusal is what the allocator itself returns, not only
        // the pre-check: geometry no contract can build stays refused.
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        assert_eq!(
            allocate_with_root_ports(
                &net,
                &chunks,
                LIMITS,
                Some(&repinned(Anchor { x: 4, y: 0, z: 4 }, Facing::North))
            ),
            Err(AllocationError::UnsupportedRootPin {
                signal: "b".into(),
                at: Anchor { x: 4, y: 0, z: 4 },
                toward: Facing::North,
                reason: "is below the plane ports are placed on",
            })
        );
        // Pins on two rows are not one caller row and never will be, but they
        // are buildable: the allocator lands the body behind them instead of
        // refusing, and moves neither pin.
        let split = repinned(Anchor { x: 4, y: 1, z: 9 }, Facing::North);
        let landed = allocate_with_root_ports(&net, &chunks, LIMITS, Some(&split)).unwrap();
        assert!(matches!(
            landed.root_placement.access,
            RootAccess::Landed { .. }
        ));
        for port in &landed.root_ports {
            assert_eq!(Some(port.pin), split.get(&port.signal));
        }
        let pinned = allocate_with_root_ports(&net, &chunks, LIMITS, Some(&honoured(7))).unwrap();
        assert_eq!(pinned.caller_row_z(), 7);
        assert_eq!(pinned.corridor.region.min.z, 8);
        assert_eq!(allocate(&net, &chunks, LIMITS).unwrap().caller_row_z(), 0);
    }

    #[test]
    fn interface_faults_are_typed() {
        let net = netlist(
            &["x"],
            &["b"],
            vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        );
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let second = chunks
            .iter()
            .position(|c| c.boundary_inputs == ["a"])
            .unwrap();
        let first = 1 - second;

        let zero_delay = AllocationLimits {
            delay_budget_ticks: 0,
            ..LIMITS
        };
        assert_eq!(
            allocate(&net, &chunks, zero_delay),
            Err(AllocationError::ZeroDelayBudget)
        );
        let tight = AllocationLimits {
            corridor_capacity: 1,
            ..LIMITS
        };
        assert_eq!(
            allocate(&net, &chunks, tight),
            Err(AllocationError::CorridorCapacityExceeded {
                needed: 3,
                capacity: 1
            })
        );
        assert_eq!(
            allocate(&net, &[], LIMITS),
            Err(AllocationError::NoChildren)
        );

        let mut orphan = chunks.clone();
        orphan[second].boundary_inputs.push("ghost".into());
        assert_eq!(
            allocate(&net, &orphan, LIMITS),
            Err(AllocationError::MissingDriver {
                signal: "ghost".into()
            })
        );
        let mut twice = chunks.clone();
        twice[second].boundary_outputs.push("x".into());
        assert_eq!(
            allocate(&net, &twice, LIMITS),
            Err(AllocationError::DuplicateDriver { signal: "x".into() })
        );
        let mut dangling = chunks.clone();
        dangling[first].boundary_outputs.push("spare".into());
        assert_eq!(
            allocate(&net, &dangling, LIMITS),
            Err(AllocationError::UnconsumedSignal {
                signal: "spare".into()
            })
        );
        let mut overlap = chunks.clone();
        overlap[second].boundary_outputs.push("a".into());
        assert_eq!(
            allocate(&net, &overlap, LIMITS),
            Err(AllocationError::BoundaryOverlap {
                chunk: chunks[second].id.clone(),
                signal: "a".into()
            })
        );
    }
}
