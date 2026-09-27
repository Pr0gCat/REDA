//! Deterministic translation-only placement for independently certified leaves.
//!
//! Leaves are ordered by their stable [`ChunkId`], never by caller order. Each
//! leaf keeps its local orientation. Candidate translations are the origin,
//! right/top contacts with already reserved halo cells, and a guaranteed
//! side-by-side strip fallback. The smallest resulting parent envelope wins
//! by a stable score. The frame bounds are sums of halo spans, so the strip
//! fallback is always in range when the masks are valid.

// Crate-private until the public synthesis API unfreezes at Gate 3, exactly as
// `leaf` and `parent` are: every entry point here is reached from those two
// modules' tests and from `recursive`, so a non-test build sees no caller.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::fragment_synth::config::SearchConfig;
use crate::compile::fragment_synth::leaf::{
    interface_route_direction, FreeLeafArtifact, FreeLeafInterfaceId, ParentConnectableInterface,
};
use crate::compile::fragment_synth::partition::ChunkId;
use crate::compile::fragment_synth::terminal_geometry::{runway_core, TERMINAL_RUNWAY_CELLS};
use crate::compile::geometry::Anchor;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

/// Inclusive bounds of the parent frame available to a packing run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PackingLimits {
    pub max: Anchor,
}

/// **An explicit empty band between two children's halos, along `x`.**
///
/// Translation-only packing puts each halo in exact contact with the one
/// before it, so a parent trunk crossing the seam has no room at terminal
/// height and climbs over the halo lid instead. A band is parent-owned empty
/// space the packer leaves between two specific halos: `width` columns, no
/// child cell in them, so a trunk can cross at the height its terminals sit
/// on. Keyed by the unordered pair of children, because the width is a
/// property of the demand between those two, and zero for every pair not
/// named. [`SeamBands::none`] is the packing that has always existed, and
/// every legacy entry point uses it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SeamBands {
    widths: BTreeMap<(ChunkId, ChunkId), i32>,
}

impl SeamBands {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// Name the band between `a` and `b`; a non-positive width names none.
    pub(crate) fn set(&mut self, a: &ChunkId, b: &ChunkId, width: i32) {
        if width > 0 {
            self.widths.insert(Self::key(a, b), width);
        }
    }

    pub(crate) fn width(&self, a: &ChunkId, b: &ChunkId) -> i32 {
        self.widths.get(&Self::key(a, b)).copied().unwrap_or(0)
    }

    pub(crate) fn is_none(&self) -> bool {
        self.widths.is_empty()
    }

    /// The widest band any pair asks for.
    pub(crate) fn max_width(&self) -> i32 {
        self.widths.values().copied().max().unwrap_or(0)
    }

    fn key(a: &ChunkId, b: &ChunkId) -> (ChunkId, ChunkId) {
        if a <= b {
            (a.clone(), b.clone())
        } else {
            (b.clone(), a.clone())
        }
    }
}

/// One free leaf after its local frame has been translated into its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedFreeLeaf {
    pub chunk: ChunkId,
    pub translation: Anchor,
    pub interfaces: BTreeMap<FreeLeafInterfaceId, ParentConnectableInterface>,
    pub occupied: BTreeSet<Anchor>,
    pub halo: BTreeSet<Anchor>,
    pub access: BTreeSet<Anchor>,
}

/// Deterministic placement result, keyed by stable chunk identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackedFreeLeaves {
    pub placements: BTreeMap<ChunkId, PackedFreeLeaf>,
    pub halo: BTreeSet<Anchor>,
}

