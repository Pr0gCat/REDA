//! **The lid fabric: every trunk of a packed node on three fixed layers.**
//!
//! Lanes stack one trunk a pitch above the last, so a dense node's height grew
//! with its trunk count and its terminals had to climb ever further in the
//! narrow seams between children. The fabric instead plans every trunk onto
//! three layers above the children's lid, whatever the count:
//!
//! | layer | dust at | runs along | carries |
//! |---|---|---|---|
//! | E | lid + 2 | x | the top of each terminal's pad, on to its column |
//! | Z | lid + 5 | z | one run per terminal, on a column of its own |
//! | X | lid + 8 | x | one spine per trunk, on a row shared left-edge |
//!
//! A terminal's **pad** is a straight dust staircase in the terminal's own row,
//! out from its runway, with a three-cell landing every [`LANDING_EVERY`]
//! levels -- a flat, straight cell a repeater can hold -- so a climb of any
//! height refreshes on the way. Layers change on three-level stairs.
//!
//! Every cell is planned before routing and conductors of different trunks
//! stay at L1 distance three or more (the leaf halo standard, proven in
//! `tests/fabric_kernels.rs`), so the router only replays the plan
//! ([`crate::compile::routing::PhysicalRouter::route_planned`]).
//!
//! Space is reserved by demand before placement: [`face_reach`] is how far out
//! a face's pads and columns reach, and a strip that leaves that much room
//! between neighbours always plans clean. A tighter strip lets two facing
//! combs share one seam; [`plan_clash`] says whether a plan on it still keeps
//! every trunk apart, and the node widens only the seams that do not.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::compile::geometry::Anchor;
use crate::redstone::world::block::Facing;

/// Levels a pad climbs between two landings.
pub(crate) const LANDING_EVERY: i32 = 12;
/// Lateral distance between neighbouring columns, rows and spines.
pub(crate) const PITCH: i32 = 3;

/// The three layer heights over a lid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layers {
    pub e: i32,
    pub z: i32,
    pub x: i32,
}

