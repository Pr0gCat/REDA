//! Leaf synthesis of one chunk inside its allocated contract.
//!
//! Runs the shipping planner and verifier on the chunk's netlist with the
//! child's **local** portal pins, realises the candidate into a world sized
//! exactly to the local envelope, then refuses any occupied cell the parent
//! did not allocate.  The planner already refuses primitives and route
//! anchors outside the world (`World::set` would drop them silently); the
//! scan here closes the rest: cells inside the world but in the parent-owned
//! `x = 0` column or `z = 0` caller row.  The far and top halo faces lie
//! outside this exact-size world and are refused by the planner.
//!
//! No retries, no repair, no second placer or router.

// Crate-private until the public synthesis API unfreezes at Gate 3.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::allocation::{ChildAllocation, Prism, SignalContract};
use crate::compile::fragment_synth::candidate::{CandidateError, ExpandedPhysicalCandidate};
use crate::compile::fragment_synth::certification::{CompleteCandidateCertifier, RootCertificate};
use crate::compile::fragment_synth::config::SearchConfig;
use crate::compile::fragment_synth::identity::{PhysicalEndpointId, PortId};
use crate::compile::fragment_synth::partition::{Chunk, ChunkId};
use crate::compile::fragment_synth::seed::{
    compile_parent_connectable_seed_with_services, SeedError, SeedInput, SeedServices,
};
use crate::compile::fragment_synth::placement::{
    PitchedSeedPlacer, PlacementGuide, STANDARD_PITCH,
};
use crate::compile::fragment_synth::services::{
    DurableSeedEmitter, DurableSeedVerifier,
};
use crate::compile::fragment_synth::terminal_geometry::{
    runway_core, terminal_access_cells_from, terminal_guard_cells_from,
};
use crate::compile::geometry::{Anchor, CellFacing};
use crate::compile::planner::{self, PlannerError, PortPin, PortRole};
use crate::compile::routing::{keep_out_typed, DurablePhysicalRouter};
use crate::compile::topology::Library;
use crate::compile::Netlist;
use crate::redstone::world::block::{BlockKind, Facing};
use crate::redstone::world::storage::World;

/// One compiled chunk in its local frame; the parent translates it by
/// [`ChildAllocation::origin`] when composing.
#[derive(Debug, Clone)]
pub struct LeafArtifact {
    pub chunk: ChunkId,
    /// Local world, `(0, 0, 0)` at the allocation's origin, spanning the
    /// local region plus the halo column and caller row.
    pub world: World,
    /// Local gate output cells, retained for the ordinary compiled-circuit
    /// compatibility view after the parent translates this leaf.
    pub gate_output_positions: BTreeMap<String, (i32, i32, i32)>,
    /// Gate facings in this chunk's netlist order.
    pub gate_facings: Vec<CellFacing>,
}

/// Stable typed identity of an automatically placed child boundary.
///
/// The endpoint is derived from the chunk-local declaration order, which the
/// partitioner canonicalises.  It deliberately does not depend on placement.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FreeLeafInterfaceId {
    pub chunk: ChunkId,
    pub endpoint: PhysicalEndpointId,
}

/// One caller-owned cell a future parent may connect to without changing the
/// certified child.  `pin.handover(role)` remains owned by the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentConnectableInterface {
    pub signal: String,
    pub role: PortRole,
    pub pin: PortPin,
    pub contract: SignalContract,
}

/// A self-contained, certified leaf before a parent assigns it a position.
///
/// `occupied` is exact. `halo` includes every existing router keep-out,
/// energising reach, stair clearance, and caller access cell; translation-only
/// packing must keep those masks disjoint. `access` names the terminal core
/// and two-cell runway the selected parent route may release; guard columns
/// and coupling rings remain halo-only.
#[derive(Debug, Clone)]
pub(crate) struct FreeLeafArtifact {
    /// Stable provenance: a [`ChunkId`] is the fingerprint of the parent
    /// identity, the exact ordered boundary interface, and the membership this
    /// leaf was compiled from, so it is what a parent matches an artifact to a
    /// node's child by -- never caller order or position in a vector.
    pub chunk: ChunkId,
    /// What this leaf certified, cloned from the chunk.
    ///
    /// A parent needs the logical subset, not only the boundary: the trunks it
    /// derives come from the interfaces, but certifying the composed node
    /// against the node's own netlist is only honest if the children's gates
    /// are provably that netlist's gates, once each.  Nothing else here can
    /// answer that -- the interfaces name boundary signals, and an internal
    /// gate never reaches a boundary.
    pub netlist: Netlist,
    /// This world is accepted by the complete candidate certifier before the
    /// artifact is returned.
    pub world: World,
    pub interfaces: BTreeMap<FreeLeafInterfaceId, ParentConnectableInterface>,
    pub occupied: BTreeSet<Anchor>,
    pub halo: BTreeSet<Anchor>,
    pub access: BTreeSet<Anchor>,
    /// Where this artifact's gates stand in its own frame.
    pub gates: GateMetadata,
    /// The whole-world certificate this artifact's world was accepted under,
    /// when one exists.
    ///
    /// A leaf carries `None`: its proof is the expanded-candidate certificate
    /// the seed already enforced, a different authority with a different type,
    /// and nothing downstream reads it. A packed node carries `Some`, because
    /// the world it hands on is exactly the world
    /// [`certify_root_world`](crate::compile::fragment_synth::certification::certify_root_world)
    /// accepted -- which is what stops a composition relabelling an
    /// uncertified world as a child.
    pub certificate: Option<RootCertificate>,
}