/// The emitted child blocks after deterministic translation-only packing.
///
/// Parent routes are deliberately absent: halo and access cells are
/// reservations, not child-world blocks.
#[derive(Debug, Clone)]
pub(crate) struct PackedChildWorld {
    pub world: World,
    pub occupied: BTreeSet<Anchor>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum PackingError {
    #[error("free leaf {chunk:?} has an empty halo mask")]
    EmptyHalo { chunk: ChunkId },
    #[error("free leaf {chunk:?} has an access cell outside its halo")]
    AccessOutsideHalo { chunk: ChunkId },
    #[error("free leaf {chunk:?} has an occupied cell outside its halo")]
    OccupiedOutsideHalo { chunk: ChunkId },
    #[error("free leaf {chunk:?} has interface {endpoint:?} owned by {interface_chunk:?}")]
    InterfaceChunkMismatch {
        chunk: ChunkId,
        interface_chunk: ChunkId,
        endpoint: crate::compile::fragment_synth::identity::PhysicalEndpointId,
    },
    #[error("free leaf {chunk:?} has interface {endpoint:?} outside its access cells")]
    InterfaceOutsideAccess {
        chunk: ChunkId,
        endpoint: crate::compile::fragment_synth::identity::PhysicalEndpointId,
    },
    #[error("free leaf {chunk:?} occurs more than once")]
    DuplicateChunk { chunk: ChunkId },
    #[error("packing limits must be non-negative: {max:?}")]
    InvalidLimits { max: Anchor },
    #[error("the frame {limits:?} cannot hold {chunk:?} with a {band}-column band beside it")]
    NoBandSpace {
        chunk: ChunkId,
        band: i32,
        limits: PackingLimits,
    },
    #[error("packing {chunk:?} exceeds the finite parent frame {limits:?}")]
    NoPlacement {
        chunk: ChunkId,
        limits: PackingLimits,
    },
    #[error("packing {chunk:?} overflows an i32 coordinate")]
    CoordinateOverflow { chunk: ChunkId },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum PackedWorldCompositionError {
    #[error("free leaf {chunk:?} occurs more than once")]
    DuplicateArtifact { chunk: ChunkId },
    #[error("packed child world is missing a placement for {chunk:?}")]
    MissingPlacement { chunk: ChunkId },
    #[error("packed child world has no artifact for placement {chunk:?}")]
    ExtraPlacement { chunk: ChunkId },
    #[error("placement key {key:?} contains leaf {leaf:?}")]
    PlacementIdentityMismatch { key: ChunkId, leaf: ChunkId },
    #[error("translating {chunk:?} at {at:?} overflows an i32 coordinate")]
    CoordinateOverflow {
        chunk: ChunkId,
        at: Anchor,
        translation: Anchor,
    },
    #[error("translated {chunk:?} has a negative coordinate at {at:?}")]
    NegativeCoordinate { chunk: ChunkId, at: Anchor },
    #[error("packed occupied mask for {chunk:?} does not match its translation")]
    PlacementOccupiedMismatch {
        chunk: ChunkId,
        expected: BTreeSet<Anchor>,
        actual: BTreeSet<Anchor>,
    },
    #[error("world blocks for {chunk:?} do not match its occupied mask")]
    WorldOccupiedMismatch {
        chunk: ChunkId,
        expected: BTreeSet<Anchor>,
        actual: BTreeSet<Anchor>,
    },
    #[error("translated children {first:?} and {second:?} both occupy {at:?}")]
    Collision {
        first: ChunkId,
        second: ChunkId,
        at: Anchor,
    },
    #[error("translated {chunk:?} requires a world dimension larger than i32")]
    WorldSizeOverflow { chunk: ChunkId },
}

/// Translate every certified child's non-air blocks into its packed frame.
///
/// Input order cannot affect this result: both validation and writes use the
/// stable chunk identity.  It intentionally emits neither halo nor access
/// reservations; those belong to the future parent router.
pub(crate) fn compose_packed_free_leaf_worlds(
    artifacts: &[FreeLeafArtifact],
    packed: &PackedFreeLeaves,
) -> Result<PackedChildWorld, PackedWorldCompositionError> {
    let mut by_chunk = BTreeMap::new();
    for artifact in artifacts {
        if by_chunk.insert(artifact.chunk.clone(), artifact).is_some() {
            return Err(PackedWorldCompositionError::DuplicateArtifact {
                chunk: artifact.chunk.clone(),
            });
        }
    }
    for (key, placement) in &packed.placements {
        if !by_chunk.contains_key(key) {
            return Err(PackedWorldCompositionError::ExtraPlacement { chunk: key.clone() });
        }
        if placement.chunk != *key {
            return Err(PackedWorldCompositionError::PlacementIdentityMismatch {
                key: key.clone(),
                leaf: placement.chunk.clone(),
            });
        }
    }

    let mut writes = Vec::new();
    let mut owners = BTreeMap::new();
    let mut occupied = BTreeSet::new();
    let mut maximum = Anchor { x: 0, y: 0, z: 0 };
    for (chunk, artifact) in by_chunk {
        let placement = packed.placements.get(&chunk).ok_or_else(|| {
            PackedWorldCompositionError::MissingPlacement {
                chunk: chunk.clone(),
            }
        })?;
        let expected = translate_occupied(&artifact.occupied, placement.translation, &chunk)?;
        if placement.occupied != expected {
            return Err(PackedWorldCompositionError::PlacementOccupiedMismatch {
                chunk,
                expected,
                actual: placement.occupied.clone(),
            });
        }
        let actual = translated_world_blocks(&artifact.world, placement.translation, &chunk)?;
        let actual_occupied = actual.iter().map(|(at, _)| *at).collect();
        if actual_occupied != expected {
            return Err(PackedWorldCompositionError::WorldOccupiedMismatch {
                chunk,
                expected,
                actual: actual_occupied,
            });
        }
        for (at, state) in actual {
            if let Some(first) = owners.insert(at, chunk.clone()) {
                return Err(PackedWorldCompositionError::Collision {
                    first,
                    second: chunk,
                    at,
                });
            }
            if at.x == i32::MAX || at.y == i32::MAX || at.z == i32::MAX {
                return Err(PackedWorldCompositionError::WorldSizeOverflow {
                    chunk: chunk.clone(),
                });
            }
            maximum.x = maximum.x.max(at.x);
            maximum.y = maximum.y.max(at.y);
            maximum.z = maximum.z.max(at.z);
            occupied.insert(at);
            writes.push((at, state));
        }
    }

    let (size_x, size_y, size_z) = (maximum.x + 1, maximum.y + 1, maximum.z + 1);
    let mut world = World::new(size_x, size_y, size_z);
    for (at, state) in writes {
        world.set(at.x, at.y, at.z, state);
    }
    Ok(PackedChildWorld { world, occupied })
}

fn translate_occupied(
    cells: &BTreeSet<Anchor>,
    translation: Anchor,
    chunk: &ChunkId,
) -> Result<BTreeSet<Anchor>, PackedWorldCompositionError> {
    cells
        .iter()
        .copied()
        .map(|at| translate_composed_anchor(at, translation, chunk))
        .collect()
}

fn translated_world_blocks(
    world: &World,
    translation: Anchor,
    chunk: &ChunkId,
) -> Result<Vec<(Anchor, BlockState)>, PackedWorldCompositionError> {
    let (size_x, size_y, size_z) = world.size();
    let mut blocks = Vec::new();
    for y in 0..size_y {
        for z in 0..size_z {
            for x in 0..size_x {
                let state = world.get(x, y, z);
                if state.kind != BlockKind::Air {
                    blocks.push((
                        translate_composed_anchor(Anchor { x, y, z }, translation, chunk)?,
                        state.clone(),
                    ));
                }
            }
        }
    }
    Ok(blocks)
}

fn translate_composed_anchor(
    at: Anchor,
    translation: Anchor,
    chunk: &ChunkId,
) -> Result<Anchor, PackedWorldCompositionError> {
    let translated = Anchor {
        x: at.x.checked_add(translation.x).ok_or_else(|| {
            PackedWorldCompositionError::CoordinateOverflow {
                chunk: chunk.clone(),
                at,
                translation,
            }
        })?,
        y: at.y.checked_add(translation.y).ok_or_else(|| {
            PackedWorldCompositionError::CoordinateOverflow {
                chunk: chunk.clone(),
                at,
                translation,
            }
        })?,
        z: at.z.checked_add(translation.z).ok_or_else(|| {
            PackedWorldCompositionError::CoordinateOverflow {
                chunk: chunk.clone(),
                at,
                translation,
            }
        })?,
    };
    if translated.x < 0 || translated.y < 0 || translated.z < 0 {
        return Err(PackedWorldCompositionError::NegativeCoordinate {
            chunk: chunk.clone(),
            at: translated,
        });
    }
    Ok(translated)
}

/// Pack leaves in stable chunk order with a derived finite frame.
pub(crate) fn pack_free_leaves(
    artifacts: &[FreeLeafArtifact],
) -> Result<PackedFreeLeaves, PackingError> {
    pack_free_leaves_with_bands(artifacts, &SeamBands::none())
}

/// [`pack_free_leaves`] with explicit empty bands between named pairs of
/// halos. With [`SeamBands::none`] this is `pack_free_leaves`, cell for cell.
pub(crate) fn pack_free_leaves_with_bands(
    artifacts: &[FreeLeafArtifact],
    bands: &SeamBands,
) -> Result<PackedFreeLeaves, PackingError> {
    let ordered = canonical_artifacts(artifacts)?;
    let limits = PackingLimits::for_ordered(&ordered, bands)?;
    pack_ordered(&ordered, limits, bands)
}

impl PackingLimits {
    /// The frame every child fits in side by side, plus the widest band once
    /// per seam. Which children end up adjacent is the packer's decision, so
    /// the frame allows the widest band at every seam and the layout is
    /// judged inside it; a wider frame than a layout uses costs nothing, the
    /// canvas is derived from what was placed.
    fn for_ordered(
        artifacts: &[&FreeLeafArtifact],
        bands: &SeamBands,
    ) -> Result<Self, PackingError> {
        if artifacts.is_empty() {
            return Ok(Self {
                max: Anchor { x: 0, y: 0, z: 0 },
            });
        }
        let mut width = 0_i64;
        let mut max_y = 0_i64;
        let mut max_z = 0_i64;
        let seams = i64::try_from(artifacts.len().saturating_sub(1)).unwrap_or(i64::MAX);
        width = width
            .checked_add(seams.saturating_mul(i64::from(bands.max_width())))
            .ok_or_else(|| PackingError::CoordinateOverflow {
                chunk: artifacts[0].chunk.clone(),
            })?;
        for artifact in artifacts {
            validate_mask(artifact)?;
            let (min, max) = bounds(&artifact.halo).ok_or_else(|| PackingError::EmptyHalo {
                chunk: artifact.chunk.clone(),
            })?;
            width = width.checked_add(span(min.x, max.x)).ok_or_else(|| {
                PackingError::CoordinateOverflow {
                    chunk: artifact.chunk.clone(),
                }
            })?;
            max_y = max_y.max(span(min.y, max.y));
            max_z = max_z.checked_add(span(min.z, max.z)).ok_or_else(|| {
                PackingError::CoordinateOverflow {
                    chunk: artifact.chunk.clone(),
                }
            })?;
        }
        let max_coordinate = |extent: i64| -> Result<i32, PackingError> {
            i32::try_from(extent.saturating_sub(1)).map_err(|_| PackingError::CoordinateOverflow {
                chunk: artifacts
                    .first()
                    .expect("empty artifacts return above")
                    .chunk
                    .clone(),
            })
        };
        Ok(Self {
            max: Anchor {
                x: max_coordinate(width)?,
                y: max_coordinate(max_y)?,
                z: max_coordinate(max_z)?,
            },
        })
    }
}

/// Pack leaves into an explicit, inclusive parent frame.
pub(crate) fn pack_free_leaves_with_limits(
    artifacts: &[FreeLeafArtifact],
    limits: PackingLimits,
) -> Result<PackedFreeLeaves, PackingError> {
    pack_ordered(&canonical_artifacts(artifacts)?, limits, &SeamBands::none())
}

/// Which placed child each reserved halo cell belongs to, so a contact
/// candidate beside that cell knows whose band it must leave.
type ReservedOwners = BTreeMap<Anchor, ChunkId>;

fn pack_ordered(
    artifacts: &[&FreeLeafArtifact],
    limits: PackingLimits,
    bands: &SeamBands,
) -> Result<PackedFreeLeaves, PackingError> {
    if limits.max.x < 0 || limits.max.y < 0 || limits.max.z < 0 {
        return Err(PackingError::InvalidLimits { max: limits.max });
    }
    require_band_space(artifacts, limits, bands)?;

    let mut placements = BTreeMap::new();
    let mut reserved = BTreeSet::new();
    let mut owners = ReservedOwners::new();
    let mut band_columns = 0_i64;
    let mut band_cells = BTreeSet::new();
    for artifact in artifacts {
        validate_mask(artifact)?;
        let (min, max) = bounds(&artifact.halo).ok_or_else(|| PackingError::EmptyHalo {
            chunk: artifact.chunk.clone(),
        })?;
        let width = span(min.x, max.x);
        if span(min.y, max.y) > i64::from(limits.max.y) + 1
            || span(min.z, max.z) > i64::from(limits.max.z) + 1
            || width > i64::from(limits.max.x) + 1
        {
            return Err(PackingError::NoPlacement {
                chunk: artifact.chunk.clone(),
                limits,
            });
        }

        let mut best = None;
        for (translation, band) in candidate_translations(artifact, &reserved, &owners, bands)? {
            let halo = translate_set_for_chunk(&artifact.halo, translation, &artifact.chunk)?;
            if !inside_limits(&halo, limits)
                || halo
                    .iter()
                    .any(|at| reserved.contains(at) || band_cells.contains(at))
            {
                continue;
            }
            let score = envelope_score_with_band(&reserved, &halo, translation, band_columns, band);
            let discount = band_discount(&reserved, &halo, band);
            if best
                .as_ref()
                .is_none_or(|(best_score, _, _)| score < *best_score)
            {
                best = Some((score, translation, discount));
            }
        }
        let (translation, discount) = best
            .map(|(_, translation, discount)| (translation, discount))
            .ok_or_else(|| PackingError::NoPlacement {
                chunk: artifact.chunk.clone(),
                limits,
            })?;

        let placement = translate_artifact(artifact, translation)?;
        if discount > 0 {
            band_cells.extend(band_slab(
                &reserved,
                &placement.halo,
                i32::try_from(discount).unwrap_or(i32::MAX),
            ));
        }
        reserved.extend(placement.halo.iter().copied());
        if !bands.is_none() {
            owners.extend(
                placement
                    .halo
                    .iter()
                    .map(|at| (*at, artifact.chunk.clone())),
            );
        }
        band_columns = band_columns.saturating_add(discount);
        placements.insert(artifact.chunk.clone(), placement);
    }

    Ok(PackedFreeLeaves {
        placements,
        halo: reserved,
    })
}

/// A frame handed in explicitly has to hold every child side by side with
/// the widest band at every seam, or the band cannot exist and the packing
/// is refused for that rather than for a placement that quietly closed it.
fn require_band_space(
    artifacts: &[&FreeLeafArtifact],
    limits: PackingLimits,
    bands: &SeamBands,
) -> Result<(), PackingError> {
    if bands.is_none() || artifacts.is_empty() {
        return Ok(());
    }
    let band = bands.max_width();
    let mut needed = 0_i64;
    for artifact in artifacts {
        let (min, max) = bounds(&artifact.halo).ok_or_else(|| PackingError::EmptyHalo {
            chunk: artifact.chunk.clone(),
        })?;
        needed = needed.checked_add(span(min.x, max.x)).ok_or_else(|| {
            PackingError::CoordinateOverflow {
                chunk: artifact.chunk.clone(),
            }
        })?;
    }
    let seams = i64::try_from(artifacts.len() - 1).unwrap_or(i64::MAX);
    needed = needed
        .checked_add(seams.saturating_mul(i64::from(band)))
        .ok_or_else(|| PackingError::CoordinateOverflow {
            chunk: artifacts[0].chunk.clone(),
        })?;
    if needed > i64::from(limits.max.x) + 1 {
        return Err(PackingError::NoBandSpace {
            chunk: artifacts[artifacts.len() - 1].chunk.clone(),
            band,
            limits,
        });
    }
    Ok(())
}

/// How many layouts a repair may try, and how many placements it may test.
///
/// Both numbers come from limits the search configuration already carries;
/// neither is a new knob, and there is no constructor that takes a number.
/// The layout count is the fragment searcher's backtrack allowance, because a
/// layout retry is the same kind of bounded backtrack over the same kind of
/// placement decision. The expansion cap is the router's own node-expansion
/// ceiling, for the same reason a route search has one: it is the hard stop
/// that makes an otherwise exponential walk finite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PackingBudget {
    layouts: u64,
    expansions: u64,
    /// The widest plan-view (x, z) span a layout may take, `None` along an
    /// unbounded axis. Span is the same wherever the layout ends up, so a
    /// prefix already wider than this is cut before its children are placed.
    span: (Option<i32>, Option<i32>),
    /// Move a child whose terminal would stand on another signal's runway or
    /// mouth, or drop that placement: both routes need the cell. Off, rank
    /// zero is exactly [`pack_free_leaves`].
    separate_terminals: bool,
}

impl PackingBudget {
    pub(crate) fn from_search(search: &SearchConfig) -> Self {
        Self {
            // Rank zero is not a retry, so it is not spent from the retry
            // allowance: a budget of zero backtracks still packs exactly as
            // `pack_free_leaves` does.
            layouts: search
                .max_fragment_backtracks_per_proposal
                .saturating_add(1),
            expansions: search.router_limits.max_node_expansions,
            span: (None, None),
            separate_terminals: false,
        }
    }

    /// This budget, taking only layouts no wider than `span` (x, z).
    pub(crate) fn within(self, span: (Option<i32>, Option<i32>)) -> Self {
        Self { span, ..self }
    }

