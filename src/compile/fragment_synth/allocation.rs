//! Deterministic parent allocation of child physical contracts.
//!
//! From the root [`Netlist`] and its [`partition::Chunk`]s the parent fixes,
//! before any child is compiled or any route searched: a [`RegionMask`] and
//! one-cell halo per child, a [`PortalWindow`] per child boundary signal, one
//! [`Corridor`] with checked capacity, and one [`Trunk`] per boundary signal
//! carrying its single source and every sink, so fanout is represented once.
//!
//! Global layout runs north to south (increasing `z`), all coordinates
//! nonnegative: root caller row at `z = 0`, the corridor, then every child's
//! halo and region side by side along `x`.  Each child is compiled in its own
//! **local frame** -- halo corner at the origin, caller row at `z = 0`, region
//! from `z = 1` -- and translated by [`ChildAllocation::origin`] afterwards.
//!
//! Regions are allocation envelopes sized from the planner's own seed layout.
//! They do not clamp the child's search; the leaf compiler must refuse route
//! anchors outside its local world as well as occupied cells outside the
//! region, because out-of-bounds world writes are otherwise invisible.
//!
//! Children are ordered by [`ChunkId`], signals by name.  Given the canonical
//! chunks produced by [`partition::partition`], nothing depends on slice,
//! hash, or worker order.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::partition::{Chunk, ChunkId};
use crate::compile::geometry::Anchor;
use crate::compile::planner::{self, PlannerError, PortPin, PortPlacements, PortRole};
use crate::compile::topology::SignalPolarity;
use crate::compile::Netlist;
use crate::redstone::world::block::Facing;