#[derive(Debug, Error)]
pub(crate) enum FreeLeafError {
    #[error("parent-connectable seed failed: {0}")]
    Seed(#[from] SeedError),
    #[error("{role:?} port {signal} has endpoint {actual:?}, expected {expected:?}")]
    InterfaceBinding {
        signal: String,
        role: PortRole,
        expected: PhysicalEndpointId,
        actual: Option<PhysicalEndpointId>,
    },
    #[error("{role:?} port {signal} has no typed pin contract")]
    MissingInterfacePin { signal: String, role: PortRole },
    #[error("chunk has more than u32::MAX ports")]
    PortIndexOverflow,
    #[error("the certified world is empty")]
    EmptyWorld,
    #[error("the certified candidate has no compatibility view: {0}")]
    Compatibility(#[from] CandidateError),
    #[error("{signal}'s runway is crowded: the leaf's own conductor at {at:?} stands inside the ring a parent route keeps")]
    CrowdedRunway { signal: String, at: Anchor },
}

/// Every terminal's runway is the parent's to route along ([`runway_core`]):
/// no conductor of this leaf but the terminal's own may stand on it or in the
/// ring a route keeps round it ([`keep_out_typed`]), or a parent route out of
/// the terminal is refused before it has left.
fn runways_clear(
    world: &World,
    interfaces: &BTreeMap<FreeLeafInterfaceId, ParentConnectableInterface>,
) -> Result<(), FreeLeafError> {
    for interface in interfaces.values() {
        let facing = interface_route_direction(interface);
        let core = runway_core(interface.pin.at, facing);
        let own = runway_core(interface.pin.at, facing.opposite())[1];
        for cell in &core {
            for at in std::iter::once(*cell).chain(keep_out_typed(*cell)) {
                if at == own || core.contains(&at) || world.index(at.x, at.y, at.z).is_none() {
                    continue;
                }
                if matches!(
                    world.get(at.x, at.y, at.z).kind,
                    BlockKind::RedstoneWire
                        | BlockKind::Repeater
                        | BlockKind::Comparator
                        | BlockKind::Torch
                        | BlockKind::WallTorch
                        | BlockKind::RedstoneBlock
                        | BlockKind::Observer
                ) {
                    return Err(FreeLeafError::CrowdedRunway {
                        signal: interface.signal.clone(),
                        at,
                    });
                }
            }
        }
    }
    Ok(())
}

/// The grids a leaf is placed on, densest first.
///
/// Four cells is the tightest grid every measured leaf still certifies on
/// (three refuses a 23-gate `segment_a` leaf); it packs the same gates into
/// half to two thirds of the standard grid's footprint. A leaf that will not
/// certify on it -- acceptance is not monotone in the pitch -- is placed on
/// the standard grid exactly as before.
pub(crate) const LEAF_PITCHES: [i32; 2] = [4, STANDARD_PITCH];

/// Compile a leaf in its own frame, then expose its automatic terminal cells
/// to a future parent. Existing fixed-allocation synthesis remains separate.
///
/// Each grid in `pitches` is tried in order, and the first leaf that
/// certifies ships; the last grid's refusal is the leaf's.
pub(crate) fn synthesise_free_leaf(
    chunk: &Chunk,
    contract: SignalContract,
    search: &SearchConfig,
    pitches: &[i32],
) -> Result<FreeLeafArtifact, FreeLeafError> {
    synthesise_free_leaf_timed(chunk, contract, search, pitches, None, false)
}

/// [`synthesise_free_leaf`], placed for timing when `critical` is given: the
/// boundary signals of the whole circuit the root found on its critical path,
/// by name. This leaf reads the ones that cross its own boundary, and places
/// its own critical chain straight ([`PlacementGuide`]); with `arrival`, it
/// anchors each column by estimated arrival instead
/// ([`PlacementGuide::arrival`]). `None` is the placement every leaf always
/// had, whatever `arrival` says.
pub(crate) fn synthesise_free_leaf_timed(
    chunk: &Chunk,
    contract: SignalContract,
    search: &SearchConfig,
    pitches: &[i32],
    critical: Option<&BTreeSet<String>>,
    arrival: bool,
) -> Result<FreeLeafArtifact, FreeLeafError> {
    let guide = leaf_placement_guide(chunk, critical, arrival);
    let mut refusal = None;
    for &pitch in pitches {
        match synthesise_free_leaf_at(chunk, contract, search, pitch, &guide) {
            Ok(leaf) => return Ok(leaf),
            Err(error) => refusal = Some(error),
        }
    }
    Err(refusal.expect("a leaf is tried on at least one grid"))
}

/// The guide [`synthesise_free_leaf_timed`] places `chunk` with: the
/// critical boundary signals it reads or drives, by port, when `critical` is
/// given, and the default placement otherwise.
pub(crate) fn leaf_placement_guide(
    chunk: &Chunk,
    critical: Option<&BTreeSet<String>>,
    arrival: bool,
) -> PlacementGuide {
    critical.map_or_else(PlacementGuide::default, |critical| {
        let ports = |names: &[String]| -> BTreeSet<PortId> {
            names
                .iter()
                .enumerate()
                .filter(|(_, name)| critical.contains(*name))
                .filter_map(|(index, _)| u32::try_from(index).ok().map(PortId))
                .collect()
        };
        PlacementGuide {
            timing: true,
            arrival,
            critical_inputs: ports(&chunk.netlist.inputs),
            critical_outputs: ports(&chunk.netlist.outputs),
        }
    })
}

fn synthesise_free_leaf_at(
    chunk: &Chunk,
    contract: SignalContract,
    search: &SearchConfig,
    pitch: i32,
    guide: &PlacementGuide,
) -> Result<FreeLeafArtifact, FreeLeafError> {
    let library = Library::default_library();
    let certified = compile_parent_connectable_seed_with_services(
        SeedInput {
            lowered: &chunk.netlist,
            source_provenance: None,
            pins: None,
        },
        SeedServices {
            library: &library,
            placer: &PitchedSeedPlacer(pitch, guide.clone()),
            router: &DurablePhysicalRouter,
            emitter: &DurableSeedEmitter,
            verifier: &DurableSeedVerifier,
            certifier: &CompleteCandidateCertifier,
            search_config: search,
        },
    )?;
    let interfaces = free_leaf_interfaces(chunk, certified.candidate(), contract)?;
    let world = certified.world().clone();
    runways_clear(&world, &interfaces)?;
    let masks = parent_connectable_masks(&world, &interfaces).ok_or(FreeLeafError::EmptyWorld)?;
    let gates = GateMetadata::from_candidate(certified.candidate(), &chunk.netlist)?;

    Ok(FreeLeafArtifact {
        chunk: chunk.id.clone(),
        netlist: chunk.netlist.clone(),
        world,
        interfaces,
        gates,
        occupied: masks.occupied,
        halo: masks.halo,
        access: masks.access,
        // A leaf's proof is the expanded-candidate certificate the seed
        // enforced above, which is a different authority from the whole-world
        // one a packed node answers to. It is not carried here because nothing
        // reads it; what carries one is a node, and this says which is which.
        certificate: None,
    })
}

/// Where every gate stands and which way it faces, keyed by the gate's own
/// logical output signal.
///
/// The key is the output signal rather than a position in a vector because a
/// packed parent has no netlist order to index into: its gates arrive from
/// several children at once, and the only name they all agree on is the
/// signal. `RecursiveProduct` wants `gate_output_positions` in exactly this
/// shape already; the facings it wants as a vector are recovered by reading
/// this map in the node netlist's gate order.
///
/// Nothing here is ever recovered by scanning blocks. A leaf's entries come
/// from the certified candidate that placed them, and every later frame is
/// that entry translated -- once per level, by the packing translation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GateMetadata {
    pub output_positions: BTreeMap<String, Anchor>,
    pub facings: BTreeMap<String, CellFacing>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum GateMetadataError {
    #[error("gate {gate} is placed by more than one child")]
    DuplicateGate { gate: String },
    #[error("gate {gate} has a position but no facing")]
    FacingWithoutPosition { gate: String },
    #[error("translating gate {gate} at {at:?} by {by:?} overflows an i32")]
    CoordinateOverflow {
        gate: String,
        at: Anchor,
        by: Anchor,
    },
}

impl GateMetadata {
    /// The metadata a certified candidate placed, in that candidate's frame.
    pub fn from_candidate(
        candidate: &ExpandedPhysicalCandidate,
        netlist: &Netlist,
    ) -> Result<Self, CandidateError> {
        let views = candidate.compatibility_views(netlist)?;
        let output_positions = views
            .gate_output_positions
            .into_iter()
            .map(|(gate, (x, y, z))| (gate, Anchor { x, y, z }))
            .collect();
        let facings = netlist
            .gates
            .iter()
            .zip(views.gate_facings)
            .map(|(gate, facing)| (gate.output.clone(), facing))
            .collect();
        Ok(Self {
            output_positions,
            facings,
        })
    }

    /// Fold `child`, translated by `by`, into this frame.
    ///
    /// The single place a coordinate moves: a child's entry is translated
    /// exactly once, when its world is, so no frame can be applied twice and
    /// none can be missed.
    pub fn absorb(&mut self, child: &Self, by: Anchor) -> Result<(), GateMetadataError> {
        for (gate, at) in &child.output_positions {
            let facing = *child
                .facings
                .get(gate)
                .ok_or_else(|| GateMetadataError::FacingWithoutPosition { gate: gate.clone() })?;
            let overflow = || GateMetadataError::CoordinateOverflow {
                gate: gate.clone(),
                at: *at,
                by,
            };
            let translated = Anchor {
                x: at.x.checked_add(by.x).ok_or_else(overflow)?,
                y: at.y.checked_add(by.y).ok_or_else(overflow)?,
                z: at.z.checked_add(by.z).ok_or_else(overflow)?,
            };
            if self
                .output_positions
                .insert(gate.clone(), translated)
                .is_some()
            {
                return Err(GateMetadataError::DuplicateGate { gate: gate.clone() });
            }
            self.facings.insert(gate.clone(), facing);
        }
        Ok(())
    }

    /// Every gate of `netlist`, and nothing else.
    pub fn covers(&self, netlist: &Netlist) -> Result<(), GateCoverageError> {
        for gate in &netlist.gates {
            if !self.output_positions.contains_key(&gate.output) {
                return Err(GateCoverageError::Missing {
                    gate: gate.output.clone(),
                });
            }
        }
        let declared = netlist
            .gates
            .iter()
            .map(|gate| gate.output.as_str())
            .collect::<BTreeSet<_>>();
        for gate in self.output_positions.keys() {
            if !declared.contains(gate.as_str()) {
                return Err(GateCoverageError::Foreign { gate: gate.clone() });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum GateCoverageError {
    #[error("no child placed gate {gate}")]
    Missing { gate: String },
    #[error("gate {gate} is placed but not declared here")]
    Foreign { gate: String },
}

/// The masks a parent-connectable artifact exposes, derived from a finished
/// world and the interfaces it already carries.
pub(crate) struct ParentConnectableMasks {
    pub occupied: BTreeSet<Anchor>,
    pub halo: BTreeSet<Anchor>,
    pub access: BTreeSet<Anchor>,
}

/// Derive [`ParentConnectableMasks`] for `world`, or `None` if it is empty.
///
/// The one definition of what a packable artifact reserves, whether the world
/// came from the leaf router or from a packed node's own composition. A node
/// is a bigger world with more interfaces; it is not a different contract, so
/// it must not be a second copy of this derivation.
pub(crate) fn parent_connectable_masks(
    world: &World,
    interfaces: &BTreeMap<FreeLeafInterfaceId, ParentConnectableInterface>,
) -> Option<ParentConnectableMasks> {
    let occupied = occupied_cells(world);
    let caller_cells: BTreeSet<_> = interfaces
        .values()
        .map(|interface| interface.pin.at)
        .collect();
    let mut halo = conservative_halo(world, &occupied, &caller_cells);
    // Project through the complete local mask, rather than only the emitted
    // volume: energising and stair guards can already extend above it.
    let bottom = halo.iter().map(|at| at.y).min()?;
    let top = halo.iter().map(|at| at.y).max()?;
    let access: BTreeSet<_> = interfaces
        .values()
        .flat_map(|interface| {
            terminal_access_cells_from(
                interface.pin.at,
                interface_route_direction(interface),
                bottom,
                top,
            )
        })
        .collect();
    halo.extend(access.iter().copied());
    for interface in interfaces.values() {
        halo.extend(terminal_guard_cells_from(
            interface.pin.at,
            interface_route_direction(interface),
            bottom,
            top,
        ));
    }
    Some(ParentConnectableMasks {
        occupied,
        halo,
        access,
    })
}

/// The direction a parent route leaves or enters an interface along: out of an
/// output, into an input.  The single rule the leaf's own access columns, the
/// packed trunk router and the packed root boundary all read.
pub(crate) fn interface_route_direction(interface: &ParentConnectableInterface) -> Facing {
    match interface.role {
        PortRole::Output => interface.pin.toward,
        PortRole::Input => interface.pin.toward.opposite(),
    }
}

fn free_leaf_interfaces(
    chunk: &Chunk,
    candidate: &ExpandedPhysicalCandidate,
    contract: SignalContract,
) -> Result<BTreeMap<FreeLeafInterfaceId, ParentConnectableInterface>, FreeLeafError> {
    let mut interfaces = BTreeMap::new();
    for (role, signals) in [
        (PortRole::Input, &chunk.netlist.inputs),
        (PortRole::Output, &chunk.netlist.outputs),
    ] {
        for (index, signal) in signals.iter().enumerate() {
            let port = PortId(u32::try_from(index).map_err(|_| FreeLeafError::PortIndexOverflow)?);
            let expected = match role {
                PortRole::Input => PhysicalEndpointId::PrimaryInput(port),
                PortRole::Output => PhysicalEndpointId::DeclaredOutput(port),
            };
            let actual = candidate.pin_name_bindings.get(signal).copied();
            if actual != Some(expected) {
                return Err(FreeLeafError::InterfaceBinding {
                    signal: signal.clone(),
                    role,
                    expected,
                    actual,
                });
            }
            let pin = candidate
                .pin_contracts
                .get(&expected)
                .copied()
                .ok_or_else(|| FreeLeafError::MissingInterfacePin {
                    signal: signal.clone(),
                    role,
                })?;
            interfaces.insert(
                FreeLeafInterfaceId {
                    chunk: chunk.id.clone(),
                    endpoint: expected,
                },
                ParentConnectableInterface {
                    signal: signal.clone(),
                    role,
                    pin,
                    contract,
                },
            );
        }
    }
    Ok(interfaces)
}

fn occupied_cells(world: &World) -> BTreeSet<Anchor> {
    let (size_x, size_y, size_z) = world.size();
    let mut occupied = BTreeSet::new();
    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                if world.get(x, y, z).kind != BlockKind::Air {
                    occupied.insert(Anchor { x, y, z });
                }
            }
        }
    }
    occupied
}

/// Every cell another packed child must leave unused around this world.
///
/// This is the union of the router's 12-cell `+/-Y` horizontal keep-out, the
/// production coupling authority [`two_hop_coupling_offsets`] applied to every
/// emitted block, and both directions of every realised dust/repeater
/// staircase's clearance. Caller cells remain in this mask although their
/// explicit `access` view says the parent, rather than the child, routes
/// there.
fn conservative_halo(
    world: &World,
    occupied: &BTreeSet<Anchor>,
    access: &BTreeSet<Anchor>,
) -> BTreeSet<Anchor> {
    let mut halo = occupied.clone();
    halo.extend(access.iter().copied());
    for at in occupied {
        halo.extend(router_keep_out(*at));
        halo.extend(coupling_halo(*at));
    }
    for from in occupied.iter().copied().filter(|at| route_cell(world, *at)) {
        for horizontal in [Facing::North, Facing::South, Facing::East, Facing::West] {
            for vertical in [-1, 1] {
                let to = Anchor {
                    y: from.y + vertical,
                    ..neighbour(from, horizontal)
                };
                if occupied.contains(&to) && route_cell(world, to) {
                    halo.extend(staircase_clearance(from, to));
                }
            }
        }
    }
    halo
}

fn router_keep_out(at: Anchor) -> [Anchor; 12] {
    let mut cells = [at; 12];
    let mut index = 0;
    for facing in [Facing::West, Facing::East, Facing::North, Facing::South] {
        let side = neighbour(at, facing);
        for vertical in [-1, 0, 1] {
            cells[index] = Anchor {
                y: side.y + vertical,
                ..side
            };
            index += 1;
        }
    }
    cells
}

/// **The production coupling authority: every offset an emitted block could
/// energise, by the shape of the two-hop relation rather than by its
/// measurements.**
///
/// `compile::energising` reads the two derived artifacts and answers, per block
/// kind and per facing, exactly which of those offsets were measured to couple
/// and which the rig could not ask about. It is `#[cfg(test)]` on purpose --
/// its own module doc says "nothing here is called by `compile`, and it is
/// `#[cfg(test)]` so that is a property of the build rather than a promise in a
/// comment", and `planner::keep_out_against` records the two measurements that
/// stopped the last attempt to wire a *narrowed* range into production. A halo
/// this leaf hands a packer is production code, so it cannot read that module,
/// and narrowing it on the strength of those artifacts is the change those
/// notes say is not yet made.
///
/// What is left is the part of `energises` that is pure geometry and needs no
/// artifact at all: whatever the marks say, hop 1 can only land on one of the
/// six cardinal neighbours, and hop 2 can only land on a cardinal step out of
/// one of those six -- that is the fan `energises` performs, verbatim, for
/// every mark it does not read as clear. Taking **every** mark as coupled
/// gives the union over all kinds and all facings in closed form: the ball of
/// offsets at L1 distance 1 or 2, twenty-four cells.
///
/// So this is a superset of `energises(kind, facing).conservative()` for every
/// kind and every facing, by construction rather than by inspection, and
/// `tests::the_production_authority_covers_every_measured_range` holds it to
/// that against the artifacts themselves. It cannot panic, reads no file, and
/// does not depend on a block's facing being recorded -- three properties the
/// artifact reader does not have and a production halo needs.
///
/// The cost is stated rather than hidden: this is **wider** than the measured
/// range, so packed children stand further apart than a measurement-aware halo
/// would need. Narrowing it is the separate change with its own measurements
/// that `energising`'s module doc and `keep_out_against` both describe.
fn two_hop_coupling_offsets() -> impl Iterator<Item = (i32, i32, i32)> {
    (-2..=2_i32)
        .flat_map(|x| (-2..=2_i32).flat_map(move |y| (-2..=2_i32).map(move |z| (x, y, z))))
        .filter(|(x, y, z)| (1..=2).contains(&(x.abs() + y.abs() + z.abs())))
}

fn coupling_halo(at: Anchor) -> BTreeSet<Anchor> {
    two_hop_coupling_offsets()
        .map(|(x, y, z)| Anchor {
            x: at.x + x,
            y: at.y + y,
            z: at.z + z,
        })
        .collect()
}

fn route_cell(world: &World, at: Anchor) -> bool {
    matches!(
        world.get(at.x, at.y, at.z).kind,
        BlockKind::RedstoneWire | BlockKind::Repeater
    )
}

fn staircase_clearance(from: Anchor, to: Anchor) -> Vec<Anchor> {
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

fn neighbour(at: Anchor, facing: Facing) -> Anchor {
    match facing {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum LeafError {
    #[error("allocation is for chunk {allocated:?}, not {chunk:?}")]
    AllocationMismatch { chunk: ChunkId, allocated: ChunkId },
    #[error("planner refused the chunk: {0}")]
    Planner(#[source] PlannerError),
    #[error("{role:?} port {signal} realised at {actual:?}, expected local {expected:?}")]
    PortMismatch {
        signal: String,
        role: PortRole,
        expected: Anchor,
        actual: Option<(i32, i32, i32)>,
    },
    #[error("{kind:?} at local {at:?} lies outside the allocated region")]
    EscapedCell { at: Anchor, kind: BlockKind },
}

/// Compile `chunk` under `allocation`.
pub fn synthesise_leaf(
    chunk: &Chunk,
    allocation: &ChildAllocation,
) -> Result<LeafArtifact, LeafError> {
    if chunk.id != allocation.chunk {
        return Err(LeafError::AllocationMismatch {
            chunk: chunk.id.clone(),
            allocated: allocation.chunk.clone(),
        });
    }
    let placements = allocation.port_placements();
    let region = allocation.local_region();
    let size = (region.max.x + 1, region.max.y + 1, region.max.z + 1);

    let candidate =
        planner::plan_from_netlist(&chunk.netlist, &placements).map_err(LeafError::Planner)?;
    let realised = planner::realise_and_verify(&candidate, &chunk.netlist, size)
        .map_err(LeafError::Planner)?;
    for portal in &allocation.portals {
        let expected = allocation.to_local(portal.pin.at);
        let actual = match portal.role {
            PortRole::Input => realised.ports.input_positions.get(&portal.signal),
            PortRole::Output => realised.ports.output_positions.get(&portal.signal),
        }
        .copied();
        if actual != Some((expected.x, expected.y, expected.z)) {
            return Err(LeafError::PortMismatch {
                signal: portal.signal.clone(),
                role: portal.role,
                expected,
                actual,
            });
        }
    }
    refuse_escapes(&realised.world, &region)?;

    Ok(LeafArtifact {
        chunk: chunk.id.clone(),
        world: realised.world,
        gate_output_positions: realised.ports.gate_output_positions,
        gate_facings: (0..chunk.netlist.gates.len())
            .map(|gate| candidate.facing_of(gate))
            .collect(),
    })
}

/// The first occupied cell, in `(y, z, x)` scan order, outside `region`.
fn refuse_escapes(world: &World, region: &Prism) -> Result<(), LeafError> {
    let (sx, sy, sz) = world.size();
    for y in 0..sy {
        for z in 0..sz {
            for x in 0..sx {
                let kind = world.get(x, y, z).kind;
                let at = Anchor { x, y, z };
                if kind != BlockKind::Air && !region.contains(at) {
                    return Err(LeafError::EscapedCell { at, kind });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::{
        allocate, AllocationLimits, RegionMask, SignalContract,
    };
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::stone;
    use crate::compile::topology::SignalPolarity;
    use crate::compile::{Gate, Netlist};

    const LIMITS: AllocationLimits = AllocationLimits {
        delay_budget_ticks: 4,
        corridor_capacity: 8,
    };

    fn chain() -> Netlist {
        Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        }
    }

    fn allocated(net: &Netlist, max_gates: usize) -> (Vec<Chunk>, Vec<ChildAllocation>) {
        let chunks = partition(net, &root_chunk_id(net).unwrap(), max_gates).unwrap();
        let plan = allocate(net, &chunks, LIMITS).unwrap();
        (chunks, plan.children)
    }

    fn allocation_for<'a>(children: &'a [ChildAllocation], chunk: &Chunk) -> &'a ChildAllocation {
        children.iter().find(|c| c.chunk == chunk.id).unwrap()
    }

    #[test]
    fn pinned_chunks_compile_inside_their_local_region() {
        let net = chain();
        let (chunks, children) = allocated(&net, 1);
        for chunk in &chunks {
            let allocation = allocation_for(&children, chunk);
            let leaf = synthesise_leaf(chunk, allocation).unwrap();
            assert_eq!(leaf.chunk, chunk.id);
            let region = allocation.local_region();
            assert_eq!(
                leaf.world.size(),
                (region.max.x + 1, region.max.y + 1, region.max.z + 1)
            );
            assert!(refuse_escapes(&leaf.world, &region).is_ok());
        }
    }

    #[test]
    fn shrunken_region_is_refused_by_the_planner() {
        let net = chain();
        let (chunks, children) = allocated(&net, 1);
        let mut allocation = allocation_for(&children, &chunks[0]).clone();
        let mut bounds = allocation.region.bounds();
        bounds.max = Anchor {
            x: bounds.min.x + 1,
            y: bounds.max.y,
            z: bounds.min.z + 1,
        };
        allocation.region = RegionMask::Prism(bounds);
        assert!(matches!(
            synthesise_leaf(&chunks[0], &allocation),
            Err(LeafError::Planner(PlannerError::UnrealisableNode { .. }))
        ));
    }

    #[test]
    fn occupied_halo_cell_is_an_escape_with_its_coordinate() {
        let region = Prism {
            min: Anchor { x: 1, y: 0, z: 1 },
            max: Anchor { x: 3, y: 3, z: 3 },
        };
        let mut world = World::new(4, 4, 4);
        world.set(2, 1, 2, stone());
        assert_eq!(refuse_escapes(&world, &region), Ok(()));
        world.set(2, 1, 0, stone());
        assert_eq!(
            refuse_escapes(&world, &region),
            Err(LeafError::EscapedCell {
                at: Anchor { x: 2, y: 1, z: 0 },
                kind: BlockKind::Solid,
            })
        );
    }

    #[test]
    fn mismatched_chunk_is_refused() {
        let net = chain();
        let (chunks, children) = allocated(&net, 1);
        let wrong = allocation_for(&children, &chunks[1]);
        assert_eq!(
            synthesise_leaf(&chunks[0], wrong).err(),
            Some(LeafError::AllocationMismatch {
                chunk: chunks[0].id.clone(),
                allocated: chunks[1].id.clone(),
            })
        );
    }

    #[test]
    fn free_leaf_exposes_certified_interfaces_and_packing_masks() {
        let net = chain();
        let (chunks, _) = allocated(&net, 1);
        let contract = SignalContract {
            polarity: SignalPolarity::Positive,
            strength: 15,
            delay_budget_ticks: 4,
        };
        let artifact =
            synthesise_free_leaf(&chunks[0], contract, &SearchConfig::checked_defaults(), &LEAF_PITCHES).unwrap();

        assert_eq!(artifact.chunk, chunks[0].id);
        assert_eq!(artifact.occupied, occupied_cells(&artifact.world));
        assert!(!artifact.occupied.is_empty());
        assert!(artifact.halo.is_superset(&artifact.occupied));
        assert!(artifact.halo.is_superset(&artifact.access));
        assert_eq!(artifact.interfaces.len(), 2);
        let bottom = artifact.halo.iter().map(|at| at.y).min().unwrap();
        let top = artifact.halo.iter().map(|at| at.y).max().unwrap();
        for (id, interface) in &artifact.interfaces {
            assert_eq!(id.chunk, artifact.chunk);
            assert_eq!(interface.contract, contract);
            assert!(artifact.access.contains(&interface.pin.at));
            assert_eq!(
                artifact
                    .world
                    .get(interface.pin.at.x, interface.pin.at.y, interface.pin.at.z)
                    .kind,
                BlockKind::Air
            );
            assert!(artifact
                .halo
                .contains(&interface.pin.handover(interface.role)));
            let access = terminal_access_cells_from(
                interface.pin.at,
                interface_route_direction(interface),
                bottom,
                top,
            );
            assert!(access.iter().all(|at| artifact.access.contains(at)));
            let guard = terminal_guard_cells_from(
                interface.pin.at,
                interface_route_direction(interface),
                bottom,
                top,
            );
            let keep_out = guard
                .into_iter()
                .find(|at| !access.contains(at))
                .expect("terminal guard has a lateral or coupling cell");
            assert!(artifact.halo.contains(&keep_out));
            assert!(!artifact.access.contains(&keep_out));
        }
        assert!(artifact.interfaces.iter().any(|(id, interface)| {
            id.endpoint == PhysicalEndpointId::PrimaryInput(PortId(0))
                && interface.signal == "x"
                && interface.role == PortRole::Input
        }));
        assert!(artifact.interfaces.iter().any(|(id, interface)| {
            id.endpoint == PhysicalEndpointId::DeclaredOutput(PortId(0))
                && interface.signal == "a"
                && interface.role == PortRole::Output
        }));
    }

    /// The claim [`two_hop_coupling_offsets`]'s doc makes, held against the
    /// artifacts themselves: for every block kind the measurement can answer
    /// for, at every facing including none, the production authority contains
    /// the whole conservative range -- measured couplings *and* the offsets
    /// the rig could not ask about.
    ///
    /// This is what keeps `compile::energising`'s "this module only measures"
    /// true while a production halo still refuses everything it found: the
    /// production side derives its own superset, and this fails the moment the
    /// two stop agreeing.
    #[test]
    fn the_production_authority_covers_every_measured_range() {
        use crate::compile::energising;

        let production = two_hop_coupling_offsets().collect::<BTreeSet<_>>();
        let every_kind = [
            BlockKind::Air,
            BlockKind::Solid,
            BlockKind::Glass,
            BlockKind::Slab,
            BlockKind::RedstoneWire,
            BlockKind::Repeater,
            BlockKind::Comparator,
            BlockKind::Torch,
            BlockKind::WallTorch,
            BlockKind::Lever,
            BlockKind::RedstoneBlock,
            BlockKind::Lamp,
            BlockKind::Piston,
            BlockKind::Button,
            BlockKind::PressurePlate,
            BlockKind::WeightedPressurePlate,
            BlockKind::Observer,
            BlockKind::Target,
            BlockKind::DaylightDetector,
            BlockKind::Other,
        ];
        let mut answered = 0;
        for kind in every_kind {
            // The artifact prints four rows for a facing-sensitive kind, and
            // they are the four horizontal ones -- a repeater facing up is not
            // a block this compiler writes, and asking for its row is asking
            // the artifact a question it never measured.
            let vertical_is_answerable = !matches!(
                kind,
                BlockKind::Repeater | BlockKind::Comparator | BlockKind::WallTorch
            );
            for facing in [
                None,
                Some(Facing::North),
                Some(Facing::South),
                Some(Facing::East),
                Some(Facing::West),
                Some(Facing::Up),
                Some(Facing::Down),
            ]
            .into_iter()
            .filter(|facing| {
                vertical_is_answerable || !matches!(facing, Some(Facing::Up) | Some(Facing::Down))
            }) {
                let measured = energising::energises(kind, facing).conservative();
                answered += usize::from(!measured.is_empty());
                assert!(
                    measured.is_subset(&production),
                    "{kind:?}/{facing:?} couples outside the production authority: {:?}",
                    measured.difference(&production).collect::<Vec<_>>()
                );
            }
        }
        assert!(
            answered > 0,
            "the artifacts must answer for something, or this proves nothing"
        );

        // The other half of the keep-out question the halo has to answer, from
        // the other artifact, is inside it too.
        assert!(energising::dust_join_offsets().is_subset(&production));
        // And the router's own twelve-cell rule, which the halo also unions in.
        let origin = Anchor { x: 0, y: 0, z: 0 };
        assert!(router_keep_out(origin)
            .into_iter()
            .all(|at| production.contains(&(at.x, at.y, at.z))));
    }

    /// The closed form, stated once so a change to it is visible: the ball of
    /// offsets one or two cardinal steps from the emitter, and nothing else.
    #[test]
    fn the_production_authority_is_the_two_step_ball() {
        let production = two_hop_coupling_offsets().collect::<BTreeSet<_>>();

        assert_eq!(production.len(), 24);
        assert!(!production.contains(&(0, 0, 0)));
        assert!(production.contains(&(0, 0, 2)));
        assert!(production.contains(&(1, 1, 0)));
        assert!(!production.contains(&(1, 1, 1)));
        assert!(!production.contains(&(0, 0, 3)));
        assert!(production
            .iter()
            .all(|(x, y, z)| (1..=2).contains(&(x.abs() + y.abs() + z.abs()))));
    }

    #[test]
    fn packing_halo_reserves_energising_unknowns_and_stair_clearance() {
        let mut world = World::new(32, 12, 32);
        let repeater_at = Anchor { x: 10, y: 5, z: 10 };
        let repeater = crate::compile::repeater(Facing::North);
        let range = crate::compile::energising::energises(repeater.kind, repeater.facing);
        let hop_two = *range
            .hop2
            .iter()
            .next()
            .expect("repeater has hop-two reach");
        let unknown = *range
            .unmeasured
            .iter()
            .next()
            .expect("repeater has an unmeasured rear");
        world.set(repeater_at.x, repeater_at.y, repeater_at.z, repeater);

        let rising_from = Anchor { x: 20, y: 5, z: 20 };
        let rising_to = Anchor { x: 21, y: 6, z: 20 };
        let falling_from = Anchor { x: 24, y: 6, z: 20 };
        let falling_to = Anchor { x: 25, y: 5, z: 20 };
        for at in [rising_from, rising_to, falling_from, falling_to] {
            world.set(at.x, at.y, at.z, crate::compile::dust());
        }

        let occupied = occupied_cells(&world);
        let halo = conservative_halo(&world, &occupied, &BTreeSet::new());
        for (x, y, z) in [hop_two, unknown] {
            assert!(halo.contains(&Anchor {
                x: repeater_at.x + x,
                y: repeater_at.y + y,
                z: repeater_at.z + z,
            }));
        }
        assert!(halo.contains(&Anchor {
            x: rising_from.x,
            y: rising_from.y + 1,
            z: rising_from.z,
        }));
        assert!(halo.contains(&Anchor {
            x: falling_to.x,
            y: falling_from.y,
            z: falling_to.z,
        }));
    }
}