    /// This budget, keeping every terminal off other signals' runways.
    pub(crate) fn separating_terminals(self) -> Self {
        Self {
            separate_terminals: true,
            ..self
        }
    }
}

/// What a caller did with one ranked layout.
pub(crate) enum LayoutVerdict<T, E> {
    /// It worked. The search stops here.
    Accepted(T),
    /// It failed for a reason a different layout could fix -- a collision, a
    /// route with nowhere to go, a boundary cell another placement would have
    /// left clear.
    Retry(E),
    /// It failed for a reason no layout can fix. Trying more would be waste.
    Fatal(E),
}

#[derive(Debug)]
pub(crate) enum LayoutSearchError<E> {
    /// Not even rank zero could be built; this is what `pack_free_leaves`
    /// itself would have returned.
    Packing(PackingError),
    Fatal(E),
    /// Every layout the budget allowed was tried. The error reported is rank
    /// zero's, because that is the layout a caller without this repair would
    /// have got, and `attempted` says how much was spent proving it.
    Exhausted {
        attempted: usize,
        rank_zero: E,
    },
    /// The budget stopped the walk before a single complete layout existed,
    /// so there is no rank-zero refusal to report.
    BudgetExhausted {
        attempted: usize,
    },
    /// The caller's placement order is not an exact cover of its children.
    Order(PlacementOrderError),
}

/// Try layouts in rank order until `attempt` accepts one.
///
/// Rank zero is byte-identical to [`pack_free_leaves`]: the walk visits each
/// leaf in canonical [`ChunkId`] order and takes the same lowest-scoring
/// feasible translation first, so the first complete layout is the greedy one
/// and every later rank differs from it in the latest possible choice. Leaves
/// are never permuted, the candidate translations are the existing ones, and
/// the feasibility test is the existing halo-disjointness and limit check --
/// the only thing added is that a refused layout is followed by the next one
/// instead of by a refusal.
pub(crate) fn search_ranked_layouts<T, E, F>(
    artifacts: &[FreeLeafArtifact],
    budget: PackingBudget,
    attempt: F,
) -> Result<(usize, T), LayoutSearchError<E>>
where
    F: FnMut(usize, &PackedFreeLeaves) -> LayoutVerdict<T, E>,
{
    let ordered = canonical_artifacts(artifacts).map_err(LayoutSearchError::Packing)?;
    search_ordered(ordered, budget, &SeamBands::none(), attempt)
}

/// [`search_ranked_layouts`] over a placement order the caller has chosen.
///
/// The one seam that lets a caller say *which child is placed first*, because
/// for the only caller that has an opinion -- a packed node -- that is not a
/// free choice. Every terminal this crate builds faces east, so a trunk leaves
/// its source eastward and enters its sink from the west: the driver has to be
/// placed before the reader or the two runways point away from each other and
/// no amount of re-ranking the second child's translation can bring them back.
///
/// `order` is validated as an exact cover of `artifacts` -- every identity
/// once, none foreign, none missing -- so a caller cannot quietly drop or
/// duplicate a child by handing in the wrong list. The caller's *vector* order
/// still cannot reach the result: `order` is the only thing consulted, and it
/// is built from the netlist rather than from the slice.
///
/// Generic packing is unchanged. [`pack_free_leaves`] and
/// [`search_ranked_layouts`] both remain canonical-[`ChunkId`] order, so rank
/// zero on that path is still byte-identical to what it always was.
pub(crate) fn search_ranked_layouts_in_order<T, E, F>(
    artifacts: &[FreeLeafArtifact],
    order: &[ChunkId],
    budget: PackingBudget,
    bands: &SeamBands,
    attempt: F,
) -> Result<(usize, T), LayoutSearchError<E>>
where
    F: FnMut(usize, &PackedFreeLeaves) -> LayoutVerdict<T, E>,
{
    // Validate against the canonical view, which has already refused a
    // duplicated artifact.
    let canonical = canonical_artifacts(artifacts).map_err(LayoutSearchError::Packing)?;
    let mut by_chunk: BTreeMap<&ChunkId, &FreeLeafArtifact> = canonical
        .iter()
        .map(|artifact| (&artifact.chunk, *artifact))
        .collect();
    let mut ordered = Vec::with_capacity(order.len());
    for chunk in order {
        let artifact = by_chunk.remove(chunk).ok_or_else(|| {
            LayoutSearchError::Order(PlacementOrderError::UnknownChild {
                chunk: chunk.clone(),
            })
        })?;
        ordered.push(artifact);
    }
    if let Some(chunk) = by_chunk.keys().next() {
        return Err(LayoutSearchError::Order(
            PlacementOrderError::MissingChild {
                chunk: (*chunk).clone(),
            },
        ));
    }
    search_ordered(ordered, budget, bands, attempt)
}

/// Refusals about the placement order itself, before a cell is chosen.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub(crate) enum PlacementOrderError {
    #[error("placement order names {chunk:?}, which is not one of these children")]
    UnknownChild { chunk: ChunkId },
    #[error("placement order omits child {chunk:?}")]
    MissingChild { chunk: ChunkId },
}

fn search_ordered<T, E, F>(
    ordered: Vec<&FreeLeafArtifact>,
    budget: PackingBudget,
    bands: &SeamBands,
    attempt: F,
) -> Result<(usize, T), LayoutSearchError<E>>
where
    F: FnMut(usize, &PackedFreeLeaves) -> LayoutVerdict<T, E>,
{
    // The frame is a sum over all the children and a max over their heights,
    // so it is the same whichever order they are placed in.
    let limits = PackingLimits::for_ordered(&ordered, bands).map_err(LayoutSearchError::Packing)?;
    if limits.max.x < 0 || limits.max.y < 0 || limits.max.z < 0 {
        return Err(LayoutSearchError::Packing(PackingError::InvalidLimits {
            max: limits.max,
        }));
    }
    let mut search = LayoutSearch {
        artifacts: &ordered,
        limits,
        budget,
        bands,
        band_columns: 0,
        band_cells: BTreeSet::new(),
        attempt,
        rank: 0,
        expansions: 0,
        rank_zero: None,
        no_placement: None,
    };
    let mut placements = BTreeMap::new();
    let mut reserved = BTreeSet::new();
    let mut owners = ReservedOwners::new();
    match search.descend(0, &mut placements, &mut reserved, &mut owners)? {
        Some(accepted) => Ok(accepted),
        // No layout was accepted. Which refusal that is depends on how far the
        // walk got: rank zero's if it was built, the placement refusal
        // `pack_free_leaves` itself would raise if no leaf ever fitted, and
        // the budget otherwise.
        None => Err(match (search.rank_zero, search.no_placement) {
            (Some(rank_zero), _) => LayoutSearchError::Exhausted {
                attempted: search.rank,
                rank_zero,
            },
            (None, Some(error)) => LayoutSearchError::Packing(error),
            (None, None) => LayoutSearchError::BudgetExhausted {
                attempted: search.rank,
            },
        }),
    }
}

struct LayoutSearch<'a, F, E> {
    artifacts: &'a [&'a FreeLeafArtifact],
    limits: PackingLimits,
    budget: PackingBudget,
    bands: &'a SeamBands,
    /// Band columns the placements so far have left, discounted from every
    /// deeper candidate's envelope.
    band_columns: i64,
    /// The cells those bands occupy, closed to later placement.
    band_cells: BTreeSet<Anchor>,
    attempt: F,
    /// Complete layouts offered so far; the next one's rank.
    rank: usize,
    expansions: u64,
    rank_zero: Option<E>,
    /// The first leaf that had nowhere to go, as the refusal
    /// [`pack_free_leaves`] would have raised for it.
    no_placement: Option<PackingError>,
}