/// Caller-cell pitch along a face: face-neighbour halos of two portals never
/// share a cell.
const PORTAL_PITCH: i32 = 3;
/// Corridor depth per unit of capacity.
const LANE_PITCH: i32 = 2;
/// Router ceiling plus its one-cell staircase clearance.
const REGION_TOP: i32 = 8;
/// The plane the planner places ports on.
const PORTAL_Y: i32 = 1;

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
    /// compiler.
    pub fn port_placements(&self) -> PortPlacements {
        let mut placements = PortPlacements::default();
        for portal in &self.portals {
            placements.pin(
                portal.signal.clone(),
                self.to_local(portal.pin.at),
                portal.pin.toward,
            );
        }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootPort {
    pub signal: String,
    pub role: PortRole,
    pub pin: PortPin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllocationPlan {
    pub contract: SignalContract,
    /// Declared order, inputs then outputs, unused inputs included.
    pub root_ports: Vec<RootPort>,
    /// By `ChunkId`.
    pub children: Vec<ChildAllocation>,
    pub corridor: Corridor,
    /// By signal name; one per boundary signal.
    pub trunks: Vec<Trunk>,
}

impl AllocationPlan {
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

#[derive(Default)]
struct SignalUse {
    sources: Vec<TrunkOwner>,
    child_sinks: BTreeSet<ChunkId>,
    root_sink: bool,
}

pub fn allocate(
    root: &Netlist,
    chunks: &[Chunk],
    limits: AllocationLimits,
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
    let mut uses: BTreeMap<&str, SignalUse> = BTreeMap::new();
    for signal in &root.inputs {
        uses.entry(signal)
            .or_default()
            .sources
            .push(TrunkOwner::Root);
    }
    for chunk in by_id.values() {
        for signal in &chunk.boundary_outputs {
            if chunk.boundary_inputs.contains(signal) {
                return Err(AllocationError::BoundaryOverlap {
                    chunk: chunk.id.clone(),
                    signal: signal.clone(),
                });
            }
            uses.entry(signal)
                .or_default()
                .sources
                .push(TrunkOwner::Child(chunk.id.clone()));
        }
        for signal in &chunk.boundary_inputs {
            uses.entry(signal)
                .or_default()
                .child_sinks
                .insert(chunk.id.clone());
        }
    }
    for signal in &root.outputs {
        uses.entry(signal).or_default().root_sink = true;
    }
    for (&signal, use_) in &uses {
        let signal = signal.to_owned();
        match use_.sources.len() {
            0 => return Err(AllocationError::MissingDriver { signal }),
            1 => {}
            _ => return Err(AllocationError::DuplicateDriver { signal }),
        }
        if use_.child_sinks.is_empty() && !use_.root_sink {
            return Err(AllocationError::UnconsumedSignal { signal });
        }
    }
    let needed = u32::try_from(uses.len()).map_err(|_| AllocationError::CoordinateOverflow)?;
    if needed > limits.corridor_capacity {
        return Err(AllocationError::CorridorCapacityExceeded {
            needed,
            capacity: limits.corridor_capacity,
        });
    }

    // Root caller row at z = 0; corridor from z = 1; children's halos from
    // the row after it.
    let corridor_max_z = mul(count(limits.corridor_capacity as usize)?, LANE_PITCH)?;
    let halo_z = add(corridor_max_z, 1)?;

    // Each child laid out in its local frame -- halo at the origin, caller
    // row z = 0, region from (1, 0, 1) -- then translated along +x so halos
    // stay disjoint with one parent-owned cell between them.
    let mut children = Vec::with_capacity(by_id.len());
    let mut cursor_x = 0;
    for chunk in by_id.values() {
        let mut inputs: Vec<&String> = chunk.boundary_inputs.iter().collect();
        inputs.sort();
        let mut outputs: Vec<&String> = chunk.boundary_outputs.iter().collect();
        outputs.sort();
        let named: Vec<(&String, PortRole)> = inputs
            .into_iter()
            .map(|s| (s, PortRole::Input))
            .chain(outputs.into_iter().map(|s| (s, PortRole::Output)))
            .collect();

        let mut local = PortPlacements::default();
        let mut local_pins = Vec::with_capacity(named.len());
        for (slot, (signal, role)) in named.iter().enumerate() {
            let pin = north_pin(row_cell(2, slot, 0)?, *role);
            local.pin((*signal).clone(), pin.at, pin.toward);
            local_pins.push(pin);
        }
        let seed = planner::seed_extent(&chunk.netlist, &local).map_err(|error| {
            AllocationError::Seed {
                chunk: chunk.id.clone(),
                error,
            }
        })?;
        // Region max: past the last portal's halo and past the seed envelope.
        let portal_end = local_pins.last().map_or(2, |pin| pin.at.x);
        let local_max = Anchor {
            x: add(portal_end, 1)?.max(seed.x),
            y: REGION_TOP,
            z: seed.z.max(1),
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
            min: translate(Anchor { x: 1, y: 0, z: 1 })?,
            max: translate(local_max)?,
        };
        let halo = Prism {
            min: origin,
            max: translate(Anchor {
                x: add(local_max.x, 1)?,
                y: add(REGION_TOP, 1)?,
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
    let declared = root
        .inputs
        .iter()
        .map(|s| (s, PortRole::Input))
        .chain(root.outputs.iter().map(|s| (s, PortRole::Output)));
    for (slot, (signal, role)) in declared.enumerate() {
        root_ports.push(RootPort {
            signal: signal.clone(),
            role,
            pin: north_pin(row_cell(1, slot, 0)?, role),
        });
    }

    // The corridor spans every child halo and every root pin.
    let children_max_x = children.last().expect("non-empty").halo.max.x;
    let root_max_x = root_ports.last().map_or(0, |port| port.pin.at.x);
    let corridor = Corridor {
        region: Prism {
            min: Anchor { x: 0, y: 0, z: 1 },
            max: Anchor {
                x: children_max_x.max(root_max_x),
                y: REGION_TOP,
                z: corridor_max_z,
            },
        },
        capacity: limits.corridor_capacity,
    };
    for port in &root_ports {
        debug_assert!(
            corridor.region.contains(port.pin.handover(port.role)),
            "root handover {:?} outside corridor {:?}",
            port.pin,
            corridor.region
        );
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
        .map(|(&signal, use_)| {
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
            Trunk {
                signal: signal.to_owned(),
                source,
                sinks,
            }
        })
        .collect();

    Ok(AllocationPlan {
        contract: SignalContract {
            polarity: SignalPolarity::Positive,
            strength: 15,
            delay_budget_ticks: limits.delay_budget_ticks,
        },
        root_ports,
        children,
        corridor,
        trunks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let mut expected = vec![
            chunk_with_output(&chunks, "b").id.clone(),
            chunk_with_output(&chunks, "c").id.clone(),
        ];
        expected.sort();
        let sinks: Vec<TrunkOwner> = a.sinks.iter().map(|s| s.owner.clone()).collect();
        let expected: Vec<TrunkOwner> = expected.into_iter().map(TrunkOwner::Child).collect();
        assert_eq!(sinks, expected);
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