impl Layers {
    pub(crate) fn over(lid: i32) -> Self {
        Self {
            e: lid + 2,
            z: lid + 5,
            x: lid + 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum FabricError {
    #[error("terminal at {at:?} faces {facing:?}; the fabric routes east and west faces only")]
    Unsupported { at: Anchor, facing: Facing },
    #[error("terminal at {at:?} would need its pad above its layer")]
    TooHigh { at: Anchor },
}

/// One end of a trunk, as the planner sees it. `face` groups the terminals
/// that share one comb of columns: the same child and the same facing.
#[derive(Debug, Clone)]
pub(crate) struct FabricEnd<F> {
    pub anchor: Anchor,
    /// Away from the child: a source's exit, a sink's entry.
    pub facing: Facing,
    pub face: F,
}

#[derive(Debug, Clone)]
pub(crate) struct FabricTrunk<F> {
    pub source: FabricEnd<F>,
    pub sinks: Vec<FabricEnd<F>>,
}

/// Every trunk's paths, source anchor to sink anchor, one per sink in order.
#[derive(Debug, Clone)]
pub(crate) struct FabricPlan {
    pub paths: Vec<Vec<Vec<Anchor>>>,
    /// The largest coordinate any planned cell takes.
    pub max: Anchor,
}

fn step(at: Anchor, facing: Facing) -> Anchor {
    match facing {
        Facing::North => Anchor { z: at.z - 1, ..at },
        Facing::South => Anchor { z: at.z + 1, ..at },
        Facing::East => Anchor { x: at.x + 1, ..at },
        Facing::West => Anchor { x: at.x - 1, ..at },
        Facing::Up => Anchor { y: at.y + 1, ..at },
        Facing::Down => Anchor { y: at.y - 1, ..at },
    }
}

fn along(facing: Facing) -> i32 {
    match facing {
        Facing::East => 1,
        Facing::West => -1,
        _ => 0,
    }
}

/// A terminal's pad: `anchor`, its two runway cells, the mouth and one flat
/// cell, then a straight staircase along `facing` up to `top` with a
/// three-cell landing every [`LANDING_EVERY`] levels, then three flat cells.
/// A sink's pad is the same cells walked the other way.
pub(crate) fn pad(anchor: Anchor, facing: Facing, top: i32) -> Vec<Anchor> {
    let mut at = anchor;
    let mut cells = vec![at];
    let mut walk = |at: &mut Anchor, up: bool| {
        *at = step(*at, facing);
        if up {
            at.y += 1;
        }
        cells.push(*at);
    };
    for _ in 0..4 {
        walk(&mut at, false);
    }
    let mut climbed = 0;
    while at.y < top {
        walk(&mut at, true);
        climbed += 1;
        if climbed % LANDING_EVERY == 0 && at.y < top {
            for _ in 0..3 {
                walk(&mut at, false);
            }
        }
    }
    for _ in 0..3 {
        walk(&mut at, false);
    }
    cells
}

/// How far out along its facing a pad of `climb` levels ends.
pub(crate) fn pad_reach(climb: i32) -> i32 {
    let climb = climb.max(0);
    4 + climb + 3 * ((climb - 1).max(0) / LANDING_EVERY) + 3
}

/// How far out past its outermost terminal a face of `terminals` terminals,
/// each climbing `climb` levels, reaches: pads, columns, and the clearance
/// before whatever stands beyond.
pub(crate) fn face_reach(terminals: usize, climb: i32) -> i32 {
    if terminals == 0 {
        return 0;
    }
    pad_reach(climb) + PITCH * i32::try_from(terminals).unwrap_or(i32::MAX / PITCH) + PITCH
}

/// Plan every trunk onto the fabric over `lid`.
///
/// Deterministic: faces, columns and rows follow the order of `trunks` and
/// coordinates only.
pub(crate) fn plan_fabric<F: Ord + Clone>(
    trunks: &[FabricTrunk<F>],
    lid: i32,
    row_base: i32,
) -> Result<FabricPlan, FabricError> {
    let layers = Layers::over(lid);
    let ends = trunks
        .iter()
        .enumerate()
        .flat_map(|(trunk, t)| {
            std::iter::once((trunk, 0usize, &t.source))
                .chain(t.sinks.iter().enumerate().map(move |(k, s)| (trunk, k + 1, s)))
        })
        .collect::<Vec<_>>();
    for (_, _, end) in &ends {
        if along(end.facing) == 0 {
            return Err(FabricError::Unsupported {
                at: end.anchor,
                facing: end.facing,
            });
        }
        if end.anchor.y > layers.e {
            return Err(FabricError::TooHigh { at: end.anchor });
        }
    }

    // Columns: per face, from the outermost terminal, past the longest pad,
    // one per terminal at the pitch, ordered along z.
    // Per face and side: (z, trunk, end, anchor) of each terminal on it.
    type Members = Vec<(i32, usize, usize, Anchor)>;
    let mut faces: BTreeMap<(F, i32), Members> = BTreeMap::new();
    for &(trunk, end, e) in &ends {
        faces
            .entry((e.face.clone(), along(e.facing)))
            .or_default()
            .push((e.anchor.z, trunk, end, e.anchor));
    }
    let mut column: BTreeMap<(usize, usize), i32> = BTreeMap::new();
    for ((_, sign), mut members) in faces {
        members.sort();
        let top = members
            .iter()
            .map(|(_, _, _, at)| at.x * sign + pad_reach(layers.e - at.y))
            .max()
            .expect("a face has a terminal");
        for (k, (_, trunk, end, _)) in members.iter().enumerate() {
            let offset = top + PITCH + PITCH * i32::try_from(k).unwrap_or(i32::MAX / PITCH);
            column.insert((*trunk, *end), offset * sign);
        }
    }

    // Spine rows: left-edge over each trunk's column interval, never within a
    // pitch of the trunk's own terminal rows (its layer changes need that run).
    let base = row_base;
    let mut order = trunks
        .iter()
        .enumerate()
        .map(|(trunk, t)| {
            let cols = (0..=t.sinks.len())
                .map(|end| column[&(trunk, end)])
                .collect::<Vec<_>>();
            let lo = *cols.iter().min().expect("a trunk has a source");
            let hi = *cols.iter().max().expect("a trunk has a source");
            (lo, hi, trunk)
        })
        .collect::<Vec<_>>();
    order.sort();
    let mut row_end: Vec<i32> = Vec::new();
    let mut row_of = vec![0; trunks.len()];
    for (lo, hi, trunk) in order {
        let t = &trunks[trunk];
        let own = std::iter::once(&t.source)
            .chain(&t.sinks)
            .map(|end| end.anchor.z)
            .collect::<Vec<_>>();
        let mut j = 0;
        loop {
            let row = base + PITCH * i32::try_from(j).unwrap_or(i32::MAX / PITCH);
            let free = row_end.get(j).is_none_or(|end| *end + PITCH <= lo);
            let clear = own.iter().all(|z| (row - z).abs() >= PITCH);
            if free && clear {
                if j == row_end.len() {
                    row_end.push(hi);
                } else {
                    row_end[j] = hi;
                }
                row_of[trunk] = row;
                break;
            }
            if j == row_end.len() {
                row_end.push(i32::MIN / 2);
            }
            j += 1;
        }
    }

    let leg = |end: &FabricEnd<F>, col: i32, row: i32| -> Vec<Anchor> {
        let mut cells = pad(end.anchor, end.facing, layers.e);
        let mut at = *cells.last().expect("a pad has cells");
        while (col - at.x).abs() > PITCH {
            at = step(at, end.facing);
            cells.push(at);
        }
        for _ in 0..PITCH {
            at = step(at, end.facing);
            at.y += 1;
            cells.push(at);
        }
        let s = (row - at.z).signum();
        while (row - at.z).abs() > PITCH {
            at.z += s;
            cells.push(at);
        }
        for _ in 0..PITCH {
            at.z += s;
            at.y += 1;
            cells.push(at);
        }
        cells
    };

    let mut max = Anchor { x: 0, y: 0, z: 0 };
    let paths = trunks
        .iter()
        .enumerate()
        .map(|(trunk, t)| {
            let row = row_of[trunk];
            let source = leg(&t.source, column[&(trunk, 0)], row);
            let junction = *source.last().expect("a leg has cells");
            t.sinks
                .iter()
                .enumerate()
                .map(|(k, sink)| {
                    let mut down = leg(sink, column[&(trunk, k + 1)], row);
                    let meet = *down.last().expect("a leg has cells");
                    down.pop();
                    let mut path = source.clone();
                    let mut at = junction;
                    let s = (meet.x - at.x).signum();
                    while at != meet {
                        at.x += s;
                        path.push(at);
                    }
                    path.extend(down.into_iter().rev());
                    for cell in &path {
                        max = Anchor {
                            x: max.x.max(cell.x),
                            y: max.y.max(cell.y),
                            z: max.z.max(cell.z),
                        };
                    }
                    path
                })
                .collect()
        })
        .collect();
    Ok(FabricPlan { paths, max })
}

/// How many cells of a leg, from its terminal, are the pad's own approach:
/// the anchor and the four flat cells that leave the child's halo.
const APPROACH: usize = 5;

/// The first cell at which `plan` breaks what the fabric promises, in plan
/// order: a conductor within two of another trunk's -- inside the two-hop
/// coupling ball a leaf halo claims -- or, past a pad's own approach, a
/// conductor inside `halo`.
///
/// A strip that reserves every face's full reach keeps both by construction.
/// A tighter one shares the room between neighbours, and this is what says
/// whether it still holds.
pub(crate) fn plan_clash(plan: &FabricPlan, halo: &BTreeSet<Anchor>) -> Option<Anchor> {
    let mut owner = BTreeMap::new();
    for (trunk, paths) in plan.paths.iter().enumerate() {
        for cell in paths.iter().flatten() {
            owner.entry(*cell).or_insert(trunk);
        }
    }
    let mut ball = Vec::new();
    for dx in -2i32..=2 {
        for dy in -2i32..=2 {
            for dz in -2i32..=2 {
                if (1..=2).contains(&(dx.abs() + dy.abs() + dz.abs())) {
                    ball.push((dx, dy, dz));
                }
            }
        }
    }
    for (trunk, paths) in plan.paths.iter().enumerate() {
        for path in paths {
            for (index, cell) in path.iter().enumerate() {
                let approach = index < APPROACH || index + APPROACH >= path.len();
                if !approach && halo.contains(cell) {
                    return Some(*cell);
                }
                let foreign = ball.iter().any(|&(dx, dy, dz)| {
                    let near = Anchor {
                        x: cell.x + dx,
                        y: cell.y + dy,
                        z: cell.z + dz,
                    };
                    owner.get(&near).is_some_and(|other| *other != trunk)
                });
                if foreign {
                    return Some(*cell);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strip a node reserves by face reach plans clean, and the same
    /// trunks pulled together until two faces' combs meet do not.
    #[test]
    fn a_plan_clash_is_found_where_two_combs_meet_and_nowhere_else() {
        let trunks = face_trunks(4);
        let plan = plan_fabric(&trunks, 12, 1).unwrap();
        assert_eq!(plan_clash(&plan, &BTreeSet::new()), None);
        let squeezed = trunks
            .iter()
            .map(|trunk| FabricTrunk {
                source: trunk.source.clone(),
                sinks: trunk
                    .sinks
                    .iter()
                    .map(|sink| FabricEnd {
                        anchor: at(trunk.source.anchor.x + 4, sink.anchor.y, sink.anchor.z),
                        ..sink.clone()
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        let plan = plan_fabric(&squeezed, 12, 1).unwrap();
        assert!(plan_clash(&plan, &BTreeSet::new()).is_some());
    }

    fn at(x: i32, y: i32, z: i32) -> Anchor {
        Anchor { x, y, z }
    }

    fn adjacent(a: Anchor, b: Anchor) -> bool {
        a.x.abs_diff(b.x) + a.z.abs_diff(b.z) == 1 && a.y.abs_diff(b.y) <= 1
    }

    /// A pad of any height is one connected walk that ends on its layer,
    /// and no stretch of stair between two landings climbs more than
    /// [`LANDING_EVERY`] levels.
    #[test]
    fn a_pad_climbs_any_height_with_a_landing_every_twelve_levels() {
        for climb in 0..=256 {
            let cells = pad(at(0, 3, 0), Facing::East, 3 + climb);
            assert!(cells.windows(2).all(|w| adjacent(w[0], w[1])));
            assert_eq!(cells.last().unwrap().y, 3 + climb);
            assert_eq!(cells.last().unwrap().x, pad_reach(climb), "climb {climb}");
            let mut run = 0;
            for w in cells.windows(2) {
                if w[1].y != w[0].y {
                    run += 1;
                    assert!(run <= LANDING_EVERY, "climb {climb}");
                } else {
                    run = 0;
                }
            }
        }
    }

    /// `n` trunks from the east face of child 0 to the west face of child 1
    /// and on to the west face of child 2, rows 6 apart, the faces as far
    /// apart as a strip reserves ([`face_reach`]).
    fn face_trunks(n: usize) -> Vec<FabricTrunk<u32>> {
        let reach = face_reach(n, 12 + 2 - 3);
        let (x0, x1) = (40, 40 + 2 * reach + 1);
        let x2 = x1 + 2 * reach + 1;
        (0..n)
            .map(|i| {
                let z = 4 + 6 * i32::try_from(i).unwrap();
                FabricTrunk {
                    source: FabricEnd {
                        anchor: at(x0, 3, z),
                        facing: Facing::East,
                        face: 0,
                    },
                    sinks: vec![
                        FabricEnd {
                            anchor: at(x1, 3, z + 3 * i32::from(i % 2 == 0)),
                            facing: Facing::West,
                            face: 1,
                        },
                        FabricEnd {
                            anchor: at(x2, 3, 4 + 6 * i32::try_from(n - 1 - i).unwrap()),
                            facing: Facing::West,
                            face: 2,
                        },
                    ],
                }
            })
            .collect()
    }

    /// Conductors of different trunks never come within L1 distance three,
    /// every path is one connected walk from its source to its sink, and the
    /// fabric's height is the same for one trunk as for sixty-four.
    #[test]
    fn trunks_stay_apart_and_the_height_is_independent_of_their_count() {
        for n in [1, 2, 5, 16, 64] {
            let trunks = face_trunks(n);
            let plan = plan_fabric(&trunks, 12, 1).unwrap();
            assert_eq!(plan.max.y, 12 + 8, "{n} trunks");
            let mut owner: BTreeMap<Anchor, usize> = BTreeMap::new();
            for (trunk, paths) in plan.paths.iter().enumerate() {
                for (k, path) in paths.iter().enumerate() {
                    assert_eq!(path[0], trunks[trunk].source.anchor);
                    assert_eq!(*path.last().unwrap(), trunks[trunk].sinks[k].anchor);
                    assert!(path.windows(2).all(|w| adjacent(w[0], w[1])));
                    for cell in path {
                        owner.insert(*cell, trunk);
                    }
                }
            }
            let cells = owner.iter().collect::<Vec<_>>();
            for (i, (a, ta)) in cells.iter().enumerate() {
                for (b, tb) in &cells[i + 1..] {
                    if ta != tb {
                        let l1 = a.x.abs_diff(b.x) + a.y.abs_diff(b.y) + a.z.abs_diff(b.z);
                        assert!(l1 >= 3, "{n} trunks: {a:?} (#{ta}) and {b:?} (#{tb}) at {l1}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_plan_does_not_depend_on_anything_but_its_trunks() {
        let a = plan_fabric(&face_trunks(9), 12, 1).unwrap();
        let b = plan_fabric(&face_trunks(9), 12, 1).unwrap();
        assert_eq!(a.paths, b.paths);
    }

    #[test]
    fn a_face_reach_holds_its_pads_and_columns() {
        for n in [1, 5, 16] {
            let plan = plan_fabric(&face_trunks(n), 12, 1).unwrap();
            let reach = face_reach(n, 12 + 2 - 3);
            for paths in &plan.paths {
                // The source leg: every cell before the spine layer.
                for cell in paths[0].iter().take_while(|cell| cell.y < 12 + 8) {
                    assert!(cell.x - 40 < reach, "{n}: {cell:?} beyond reach {reach}");
                }
            }
        }
    }
}