impl<'a, T, E, F> LayoutSearch<'a, F, E>
where
    F: FnMut(usize, &PackedFreeLeaves) -> LayoutVerdict<T, E>,
{
    /// Depth-first over leaves in canonical order, translations in score
    /// order. The first complete path is the greedy layout; the enumeration
    /// after it is the lexicographic order of those same choices.
    fn descend(
        &mut self,
        index: usize,
        placements: &mut BTreeMap<ChunkId, PackedFreeLeaf>,
        reserved: &mut BTreeSet<Anchor>,
        owners: &mut ReservedOwners,
    ) -> Result<Option<(usize, T)>, LayoutSearchError<E>> {
        if index == self.artifacts.len() {
            if u64::try_from(self.rank).unwrap_or(u64::MAX) >= self.budget.layouts {
                return Ok(None);
            }
            // **One global translation, then one judgement.** Intermediate
            // placements are signed because a bridge is relative; the frame is
            // whatever box the finished layout sits in, moved so its minimum
            // is the origin. On the legacy path every minimum is already zero,
            // so this is the identity and rank zero is untouched.
            let Some(packed) = self.normalised(placements, reserved)? else {
                return Ok(None);
            };
            let rank = self.rank;
            self.rank += 1;
            return match (self.attempt)(rank, &packed) {
                LayoutVerdict::Accepted(value) => Ok(Some((rank, value))),
                LayoutVerdict::Fatal(error) => Err(LayoutSearchError::Fatal(error)),
                LayoutVerdict::Retry(error) => {
                    if rank == 0 {
                        self.rank_zero = Some(error);
                    }
                    Ok(None)
                }
            };
        }

        let artifact = self.artifacts[index];
        for (translation, discount) in self.feasible(index, placements, reserved, owners)? {
            if self.expansions >= self.budget.expansions {
                return Ok(None);
            }
            self.expansions += 1;
            let placement =
                translate_artifact(artifact, translation).map_err(LayoutSearchError::Packing)?;
            let slab = if discount > 0 {
                band_slab(
                    reserved,
                    &placement.halo,
                    i32::try_from(discount).unwrap_or(i32::MAX),
                )
                .into_iter()
                .filter(|at| self.band_cells.insert(*at))
                .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            reserved.extend(placement.halo.iter().copied());
            if !self.bands.is_none() {
                owners.extend(
                    placement
                        .halo
                        .iter()
                        .map(|at| (*at, artifact.chunk.clone())),
                );
            }
            self.band_columns = self.band_columns.saturating_add(discount);
            let halo = placement.halo.clone();
            placements.insert(artifact.chunk.clone(), placement);
            let found = self.descend(index + 1, placements, reserved, owners)?;
            placements.remove(&artifact.chunk);
            self.band_columns = self.band_columns.saturating_sub(discount);
            for at in &slab {
                self.band_cells.remove(at);
            }
            for at in &halo {
                reserved.remove(at);
                owners.remove(at);
            }
            if let Some(accepted) = found {
                return Ok(Some(accepted));
            }
        }
        Ok(None)
    }

    /// Move a complete layout so its minimum is the origin, then judge it
    /// against the same derived limits. `None` is a layout the frame cannot
    /// hold even after the move.
    fn normalised(
        &self,
        placements: &BTreeMap<ChunkId, PackedFreeLeaf>,
        reserved: &BTreeSet<Anchor>,
    ) -> Result<Option<PackedFreeLeaves>, LayoutSearchError<E>> {
        let Some((min, _)) = bounds(reserved) else {
            return Ok(Some(PackedFreeLeaves {
                placements: placements.clone(),
                halo: reserved.clone(),
            }));
        };
        let shift = Anchor {
            x: -min.x,
            y: -min.y,
            z: -min.z,
        };
        let shifted = |at: Anchor, chunk: &ChunkId| -> Result<Anchor, LayoutSearchError<E>> {
            let overflow = || {
                LayoutSearchError::Packing(PackingError::CoordinateOverflow {
                    chunk: chunk.clone(),
                })
            };
            Ok(Anchor {
                x: at.x.checked_add(shift.x).ok_or_else(overflow)?,
                y: at.y.checked_add(shift.y).ok_or_else(overflow)?,
                z: at.z.checked_add(shift.z).ok_or_else(overflow)?,
            })
        };
        let mut moved = BTreeMap::new();
        for (chunk, placement) in placements {
            let artifact = self
                .artifacts
                .iter()
                .find(|artifact| artifact.chunk == *chunk)
                .ok_or_else(|| {
                    LayoutSearchError::Packing(PackingError::DuplicateChunk {
                        chunk: chunk.clone(),
                    })
                })?;
            let translation = shifted(placement.translation, chunk)?;
            let placement =
                translate_artifact(artifact, translation).map_err(LayoutSearchError::Packing)?;
            if !inside_limits(&placement.halo, self.limits) {
                return Ok(None);
            }
            moved.insert(chunk.clone(), placement);
        }
        let halo = moved
            .values()
            .flat_map(|placement| placement.halo.iter().copied())
            .collect();
        Ok(Some(PackedFreeLeaves {
            placements: moved,
            halo,
        }))
    }

    /// Every translation this leaf may take here, lowest score first.
    ///
    /// The same candidates, the same limit test and the same halo-disjointness
    /// test `pack_ordered` uses, and the same score -- sorted rather than
    /// reduced to one winner.
    fn feasible(
        &mut self,
        index: usize,
        placements: &BTreeMap<ChunkId, PackedFreeLeaf>,
        reserved: &BTreeSet<Anchor>,
        owners: &ReservedOwners,
    ) -> Result<Vec<(Anchor, i64)>, LayoutSearchError<E>> {
        let artifact = self.artifacts[index];
        let limits = self.limits;
        let mut blocked = || PackingError::NoPlacement {
            chunk: artifact.chunk.clone(),
            limits,
        };
        validate_mask(artifact).map_err(LayoutSearchError::Packing)?;
        let (min, max) = bounds(&artifact.halo).ok_or_else(|| {
            LayoutSearchError::Packing(PackingError::EmptyHalo {
                chunk: artifact.chunk.clone(),
            })
        })?;
        if span(min.y, max.y) > i64::from(limits.max.y) + 1
            || span(min.z, max.z) > i64::from(limits.max.z) + 1
            || span(min.x, max.x) > i64::from(limits.max.x) + 1
        {
            self.no_placement.get_or_insert_with(&mut blocked);
            return Ok(Vec::new());
        }
        let frame = bounds(reserved);
        let span_cap = self.budget.span;
        let too_wide = |halo: &BTreeSet<Anchor>| {
            let Some((mut low, mut high)) = bounds(halo) else {
                return false;
            };
            if let Some((min, max)) = frame {
                low = Anchor { x: low.x.min(min.x), y: 0, z: low.z.min(min.z) };
                high = Anchor { x: high.x.max(max.x), y: 0, z: high.z.max(max.z) };
            }
            span_cap.0.is_some_and(|cap| span(low.x, high.x) > i64::from(cap))
                || span_cap.1.is_some_and(|cap| span(low.z, high.z) > i64::from(cap))
        };
        // Every placed terminal's runway and mouth. A translation that stands
        // one of this child's terminals on another signal's is no layout at
        // all -- both routes must take that cell -- so it moves along `z`
        // until the two clear, or is dropped.
        let placed_ways = placements
            .values()
            .flat_map(|placement| terminal_ways(placement.interfaces.values(), Anchor { x: 0, y: 0, z: 0 }))
            .collect::<BTreeMap<_, _>>();
        let separate = self.budget.separate_terminals;
        let clashes = |translation: Anchor| {
            separate
                && terminal_ways(artifact.interfaces.values(), translation)
                .into_iter()
                .any(|(at, signal)| placed_ways.get(&at).is_some_and(|other| *other != signal))
        };
        let mut scored = Vec::new();
        for (translation, band) in candidate_translations(artifact, reserved, owners, self.bands)
            .map_err(LayoutSearchError::Packing)?
        {
            let Some(translation) = (0..=TERMINAL_CLASH_SHIFT)
                .flat_map(|dz| [dz, -dz])
                .map(|dz| Anchor {
                    z: translation.z + dz,
                    ..translation
                })
                .find(|translation| !clashes(*translation))
            else {
                continue;
            };
            let halo = translate_set_for_chunk(&artifact.halo, translation, &artifact.chunk)
                .map_err(LayoutSearchError::Packing)?;
            if !inside_limits(&halo, limits)
                || too_wide(&halo)
                || halo
                    .iter()
                    .any(|at| reserved.contains(at) || self.band_cells.contains(at))
            {
                continue;
            }
            scored.push((
                envelope_score_with_band(reserved, &halo, translation, self.band_columns, band),
                translation,
                band_discount(reserved, &halo, band),
            ));
        }
        scored.sort();
        let mut order = scored
            .into_iter()
            .map(|(_, translation, discount)| (translation, discount))
            .collect::<Vec<_>>();

        // The legacy candidates are exhausted before the aligned ones are
        // even looked at, whatever they score. Rank zero is the first
        // complete path, so anything that could come before the greedy pick
        // would change it -- and rank zero is `pack_free_leaves`.
        let already = order
            .iter()
            .map(|(translation, _)| *translation)
            .collect::<BTreeSet<_>>();
        let mut aligned = Vec::new();
        for (score, translation) in self.aligned_candidates(artifact, placements)? {
            if already.contains(&translation) {
                continue;
            }
            // No frame test here. A bridge is a *relative* statement about
            // two terminals, and the frame is an absolute one about an origin
            // nobody has fixed yet: rejecting a prefix for a negative
            // coordinate would throw away the bridge rather than move the
            // origin. The complete layout is normalised and judged below.
            let halo = translate_set_for_chunk(&artifact.halo, translation, &artifact.chunk)
                .map_err(LayoutSearchError::Packing)?;
            if too_wide(&halo)
                || clashes(translation)
                || halo
                    .iter()
                    .any(|at| reserved.contains(at) || self.band_cells.contains(at))
            {
                continue;
            }
            let discount = band_discount(reserved, &halo, self.bands.max_width());
            aligned.push((score, translation, discount));
        }
        aligned.sort();
        aligned.dedup_by_key(|(_, translation, _)| *translation);
        order.extend(
            aligned
                .into_iter()
                .map(|(_, translation, discount)| (translation, discount)),
        );

        if order.is_empty() {
            self.no_placement.get_or_insert_with(&mut blocked);
        }
        Ok(order)
    }

    /// **Translations that put one of this leaf's terminals opposite an
    /// already placed leaf's terminal for the same signal.**
    ///
    /// The legacy candidates know only about occupied space: they abut a
    /// halo's corner and let the router find the rest. That is enough when one
    /// signal crosses a seam and the terminals happen to line up, and it is
    /// why a two-child node has exactly two layouts -- neither of which is
    /// chosen for having anything to do with the signal that must cross.
    ///
    /// These are chosen for exactly that. For each pair of interfaces that
    /// must be joined -- same signal, one an output and the other an input,
    /// with runways that point at each other rather than away -- there is one
    /// translation that puts the two terminals on a common axis at a common
    /// height, with the halos in exact contact. No gap is invented: the axial
    /// offset is whatever makes the two masks touch, and every other
    /// coordinate is read off the terminal that is already placed.
    ///
    /// One translation cannot align two signal pairs at once unless the
    /// geometry already agrees, so each pair contributes its own candidate and
    /// the ranked search decides which one routes.
    #[allow(clippy::type_complexity)]
    fn aligned_candidates(
        &self,
        artifact: &FreeLeafArtifact,
        placements: &BTreeMap<ChunkId, PackedFreeLeaf>,
    ) -> Result<Vec<(AlignedScore, Anchor)>, LayoutSearchError<E>> {
        if bounds(&artifact.halo).is_none() {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for (chunk, placed) in placements {
            for (placed_id, anchor) in &placed.interfaces {
                for (mine_id, mine) in &artifact.interfaces {
                    if anchor.signal != mine.signal || anchor.role == mine.role {
                        continue;
                    }
                    let out = interface_route_direction(anchor);
                    let into = interface_route_direction(mine);
                    // Two runways that do not point at each other cannot be
                    // joined by a straight exit and a straight entry, however
                    // the leaves are placed.
                    if into != out.opposite() {
                        continue;
                    }
                    let band = self.bands.width(chunk, &artifact.chunk);
                    let Some(translation) =
                        bridge_translation(out, anchor.pin.at, mine.pin.at, band)
                    else {
                        continue;
                    };
                    found.push((
                        (
                            translation.x,
                            translation.y,
                            translation.z,
                            chunk.clone(),
                            placed_id.endpoint,
                            mine_id.endpoint,
                        ),
                        translation,
                    ));
                }
            }
        }
        Ok(found)
    }
}

/// The deterministic key an aligned candidate is ordered by: the translation
/// first, then the two interface identities that proposed it, so two runs
/// enumerate the same candidates in the same order and a tie is broken by
/// identity rather than by iteration order.
type AlignedScore = (
    i32,
    i32,
    i32,
    ChunkId,
    crate::compile::fragment_synth::identity::PhysicalEndpointId,
    crate::compile::fragment_synth::identity::PhysicalEndpointId,
);

/// **The axial pin separation that leaves two forced runways mouth to mouth.**
///
/// A terminal's route is held straight for its anchor and
/// [`TERMINAL_RUNWAY_CELLS`] further cells -- that is [`runway_core`], and it
/// is the same at both ends. Two terminals facing each other therefore spend
/// `2 * TERMINAL_RUNWAY_CELLS` cells on runway before either is free to turn,
/// and the two mouths have to be neighbours rather than the same cell, which
/// is the `+ 1`. Nothing here is a gap anybody chose: shorten it and one
/// runway overruns the other, lengthen it and the two mouths are not adjacent.
fn bridge_pin_separation() -> i32 {
    2 * i32::from(TERMINAL_RUNWAY_CELLS as u16) + 1
}

/// The translation that bridges `mine`'s terminal to `placed`'s along `out`.
///
/// `out` is the direction a route leaves the already placed terminal, so the
/// leaf being placed goes that way. Height and the transverse axis are read
/// off the placed terminal; the axial coordinate is
/// [`bridge_pin_separation`] cells along `out`, which is where the two runway
/// mouths end up adjacent. The result may be signed -- a bridge often wants
/// the incoming leaf at a transverse offset the frame origin does not admit --
/// and the complete layout is normalised before it is judged.
fn bridge_translation(
    out: Facing,
    placed_pin: Anchor,
    local_pin: Anchor,
    band: i32,
) -> Option<Anchor> {
    // A band widens the seam the bridge crosses by exactly its own width.
    let reach = bridge_pin_separation().checked_add(band)?;
    let target = match out {
        Facing::East => Anchor {
            x: placed_pin.x + reach,
            ..placed_pin
        },
        Facing::West => Anchor {
            x: placed_pin.x - reach,
            ..placed_pin
        },
        Facing::South => Anchor {
            z: placed_pin.z + reach,
            ..placed_pin
        },
        Facing::North => Anchor {
            z: placed_pin.z - reach,
            ..placed_pin
        },
        // A vertical runway names no plane to bridge across.
        Facing::Up | Facing::Down => return None,
    };
    Some(Anchor {
        x: target.x - local_pin.x,
        y: target.y - local_pin.y,
        z: target.z - local_pin.z,
    })
}

fn validate_mask(artifact: &FreeLeafArtifact) -> Result<(), PackingError> {
    if !artifact.access.is_subset(&artifact.halo) {
        return Err(PackingError::AccessOutsideHalo {
            chunk: artifact.chunk.clone(),
        });
    }
    if !artifact.occupied.is_subset(&artifact.halo) {
        return Err(PackingError::OccupiedOutsideHalo {
            chunk: artifact.chunk.clone(),
        });
    }
    for (id, interface) in &artifact.interfaces {
        if id.chunk != artifact.chunk {
            return Err(PackingError::InterfaceChunkMismatch {
                chunk: artifact.chunk.clone(),
                interface_chunk: id.chunk.clone(),
                endpoint: id.endpoint,
            });
        }
        if !artifact.access.contains(&interface.pin.at) {
            return Err(PackingError::InterfaceOutsideAccess {
                chunk: artifact.chunk.clone(),
                endpoint: id.endpoint,
            });
        }
    }
    Ok(())
}

fn canonical_artifacts(
    artifacts: &[FreeLeafArtifact],
) -> Result<Vec<&FreeLeafArtifact>, PackingError> {
    let mut ordered: Vec<_> = artifacts.iter().collect();
    ordered.sort_by(|left, right| left.chunk.cmp(&right.chunk));
    if let Some(duplicate) = ordered
        .windows(2)
        .find_map(|pair| (pair[0].chunk == pair[1].chunk).then(|| pair[0].chunk.clone()))
    {
        return Err(PackingError::DuplicateChunk { chunk: duplicate });
    }
    Ok(ordered)
}

/// Every translation the legacy packer considers for `artifact`, each with
/// the band it claims: the origin, the column past the whole reservation,
/// and beside or behind every reserved cell. An `x`-contact candidate beside
/// a cell of child `c` stands `bands.width(c, artifact)` further east and
/// claims that width; `z`-contact candidates claim none. The claim is only a
/// claim: [`band_discount`] reads the empty columns a translation really
/// leaves before any of it is priced. With no bands the owner map is never
/// consulted and the set is the one the packer has always enumerated.
fn candidate_translations(
    artifact: &FreeLeafArtifact,
    reserved: &BTreeSet<Anchor>,
    owners: &ReservedOwners,
    bands: &SeamBands,
) -> Result<BTreeMap<Anchor, i32>, PackingError> {
    let (min, _) = bounds(&artifact.halo).ok_or_else(|| PackingError::EmptyHalo {
        chunk: artifact.chunk.clone(),
    })?;
    let baseline = translation_from_components(
        -i64::from(min.x),
        -i64::from(min.y),
        -i64::from(min.z),
        &artifact.chunk,
    )?;
    let mut candidates = BTreeMap::from([(baseline, 0)]);
    let mut offer = |candidates: &mut BTreeMap<Anchor, i32>, translation: Anchor, band: i32| {
        let entry = candidates.entry(translation).or_insert(band);
        *entry = (*entry).max(band);
    };
    if let Some((_, reserved_max)) = bounds(reserved) {
        // A band is measured from the east edge of the halo it belongs to,
        // never from an interior cell of it: a cell one column back would
        // offer a seam one column too narrow. None of this is computed when
        // there are no bands.
        let owner_max_x: BTreeMap<&ChunkId, i32> = if bands.is_none() {
            BTreeMap::new()
        } else {
            let mut edges = BTreeMap::new();
            for (at, owner) in owners {
                let entry = edges.entry(owner).or_insert(at.x);
                *entry = (*entry).max(at.x);
            }
            edges
        };
        let band_of = |at: &Anchor| -> (i32, i32) {
            if bands.is_none() {
                return (0, at.x);
            }
            let owner = owners.get(at);
            let band = owner
                .map(|owner| bands.width(owner, &artifact.chunk))
                .unwrap_or(0);
            let edge = if band > 0 {
                owner
                    .and_then(|owner| owner_max_x.get(owner).copied())
                    .unwrap_or(at.x)
            } else {
                at.x
            };
            (band, edge)
        };
        let band_at_max = reserved
            .iter()
            .filter(|at| at.x == reserved_max.x)
            .map(|at| band_of(at).0)
            .max()
            .unwrap_or(0);
        offer(
            &mut candidates,
            translation_from_components(
                i64::from(reserved_max.x) + 1 + i64::from(band_at_max) - i64::from(min.x),
                -i64::from(min.y),
                -i64::from(min.z),
                &artifact.chunk,
            )?,
            band_at_max,
        );
        for at in reserved {
            offer(
                &mut candidates,
                translation_from_components(
                    -i64::from(min.x),
                    -i64::from(min.y),
                    i64::from(at.z) + 1 - i64::from(min.z),
                    &artifact.chunk,
                )?,
                0,
            );
            let (band, edge) = band_of(at);
            offer(
                &mut candidates,
                translation_from_components(
                    i64::from(edge) + 1 + i64::from(band) - i64::from(min.x),
                    -i64::from(min.y),
                    -i64::from(min.z),
                    &artifact.chunk,
                )?,
                band,
            );
        }
    }
    Ok(candidates)
}

/// How far along `z` a contact translation moves to take a child's terminals
/// off another signal's runway: a terminal row pitch and a runway.
const TERMINAL_CLASH_SHIFT: i32 = 6;

/// Each terminal's runway and mouth, moved by `translation`, with the signal
/// it carries: the cells only that signal's route may take.
fn terminal_ways<'a>(
    interfaces: impl Iterator<Item = &'a ParentConnectableInterface>,
    translation: Anchor,
) -> Vec<(Anchor, &'a str)> {
    let mut ways = Vec::new();
    for interface in interfaces {
        let facing = interface_route_direction(interface);
        let at = Anchor {
            x: interface.pin.at.x + translation.x,
            y: interface.pin.at.y + translation.y,
            z: interface.pin.at.z + translation.z,
        };
        let mut cells = runway_core(at, facing);
        let mouth = runway_core(*cells.last().expect("a core has its anchor"), facing)[1];
        cells.push(mouth);
        ways.extend(cells.into_iter().map(|cell| (cell, interface.signal.as_str())));
    }
    ways
}

/// The cells a band occupies once a child has been placed across it: every
/// column between the reservation's east edge and the new halo's west edge,
/// over the full height and depth of everything placed so far. Excluded from
/// later placement only -- never part of any halo -- so a later child cannot
/// be packed into the room a trunk was promised.
fn band_slab(reserved: &BTreeSet<Anchor>, halo: &BTreeSet<Anchor>, band: i32) -> BTreeSet<Anchor> {
    let mut slab = BTreeSet::new();
    if band_discount(reserved, halo, band) <= 0 {
        return slab;
    }
    let (Some((_, reserved_max)), Some((halo_min, _))) = (bounds(reserved), bounds(halo)) else {
        return slab;
    };
    let all = reserved.iter().chain(halo).copied().collect();
    let (min, max) = bounds(&all).expect("a placement halo is non-empty");
    for x in (reserved_max.x + 1)..halo_min.x {
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                slab.insert(Anchor { x, y, z });
            }
        }
    }
    slab
}

/// The empty band columns a translation really leaves: the columns between
/// the reservation's east edge and the halo's west edge, at most the band
/// claimed. A claim on a translation that closes the gap -- the same cell
/// offered by a `z`-contact -- discounts nothing.
fn band_discount(reserved: &BTreeSet<Anchor>, halo: &BTreeSet<Anchor>, band: i32) -> i64 {
    if band <= 0 {
        return 0;
    }
    let (Some((_, reserved_max)), Some((halo_min, _))) = (bounds(reserved), bounds(halo)) else {
        return 0;
    };
    let gap = i64::from(halo_min.x) - i64::from(reserved_max.x) - 1;
    gap.clamp(0, i64::from(band))
}

fn translation_from_components(
    x: i64,
    y: i64,
    z: i64,
    chunk: &ChunkId,
) -> Result<Anchor, PackingError> {
    Ok(Anchor {
        x: i32::try_from(x).map_err(|_| PackingError::CoordinateOverflow {
            chunk: chunk.clone(),
        })?,
        y: i32::try_from(y).map_err(|_| PackingError::CoordinateOverflow {
            chunk: chunk.clone(),
        })?,
        z: i32::try_from(z).map_err(|_| PackingError::CoordinateOverflow {
            chunk: chunk.clone(),
        })?,
    })
}

fn inside_limits(mask: &BTreeSet<Anchor>, limits: PackingLimits) -> bool {
    mask.iter().all(|at| {
        at.x >= 0
            && at.y >= 0
            && at.z >= 0
            && at.x <= limits.max.x
            && at.y <= limits.max.y
            && at.z <= limits.max.z
    })
}

pub(crate) fn envelope_score(
    reserved: &BTreeSet<Anchor>,
    halo: &BTreeSet<Anchor>,
    translation: Anchor,
) -> (u128, i32, i32, i32, i32) {
    envelope_score_with_band(reserved, halo, translation, 0, 0)
}

/// [`envelope_score`] with the deliberate emptiness taken out: `prior`
/// band columns already inside the reservation, and the columns this
/// candidate itself leaves as [`band_discount`] measures them. Those columns
/// are room a trunk was promised, not congestion, so a banded layout ranks
/// its candidates exactly as the same layout ranks them without the bands.
/// The tie-breakers stay literal.
pub(crate) fn envelope_score_with_band(
    reserved: &BTreeSet<Anchor>,
    halo: &BTreeSet<Anchor>,
    translation: Anchor,
    prior: i64,
    band: i32,
) -> (u128, i32, i32, i32, i32) {
    let discount = prior.saturating_add(band_discount(reserved, halo, band));
    let all = reserved.iter().chain(halo).copied().collect();
    let (min, max) = bounds(&all).expect("a placement halo is non-empty");
    let width = u128::try_from(span(min.x, max.x).saturating_sub(discount).max(1))
        .expect("span is positive");
    let depth = u128::try_from(span(min.z, max.z)).expect("span is positive");
    (width * depth, max.x, max.z, translation.x, translation.z)
}

pub(crate) fn translate_artifact(
    artifact: &FreeLeafArtifact,
    translation: Anchor,
) -> Result<PackedFreeLeaf, PackingError> {
    let translate = |at| translate_anchor(at, translation, &artifact.chunk);
    let interfaces = artifact
        .interfaces
        .iter()
        .map(|(id, interface)| {
            let mut interface = interface.clone();
            interface.pin.at = translate(interface.pin.at)?;
            Ok((id.clone(), interface))
        })
        .collect::<Result<_, PackingError>>()?;
    Ok(PackedFreeLeaf {
        chunk: artifact.chunk.clone(),
        translation,
        interfaces,
        occupied: translate_set_for_chunk(&artifact.occupied, translation, &artifact.chunk)?,
        halo: translate_set_for_chunk(&artifact.halo, translation, &artifact.chunk)?,
        access: translate_set_for_chunk(&artifact.access, translation, &artifact.chunk)?,
    })
}

fn translate_set(mask: &BTreeSet<Anchor>, translation: Anchor) -> Option<BTreeSet<Anchor>> {
    mask.iter()
        .map(|at| {
            Some(Anchor {
                x: at.x.checked_add(translation.x)?,
                y: at.y.checked_add(translation.y)?,
                z: at.z.checked_add(translation.z)?,
            })
        })
        .collect()
}

fn translate_set_for_chunk(
    mask: &BTreeSet<Anchor>,
    translation: Anchor,
    chunk: &ChunkId,
) -> Result<BTreeSet<Anchor>, PackingError> {
    translate_set(mask, translation).ok_or_else(|| PackingError::CoordinateOverflow {
        chunk: chunk.clone(),
    })
}

fn translate_anchor(
    at: Anchor,
    translation: Anchor,
    chunk: &ChunkId,
) -> Result<Anchor, PackingError> {
    translate_set_for_chunk(&BTreeSet::from([at]), translation, chunk)
        .map(|mut translated| translated.pop_first().expect("one translated anchor"))
}

fn bounds(mask: &BTreeSet<Anchor>) -> Option<(Anchor, Anchor)> {
    let first = *mask.first()?;
    Some(mask.iter().copied().fold((first, first), |(min, max), at| {
        (
            Anchor {
                x: min.x.min(at.x),
                y: min.y.min(at.y),
                z: min.z.min(at.z),
            },
            Anchor {
                x: max.x.max(at.x),
                y: max.y.max(at.y),
                z: max.z.max(at.z),
            },
        )
    }))
}

fn span(min: i32, max: i32) -> i64 {
    i64::from(max) - i64::from(min) + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::fragment_synth::allocation::SignalContract;
    use crate::compile::fragment_synth::config::SearchConfig;
    use crate::compile::fragment_synth::identity::{PhysicalEndpointId, PortId};
    use crate::compile::fragment_synth::leaf::{synthesise_free_leaf, LEAF_PITCHES};
    use crate::compile::fragment_synth::partition::{partition, root_chunk_id};
    use crate::compile::fragment_synth::terminal_geometry::{
        source_escape_corridor, terminal_access_cells, terminal_guard_cells,
    };
    use crate::compile::planner::{PortPin, PortRole};
    use crate::compile::topology::SignalPolarity;
    use crate::compile::{Gate, Netlist};
    use crate::redstone::world::block::{BlockKind, Facing};
    use crate::redstone::world::storage::World;

    fn leaf_netlist(name: &str) -> Netlist {
        Netlist {
            inputs: vec!["in".into()],
            outputs: vec![name.into()],
            gates: vec![Gate::nor(name, &["in"])],
        }
    }

    fn chunk(name: &str) -> ChunkId {
        root_chunk_id(&leaf_netlist(name)).unwrap()
    }

    fn artifact(name: &str, halo: &[(i32, i32, i32)]) -> FreeLeafArtifact {
        let chunk = chunk(name);
        let occupied = BTreeSet::from([Anchor { x: 0, y: 0, z: 0 }]);
        let mut halo: BTreeSet<_> = halo.iter().map(|&(x, y, z)| Anchor { x, y, z }).collect();
        halo.insert(Anchor { x: 0, y: 0, z: 0 });
        let pin = PortPin {
            at: Anchor { x: 1, y: 0, z: 0 },
            toward: Facing::East,
        };
        halo.insert(pin.at);
        FreeLeafArtifact {
            chunk: chunk.clone(),
            netlist: leaf_netlist(name),
            gates: Default::default(),
            certificate: None,
            world: World::new(1, 1, 1),
            interfaces: BTreeMap::from([(
                FreeLeafInterfaceId {
                    chunk,
                    endpoint: PhysicalEndpointId::PrimaryInput(PortId(0)),
                },
                ParentConnectableInterface {
                    signal: name.into(),
                    role: PortRole::Input,
                    pin,
                    contract: SignalContract {
                        polarity: SignalPolarity::Positive,
                        strength: 15,
                        delay_budget_ticks: 4,
                    },
                },
            )]),
            occupied,
            halo,
            access: BTreeSet::from([pin.at]),
        }
    }

    /// Two halos the legacy packer sets side by side along `x`: a 4-wide and
    /// a 3-wide strip, where contact along `x` (envelope 7 by 1) beats
    /// stacking along `z` (4 by 2).
    fn side_by_side() -> Vec<FreeLeafArtifact> {
        vec![
            artifact("left", &[(3, 0, 0)]),
            artifact("right", &[(2, 0, 0)]),
        ]
    }

    fn halo_x(packed: &PackedFreeLeaves, name: &str) -> (i32, i32) {
        let halo = &packed.placements[&chunk(name)].halo;
        (
            halo.iter().map(|at| at.x).min().unwrap(),
            halo.iter().map(|at| at.x).max().unwrap(),
        )
    }

    /// `band = 0` is the legacy packer, cell for cell, through every entry
    /// point: the direct one, the explicit-band one with no bands, one with
    /// a band named for a pair that is not here, and rank zero of the ranked
    /// search.
    #[test]
    fn a_zero_band_packs_byte_for_byte_like_the_legacy_packer() {
        let artifacts = side_by_side();
        let legacy = pack_free_leaves(&artifacts).unwrap();
        assert_eq!(
            pack_free_leaves_with_bands(&artifacts, &SeamBands::none()).unwrap(),
            legacy
        );
        let mut foreign = SeamBands::none();
        foreign.set(&chunk("left"), &chunk("elsewhere"), 9);
        assert_eq!(
            pack_free_leaves_with_bands(&artifacts, &foreign).unwrap(),
            legacy
        );
        let order = [chunk("left"), chunk("right")];
        let mut rank_zero = None;
        let _ = search_ranked_layouts_in_order::<(), (), _>(
            &artifacts,
            &order,
            PackingBudget {
                layouts: 1,
                expansions: 64,
                span: (None, None),
                separate_terminals: false,
            },
            &SeamBands::none(),
            |rank, packed| {
                if rank == 0 {
                    rank_zero = Some(packed.clone());
                }
                LayoutVerdict::Retry(())
            },
        );
        assert_eq!(rank_zero.unwrap(), legacy);
        // The legacy layout really is contact along x, so the band test
        // below measures a seam and not a stack.
        let (left, right) = (halo_x(&legacy, "left"), halo_x(&legacy, "right"));
        assert_eq!(right.0, left.1 + 1, "legacy halos are not in x contact");
    }

    /// A named band separates the two halos by exactly its width, and only
    /// the placement moves: every child's masks, pins and identity are the
    /// artifact's own, translated.
    #[test]
    fn adjacent_halos_are_separated_by_exactly_the_band() {
        let artifacts = side_by_side();
        let legacy = pack_free_leaves(&artifacts).unwrap();
        let mut bands = SeamBands::none();
        bands.set(&chunk("left"), &chunk("right"), 9);
        let banded = pack_free_leaves_with_bands(&artifacts, &bands).unwrap();
        let (left, right) = (halo_x(&banded, "left"), halo_x(&banded, "right"));
        assert_eq!(right.0 - left.1 - 1, 9, "the band is not exactly 9 wide");
        assert_eq!(halo_x(&banded, "left"), halo_x(&legacy, "left"));
        for artifact in &artifacts {
            let placement = &banded.placements[&artifact.chunk];
            assert_eq!(placement.chunk, artifact.chunk);
            let shift = placement.translation;
            let moved = |at: Anchor| Anchor {
                x: at.x + shift.x,
                y: at.y + shift.y,
                z: at.z + shift.z,
            };
            assert_eq!(
                placement.halo,
                artifact.halo.iter().map(|at| moved(*at)).collect()
            );
            assert_eq!(
                placement.occupied,
                artifact.occupied.iter().map(|at| moved(*at)).collect()
            );
            for (id, interface) in &artifact.interfaces {
                let packed = &placement.interfaces[id];
                assert_eq!(packed.pin.at, moved(interface.pin.at));
                assert_eq!(packed.pin.toward, interface.pin.toward);
                assert_eq!(packed.signal, interface.signal);
            }
        }
        // The frame grew by the band and nothing else.
        let ordered = canonical_artifacts(&artifacts).unwrap();
        let without = PackingLimits::for_ordered(&ordered, &SeamBands::none()).unwrap();
        let with = PackingLimits::for_ordered(&ordered, &bands).unwrap();
        assert_eq!(with.max.x, without.max.x + 9);
        assert_eq!((with.max.y, with.max.z), (without.max.y, without.max.z));
    }

    /// A frame that cannot hold the children plus their band is refused for
    /// that, by type, before any placement closes the band.
    #[test]
    fn a_frame_too_small_for_the_band_is_refused_by_type() {
        let artifacts = side_by_side();
        let ordered = canonical_artifacts(&artifacts).unwrap();
        let mut bands = SeamBands::none();
        bands.set(&chunk("left"), &chunk("right"), 9);
        let tight = PackingLimits::for_ordered(&ordered, &SeamBands::none()).unwrap();
        assert!(matches!(
            pack_ordered(&ordered, tight, &bands),
            Err(PackingError::NoBandSpace { band: 9, .. })
        ));
        let roomy = PackingLimits::for_ordered(&ordered, &bands).unwrap();
        assert!(pack_ordered(&ordered, roomy, &bands).is_ok());
    }

    /// Three children the legacy packer stacks: two equal 10-by-2 strips tie
    /// on envelope area between contact along `x` and stacking along `z`, and
    /// the `max.x` tie-break picks the stack; the third, 4-by-1, then abuts.
    fn three_with_a_stack() -> Vec<FreeLeafArtifact> {
        vec![
            artifact("first", &[(9, 0, 1)]),
            artifact("second", &[(9, 0, 1)]),
            artifact("third", &[(3, 0, 0)]),
        ]
    }

    /// Band 0 is the legacy packer with three children too, including a
    /// layout the legacy packer stacks along `z`.
    #[test]
    fn a_zero_band_is_byte_identical_with_three_children_and_a_z_stack() {
        let artifacts = three_with_a_stack();
        let legacy = pack_free_leaves(&artifacts).unwrap();
        assert!(
            legacy
                .placements
                .values()
                .any(|placement| placement.translation.z > 0),
            "the fixture must stack along z: {:?}",
            legacy
                .placements
                .values()
                .map(|placement| placement.translation)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            pack_free_leaves_with_bands(&artifacts, &SeamBands::none()).unwrap(),
            legacy
        );
        let mut foreign = SeamBands::none();
        foreign.set(&chunk("first"), &chunk("elsewhere"), 9);
        assert_eq!(
            pack_free_leaves_with_bands(&artifacts, &foreign).unwrap(),
            legacy
        );
    }

    /// With a band on every seam of a three-child row, each seam is exactly
    /// its band and the children keep the order and rows the legacy packer
    /// gave them: the cumulative discount makes the banded row rank as the
    /// plain row does, rather than as a wider, more congested one.
    #[test]
    fn cumulative_band_discount_keeps_the_legacy_strip_ranking() {
        let artifacts = vec![
            artifact("left", &[(3, 0, 0)]),
            artifact("mid", &[(2, 0, 0)]),
            artifact("right", &[(2, 0, 0)]),
        ];
        let legacy = pack_free_leaves(&artifacts).unwrap();
        let order = |packed: &PackedFreeLeaves| {
            let mut placed = packed
                .placements
                .iter()
                .map(|(chunk, placement)| {
                    (
                        placement.translation.x,
                        placement.translation.z,
                        chunk.clone(),
                    )
                })
                .collect::<Vec<_>>();
            placed.sort();
            placed
        };
        let legacy_order = order(&legacy);
        assert!(
            legacy_order.iter().all(|(_, z, _)| *z == 0),
            "the legacy layout must be one row: {legacy_order:?}"
        );
        let mut bands = SeamBands::none();
        for pair in legacy_order.windows(2) {
            bands.set(&pair[0].2, &pair[1].2, 9);
        }
        let banded = pack_free_leaves_with_bands(&artifacts, &bands).unwrap();
        let banded_order = order(&banded);
        assert_eq!(
            banded_order.iter().map(|(_, _, c)| c).collect::<Vec<_>>(),
            legacy_order.iter().map(|(_, _, c)| c).collect::<Vec<_>>(),
            "the row order changed"
        );
        assert!(
            banded_order.iter().all(|(_, z, _)| *z == 0),
            "a band made a child stack"
        );
        for pair in banded_order.windows(2) {
            let left = &banded.placements[&pair[0].2].halo;
            let right = &banded.placements[&pair[1].2].halo;
            let gap = right.iter().map(|at| at.x).min().unwrap()
                - left.iter().map(|at| at.x).max().unwrap()
                - 1;
            assert_eq!(
                gap, 9,
                "seam {:?}->{:?} is not the band",
                pair[0].2, pair[1].2
            );
        }
    }

    fn composable_artifact(name: &str) -> FreeLeafArtifact {
        let mut artifact = artifact(name, &[]);
        let mut block = BlockState::air();
        block.kind = BlockKind::Solid;
        block.name = format!("minecraft:{name}");
        let mut world = World::new(1, 1, 1);
        world.set(0, 0, 0, block);
        artifact.world = world;
        artifact.halo.insert(Anchor { x: -2, y: 0, z: 0 });
        artifact
    }

    fn non_air(world: &World) -> Vec<(Anchor, BlockState)> {
        let (size_x, size_y, size_z) = world.size();
        let mut cells = Vec::new();
        for y in 0..size_y {
            for z in 0..size_z {
                for x in 0..size_x {
                    let state = world.get(x, y, z);
                    if state.kind != BlockKind::Air {
                        cells.push((Anchor { x, y, z }, state.clone()));
                    }
                }
            }
        }
        cells
    }

    #[test]
    fn packs_irregular_halos_without_overlap() {
        let left = artifact("left", &[(0, 0, 1), (1, 0, 1)]);
        let right = artifact("right", &[(0, 0, 2), (2, 0, 0)]);
        let packed = pack_free_leaves(&[left, right]).unwrap();
        assert_eq!(packed.placements.len(), 2);
        let mut seen = BTreeSet::new();
        for placement in packed.placements.values() {
            assert!(placement.halo.iter().all(|at| seen.insert(*at)));
        }
        assert_eq!(seen, packed.halo);
    }

    #[test]
    fn irregular_leaf_uses_a_second_z_row_before_extending_the_strip() {
        let packed = pack_free_leaves(&[artifact("row_a", &[]), artifact("row_b", &[])]).unwrap();
        let second = packed.placements.values().nth(1).unwrap();
        assert_eq!(second.translation.x, 0);
        assert_eq!(second.translation.z, 1);
    }

    /// Collect every layout the search offers, by never accepting one.
    fn every_layout(
        artifacts: &[FreeLeafArtifact],
        budget: PackingBudget,
    ) -> (Vec<PackedFreeLeaves>, usize) {
        let mut layouts = Vec::new();
        let outcome = search_ranked_layouts::<(), (), _>(artifacts, budget, |rank, packed| {
            assert_eq!(rank, layouts.len(), "ranks are offered in order");
            layouts.push(packed.clone());
            LayoutVerdict::Retry(())
        });
        let attempted = match outcome {
            Err(LayoutSearchError::Exhausted { attempted, .. }) => attempted,
            other => panic!("a search that accepts nothing must exhaust, got {other:?}"),
        };
        (layouts, attempted)
    }

    fn budget(backtracks: u64) -> PackingBudget {
        let mut search = SearchConfig::checked_defaults();
        search.max_fragment_backtracks_per_proposal = backtracks;
        PackingBudget::from_search(&search)
    }

    fn pair() -> Vec<FreeLeafArtifact> {
        vec![
            artifact("a", &[(0, 0, 1), (3, 0, 0)]),
            artifact("b", &[(0, 0, 2), (2, 0, 0)]),
        ]
    }

    /// Rank zero is not a new answer. It is the answer.
    #[test]
    fn rank_zero_is_exactly_what_pack_free_leaves_builds() {
        let artifacts = pair();
        let legacy = pack_free_leaves(&artifacts).unwrap();
        let (layouts, _) = every_layout(&artifacts, budget(8));
        assert_eq!(layouts[0], legacy);
    }

    /// The ranking is keyed by stable identity, so the caller's vector order
    /// cannot reach it -- not just for rank zero, but for every rank.
    #[test]
    fn caller_order_does_not_change_any_ranked_layout() {
        let forward = every_layout(&pair(), budget(64)).0;
        let mut reversed = pair();
        reversed.reverse();
        let backward = every_layout(&reversed, budget(64)).0;
        assert_eq!(forward, backward);
    }

    /// Distinct layouts, a finite number of them, and each one a legal packing
    /// under the same derived limits rank zero used.
    #[test]
    fn every_ranked_layout_is_distinct_legal_and_finitely_many() {
        let artifacts = pair();
        let (layouts, attempted) = every_layout(&artifacts, budget(1_000));
        assert!(layouts.len() > 1, "a repair needs somewhere to go");
        assert_eq!(attempted, layouts.len());
        assert!(
            layouts.len() < 1_000,
            "the enumeration is exhausted, not merely capped"
        );

        let mut seen = Vec::new();
        for layout in &layouts {
            assert!(!seen.contains(layout), "a layout was offered twice");
            seen.push(layout.clone());
        }

        // Every offered layout has been moved so its minimum is the origin.
        // Intermediate placements may be signed -- a bridge is relative -- but
        // nothing signed is ever handed to a caller.
        for layout in &layouts {
            assert_eq!(
                bounds(&layout.halo).unwrap().0,
                Anchor { x: 0, y: 0, z: 0 },
                "a layout was offered un-normalised"
            );
        }

        let ordered = canonical_artifacts(&artifacts).unwrap();
        let limits = PackingLimits::for_ordered(&ordered, &SeamBands::none()).unwrap();
        for layout in &layouts {
            assert_eq!(layout.placements.len(), artifacts.len());
            let mut union = BTreeSet::new();
            for placement in layout.placements.values() {
                assert!(
                    inside_limits(&placement.halo, limits),
                    "a layout left the derived frame"
                );
                assert!(
                    placement.halo.is_disjoint(&union),
                    "two children share a halo cell"
                );
                assert!(placement.access.is_subset(&placement.halo));
                assert!(placement.occupied.is_subset(&placement.halo));
                union.extend(placement.halo.iter().copied());
            }
            assert_eq!(layout.halo, union);
        }
    }

    /// The budget is finite and comes from the search configuration alone: no
    /// backtracks allowed still means rank zero, and nothing after it.
    #[test]
    fn a_zero_backtrack_budget_offers_only_rank_zero() {
        let artifacts = pair();
        let (layouts, attempted) = every_layout(&artifacts, budget(0));
        assert_eq!(attempted, 1);
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0], pack_free_leaves(&artifacts).unwrap());
    }

    /// Collect every layout an explicitly ordered search offers.
    fn every_ordered_layout(
        artifacts: &[FreeLeafArtifact],
        order: &[ChunkId],
        budget: PackingBudget,
    ) -> Vec<PackedFreeLeaves> {
        let mut layouts = Vec::new();
        let outcome = search_ranked_layouts_in_order::<(), (), _>(
            artifacts,
            order,
            budget,
            &SeamBands::none(),
            |_, packed| {
                layouts.push(packed.clone());
                LayoutVerdict::Retry(())
            },
        );
        assert!(matches!(outcome, Err(LayoutSearchError::Exhausted { .. })));
        layouts
    }

    /// An explicit order is honoured, and it is not the canonical one.
    #[test]
    fn an_explicit_order_places_its_first_child_first() {
        let artifacts = pair();
        let mut order = artifacts
            .iter()
            .map(|artifact| artifact.chunk.clone())
            .collect::<Vec<_>>();
        order.sort();
        order.reverse();

        let reversed = every_ordered_layout(&artifacts, &order, budget(8));
        let canonical = every_layout(&artifacts, budget(8)).0;
        assert_ne!(
            reversed[0], canonical[0],
            "the reversed order must reach a different rank zero"
        );
        // The first child of the given order is the one at the origin.
        assert_eq!(
            reversed[0].placements[&order[0]].translation,
            canonical[0].placements[&order[1]].translation,
            "whoever goes first lands where the first child lands"
        );
    }

    /// The caller's vector order still cannot reach the result.
    #[test]
    fn caller_order_does_not_change_an_explicitly_ordered_search() {
        let artifacts = pair();
        let order = vec![artifacts[1].chunk.clone(), artifacts[0].chunk.clone()];
        let forward = every_ordered_layout(&artifacts, &order, budget(64));
        let mut reversed = artifacts.clone();
        reversed.reverse();
        let backward = every_ordered_layout(&reversed, &order, budget(64));
        assert_eq!(forward, backward);
    }

    /// The order must be an exact cover: no foreign identity, none missing.
    #[test]
    fn a_placement_order_that_is_not_an_exact_cover_is_refused() {
        let artifacts = pair();
        let foreign = artifact("foreign", &[]).chunk;
        let accept = |_: usize, _: &PackedFreeLeaves| LayoutVerdict::<(), ()>::Accepted(());

        let short = vec![artifacts[0].chunk.clone()];
        assert!(matches!(
            search_ranked_layouts_in_order(
                &artifacts,
                &short,
                budget(8),
                &SeamBands::none(),
                accept
            ),
            Err(LayoutSearchError::Order(
                PlacementOrderError::MissingChild { .. }
            ))
        ));

        let twice = vec![artifacts[0].chunk.clone(), artifacts[0].chunk.clone()];
        assert!(matches!(
            search_ranked_layouts_in_order(
                &artifacts,
                &twice,
                budget(8),
                &SeamBands::none(),
                accept
            ),
            Err(LayoutSearchError::Order(
                PlacementOrderError::UnknownChild { .. }
            )),
        ));

        let alien = vec![artifacts[0].chunk.clone(), foreign];
        assert!(matches!(
            search_ranked_layouts_in_order(
                &artifacts,
                &alien,
                budget(8),
                &SeamBands::none(),
                accept
            ),
            Err(LayoutSearchError::Order(
                PlacementOrderError::UnknownChild { .. }
            ))
        ));
    }

    /// A fatal verdict stops the walk where it stands.    /// A fatal verdict stops the walk where it stands.    /// A fatal verdict stops the walk where it stands.
    #[test]
    fn a_fatal_layout_verdict_is_not_retried() {
        let mut offered = 0;
        let outcome = search_ranked_layouts::<(), &str, _>(&pair(), budget(1_000), |_, _| {
            offered += 1;
            LayoutVerdict::Fatal("not about placement")
        });
        assert_eq!(offered, 1, "a fatal refusal is asked once");
        assert!(matches!(outcome, Err(LayoutSearchError::Fatal(_))));
    }

    #[test]
    fn caller_order_does_not_change_packing() {
        let a = artifact("a", &[(0, 0, 1), (3, 0, 0)]);
        let b = artifact("b", &[(0, 0, 2), (2, 0, 0)]);
        let forward = pack_free_leaves(&[a, b]).unwrap();
        let a = artifact("a", &[(0, 0, 1), (3, 0, 0)]);
        let b = artifact("b", &[(0, 0, 2), (2, 0, 0)]);
        let reverse = pack_free_leaves(&[b, a]).unwrap();
        assert_eq!(forward, reverse);
    }

    #[test]
    fn translation_moves_access_and_pins_but_not_orientation() {
        let source = artifact("translated", &[(-3, 0, 0), (0, 0, 1)]);
        let original = source.interfaces.values().next().unwrap().clone();
        let packed = pack_free_leaves(&[source]).unwrap();
        let placement = packed.placements.values().next().unwrap();
        let translated = placement.interfaces.values().next().unwrap();
        assert_eq!(translated.pin.toward, original.pin.toward);
        assert_eq!(
            translated.pin.at,
            Anchor {
                x: original.pin.at.x + placement.translation.x,
                y: original.pin.at.y + placement.translation.y,
                z: original.pin.at.z + placement.translation.z,
            }
        );
        assert!(placement.access.contains(&translated.pin.at));
    }

    #[test]
    fn real_free_leaves_pack_terminal_guards_outside_sibling_halos() {
        let net = Netlist {
            inputs: vec!["x".into()],
            outputs: vec!["b".into()],
            gates: vec![Gate::nor("a", &["x"]), Gate::nor("b", &["a"])],
        };
        let chunks = partition(&net, &root_chunk_id(&net).unwrap(), 1).unwrap();
        let contract = SignalContract {
            polarity: SignalPolarity::Positive,
            strength: 15,
            delay_budget_ticks: 4,
        };
        let leaves = chunks
            .iter()
            .map(|chunk| synthesise_free_leaf(chunk, contract, &SearchConfig::checked_defaults(), &LEAF_PITCHES))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let packed = pack_free_leaves(&leaves).unwrap();
        let top = packed.halo.iter().map(|at| at.y).max().unwrap();

        for owner in packed.placements.values() {
            for interface in owner.interfaces.values() {
                let direction = match interface.role {
                    PortRole::Output => interface.pin.toward,
                    PortRole::Input => interface.pin.toward.opposite(),
                };
                let [exit, runway, _] = source_escape_corridor(interface.pin.at, direction);
                assert!(owner.access.contains(&interface.pin.at));
                assert!(owner.access.contains(&exit));
                assert!(owner.access.contains(&runway));
                let access_top = owner.access.iter().map(|at| at.y).max().unwrap();
                assert!(
                    terminal_access_cells(interface.pin.at, direction, access_top)
                        .iter()
                        .all(|at| owner.access.contains(at))
                );
                for sibling in packed.placements.values() {
                    if sibling.chunk != owner.chunk {
                        assert!(terminal_guard_cells(interface.pin.at, direction, top)
                            .iter()
                            .all(|at| !sibling.halo.contains(at)));
                    }
                }
            }
        }
    }

    #[test]
    fn explicit_bounds_refuse_without_searching_past_them() {
        let artifact = artifact("bounded", &[(2, 0, 0)]);
        assert!(matches!(
            pack_free_leaves_with_limits(
                &[artifact],
                PackingLimits {
                    max: Anchor { x: 1, y: 0, z: 0 },
                },
            ),
            Err(PackingError::NoPlacement { .. })
        ));
    }

    #[test]
    fn rejects_interface_not_owned_by_its_artifact_chunk() {
        let mut malformed = artifact("owned", &[]);
        let (_, interface) = malformed.interfaces.pop_first().unwrap();
        malformed.interfaces.insert(
            FreeLeafInterfaceId {
                chunk: chunk("other"),
                endpoint: PhysicalEndpointId::PrimaryInput(PortId(0)),
            },
            interface,
        );
        assert!(matches!(
            pack_free_leaves(&[malformed]),
            Err(PackingError::InterfaceChunkMismatch { .. })
        ));
    }

    #[test]
    fn rejects_interface_caller_cell_missing_from_access() {
        let mut malformed = artifact("access", &[]);
        malformed.interfaces.values_mut().next().unwrap().pin.at = Anchor { x: 9, y: 0, z: 0 };
        assert!(matches!(
            pack_free_leaves(&[malformed]),
            Err(PackingError::InterfaceOutsideAccess { .. })
        ));
    }

    #[test]
    fn classifies_unrepresentable_mask_span_as_coordinate_overflow() {
        let malformed = artifact("extreme", &[(i32::MIN, 0, 0)]);
        assert!(matches!(
            pack_free_leaves(&[malformed]),
            Err(PackingError::CoordinateOverflow { .. })
        ));
    }

    #[test]
    fn rejects_duplicate_chunks_before_order_dependent_work() {
        assert!(matches!(
            pack_free_leaves(&[artifact("same", &[]), artifact("same", &[])]),
            Err(PackingError::DuplicateChunk { .. })
        ));
    }

    #[test]
    fn composition_translates_blocks_without_emitting_halos() {
        let artifact = composable_artifact("compose");
        let packed = pack_free_leaves(&[artifact]).unwrap();
        let composed =
            compose_packed_free_leaf_worlds(&[composable_artifact("compose")], &packed).unwrap();
        assert_eq!(composed.world.size(), (3, 1, 1));
        assert_eq!(
            non_air(&composed.world),
            vec![(
                Anchor { x: 2, y: 0, z: 0 },
                composed.world.get(2, 0, 0).clone(),
            )]
        );
        assert_eq!(
            composed.occupied,
            BTreeSet::from([Anchor { x: 2, y: 0, z: 0 }])
        );
        assert_eq!(composed.world.get(0, 0, 0).kind, BlockKind::Air);
    }

    #[test]
    fn composition_ignores_artifact_input_order() {
        let a = composable_artifact("compose_a");
        let b = composable_artifact("compose_b");
        let forward_packed = pack_free_leaves(&[a, b]).unwrap();
        let forward = compose_packed_free_leaf_worlds(
            &[
                composable_artifact("compose_a"),
                composable_artifact("compose_b"),
            ],
            &forward_packed,
        )
        .unwrap();

        let a = composable_artifact("compose_a");
        let b = composable_artifact("compose_b");
        let reverse_packed = pack_free_leaves(&[b, a]).unwrap();
        let reverse = compose_packed_free_leaf_worlds(
            &[
                composable_artifact("compose_b"),
                composable_artifact("compose_a"),
            ],
            &reverse_packed,
        )
        .unwrap();

        assert_eq!(forward.occupied, reverse.occupied);
        assert_eq!(forward.world.size(), reverse.world.size());
        assert_eq!(non_air(&forward.world), non_air(&reverse.world));
    }

    #[test]
    fn composition_refuses_missing_extra_collision_and_malformed_placements() {
        let a = composable_artifact("compose_a");
        let b = composable_artifact("compose_b");
        let mut packed = pack_free_leaves(&[a, b]).unwrap();
        let first = packed.placements.values().next().unwrap().clone();
        let second_key = packed.placements.keys().nth(1).unwrap().clone();
        let second = packed.placements.get_mut(&second_key).unwrap();
        second.translation = first.translation;
        second.occupied = first.occupied.clone();
        assert!(matches!(
            compose_packed_free_leaf_worlds(
                &[
                    composable_artifact("compose_a"),
                    composable_artifact("compose_b")
                ],
                &packed
            ),
            Err(PackedWorldCompositionError::Collision { .. })
        ));

        let artifact = composable_artifact("missing");
        let mut packed = pack_free_leaves(&[artifact]).unwrap();
        packed.placements.clear();
        assert!(matches!(
            compose_packed_free_leaf_worlds(&[composable_artifact("missing")], &packed),
            Err(PackedWorldCompositionError::MissingPlacement { .. })
        ));

        let artifact = composable_artifact("extra");
        let mut packed = pack_free_leaves(&[artifact]).unwrap();
        let extra = chunk("unplaced");
        packed
            .placements
            .insert(extra, packed.placements.values().next().unwrap().clone());
        assert!(matches!(
            compose_packed_free_leaf_worlds(&[composable_artifact("extra")], &packed),
            Err(PackedWorldCompositionError::ExtraPlacement { .. })
        ));

        let artifact = composable_artifact("malformed");
        let mut packed = pack_free_leaves(&[artifact]).unwrap();
        packed
            .placements
            .values_mut()
            .next()
            .unwrap()
            .occupied
            .clear();
        assert!(matches!(
            compose_packed_free_leaf_worlds(&[composable_artifact("malformed")], &packed),
            Err(PackedWorldCompositionError::PlacementOccupiedMismatch { .. })
        ));
    }
}
