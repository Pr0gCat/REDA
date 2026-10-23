//! A baseline generator: the placement and routing method of dewey, ported
//! onto this crate's tiles, grid and electrical rules, so it can be compared
//! like for like with the constructive generator ([`super::construct`]).
//!
//! Quan Nguyen. 2018. *Incremental Improvements on the Placement and Routing
//! of Minecraft Redstone Circuits.* SIGTBD '18, Cambridge, MA. Source:
//! <https://github.com/qmn/dewey> (BSD licence), the C successor of Nguyen's
//! PERSHING (2016).
//!
//! What is dewey's:
//! - Placement is TimberWolf simulated annealing (Sechen and
//!   Sangiovanni-Vincentelli 1985), scoring every candidate in full: overlap
//!   (squared, with a margin), minimum-spanning-tree wire length, distance
//!   outside a 100 x 100 boundary, design area, spread from the centroid,
//!   and squared congestion. A move interchanges two cells or displaces one
//!   by a Gaussian step in a window that shrinks with temperature. Inputs
//!   keep to the west edge, outputs to the east. T0 = 100, alpha = 0.9, 100
//!   generations per temperature, at least 100 temperatures, stopping once
//!   the score has held for 100 and nothing overlaps. As in dewey, each
//!   candidate is judged against the score the temperature started with.
//! - Routing starts every net as straight city-block runs that ignore
//!   everything, then repeats Silk's natural selection: every segment is
//!   ripped up with a probability rising with its score (1000 per violating
//!   cell plus its length), and each net that lost one is rerouted by Lee's
//!   maze algorithm, in which a violation costs 1000 more rather than being
//!   forbidden. A move costs 1, plus 5 to turn, 20 to change level, half the
//!   level, 4 near the edge and 10 against the level's preferred direction.
//!   Once nothing violates, every net is rerouted again in random order and
//!   kept only if the total gets better, until a round changes nothing.
//! - Repeaters go in after routing, on straight runs, as late as the signal
//!   allows while it stays at strength 3 or more.
//!
//! Where the port differs, because the tiles differ: tiles are never turned,
//! so there are no reorient moves; a level change is a dust stair rather
//! than dewey's torch-and-piston via three levels up, and the preferred
//! direction alternates every three levels; each sink joins the nearest
//! point of its net's tree as it stands (Prim) rather than all of a net's
//! groups growing at once; a violation is this crate's gap rule (no block of
//! another owner within one cell, see [`super::grid`]), the 3 x 3 check
//! dewey's usage matrix makes; a segment no repeater fits on counts as a
//! violation, where dewey printed a warning and emitted a dead wire; and
//! annealed positions are snapped to the nearest position the gap rule
//! accepts. Both loops stop at a deadline, where dewey ran until
//! interrupted.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::{Duration, Instant};

use super::grid::{around, CellMap, CellSet, Grid, Owner};
use super::route::{self, Goal, HIGHEST, LOWEST};
use super::Builder;
use crate::compile::geometry::Anchor;
use crate::redstone::world::block::Facing;

/// dewey's xorshift-free `random()`: any deterministic generator will do.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Box-Muller, as dewey draws its displacements.
    fn gauss(&mut self, sigma: f64) -> f64 {
        let u1 = self.unit().max(f64::MIN_POSITIVE);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos() * sigma
    }
}

/// How long either loop may run before the baseline gives up.
const DEADLINE: Duration = Duration::from_secs(20 * 60);

// ---------------------------------------------------------------- placement

/// Which edge a cell is held to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Free,
    West,
    East,
}

/// One cell to place: its box relative to its origin, its pins (net and
/// offset from the origin) and the edge it keeps to.
pub struct Shape {
    pub low_x: i32,
    pub high_x: i32,
    pub low_z: i32,
    pub high_z: i32,
    pub height: i32,
    pub pins: Vec<(usize, (i32, i32))>,
    pub side: Side,
}

impl Shape {
    fn size(&self) -> (i32, i32) {
        (self.high_x - self.low_x + 1, self.high_z - self.low_z + 1)
    }
}

const EDGE_MARGIN: i32 = 4;
const MIN_WINDOW: i64 = 3;
const MAX_WINDOW: i64 = 20;
const INTERCHANGE_RATIO: f64 = 5.0;
const T0: f64 = 100.0;
const ALPHA: f64 = 0.9;
const TEMPERATURES: usize = 100;
const GENERATIONS: usize = 100;
const HOLD: usize = 100;
const WANTED: i32 = 100;

/// How much two spans overlap.
fn overlap(s1: i32, e1: i32, s2: i32, e2: i32) -> i32 {
    let space = e1.max(e2) - s1.min(s2);
    let taken = (e1 - s1) + (e2 - s2);
    (taken - space).max(0)
}

struct Placer<'a> {
    shapes: &'a [Shape],
    /// Each net's pins, as (cell, offset).
    nets: Vec<Vec<(usize, (i32, i32))>>,
}

impl Placer<'_> {
    /// The overlap volume of two boxes `margin` apart, with dewey's one-cell
    /// no-touch rim.
    fn overlaps(&self, at: &[(i32, i32)], p: usize, q: usize, margin: i32) -> i64 {
        let d = 1 + margin;
        let ((px, pz), (qx, qz)) = (at[p], at[q]);
        let ((pdx, pdz), (qdx, qdz)) = (self.shapes[p].size(), self.shapes[q].size());
        let zo = overlap(pz - d, pz + pdz + d, qz - d, qz + qdz + d);
        let xo = overlap(px - d, px + pdx + d, qx - d, qx + qdx + d);
        let yo = overlap(-d, self.shapes[p].height + d, -d, self.shapes[q].height + d);
        if zo == 0 || xo == 0 || yo == 0 {
            return 0;
        }
        let shrink = |o: i32| i64::from((o - margin).max(0));
        shrink(xo) * shrink(yo) * shrink(zo)
    }

    /// Overlapping volume (with no margin) and the margin-overlap penalty.
    fn overlap_penalty(&self, at: &[(i32, i32)]) -> (i64, f64) {
        let (mut violations, mut score) = (0, 0.0);
        for p in 0..at.len() {
            for q in p + 1..at.len() {
                violations += self.overlaps(at, p, q, 0);
                let volume = self.overlaps(at, p, q, EDGE_MARGIN);
                if volume > 0 {
                    score += (volume as f64).powi(2);
                }
            }
        }
        (violations, score)
    }

    fn pin(&self, at: &[(i32, i32)], (cell, (dx, dz)): (usize, (i32, i32))) -> (i32, i32) {
        let shape = &self.shapes[cell];
        (at[cell].0 - shape.low_x + dx, at[cell].1 - shape.low_z + dz)
    }

    /// Each net's minimum spanning tree over its pins (Prim; dewey uses
    /// Kruskal, which finds the same tree).
    fn spanning_segments(&self, at: &[(i32, i32)]) -> Vec<((i32, i32), (i32, i32))> {
        let mut segments = Vec::new();
        for pins in &self.nets {
            let points: Vec<(i32, i32)> = pins.iter().map(|&pin| self.pin(at, pin)).collect();
            if points.len() < 2 {
                continue;
            }
            let mut joined = vec![false; points.len()];
            let mut best: Vec<(f64, usize)> = vec![(f64::INFINITY, 0); points.len()];
            joined[0] = true;
            for other in 1..points.len() {
                best[other] = (distance(points[0], points[other]), 0);
            }
            for _ in 1..points.len() {
                let next = (0..points.len())
                    .filter(|&point| !joined[point])
                    .min_by(|&a, &b| best[a].0.total_cmp(&best[b].0))
                    .unwrap();
                joined[next] = true;
                segments.push((points[best[next].1], points[next]));
                for other in 0..points.len() {
                    if !joined[other] {
                        let d = distance(points[next], points[other]);
                        if d < best[other].0 {
                            best[other] = (d, next);
                        }
                    }
                }
            }
        }
        segments
    }

    fn dimensions(&self, at: &[(i32, i32)], skip_east: bool) -> (i32, i32) {
        let mut dims = (0, 0);
        for (cell, &(x, z)) in at.iter().enumerate() {
            let (dx, dz) = self.shapes[cell].size();
            if !(skip_east && self.shapes[cell].side == Side::East) {
                dims.0 = dims.0.max(x + dx + 1);
            }
            dims.1 = dims.1.max(z + dz + 1);
        }
        dims
    }

    fn score(&self, at: &[(i32, i32)]) -> f64 {
        let (_, overlap_score) = self.overlap_penalty(at);
        let segments = self.spanning_segments(at);
        let wire_length: f64 = segments.iter().map(|&(a, b)| distance(a, b).floor()).sum();
        // dewey's distance outside the boundary: sqrt(dx * dx) + dz * dz.
        let bounds: f64 = at
            .iter()
            .map(|&(x, z)| {
                let outside = |v: i32| {
                    if v > WANTED {
                        v - WANTED
                    } else if v < 0 {
                        -v
                    } else {
                        0
                    }
                };
                f64::from(outside(x)) + f64::from(outside(z)).powi(2)
            })
            .sum();
        let (width, depth) = self.dimensions(at, false);
        let size = f64::from(width) * f64::from(depth);
        let count = at.len() as i32;
        let centre = (
            at.iter().map(|p| p.0).sum::<i32>() / count,
            at.iter().map(|p| p.1).sum::<i32>() / count,
        );
        let spread: f64 = at.iter().map(|&p| distance(p, centre)).sum();
        let mut congestion = 0.0;
        for &(a, b) in &segments {
            let (dx, dz) = ((a.0 - b.0).abs() + 1, (a.1 - b.1).abs() + 1);
            let factor = f64::from(dx + dz) / f64::from(dx * dz);
            for (cell, &(x, z)) in at.iter().enumerate() {
                let (sx, sz) = self.shapes[cell].size();
                let ox = overlap(x, x + sx, a.0.min(b.0), a.0.max(b.0));
                let oz = overlap(z, z + sz, a.1.min(b.1), a.1.max(b.1));
                congestion += factor * f64::from(ox) * f64::from(oz);
            }
        }
        overlap_score + wire_length + bounds + size + spread + congestion.powi(2)
    }

    /// Move the free cells up against the north-west corner and the edge
    /// cells onto their edges.
    fn recentre(&self, at: &mut [(i32, i32)]) {
        let free = (0..at.len()).filter(|&cell| self.shapes[cell].side != Side::West);
        let (low_x, low_z) = free.fold((i32::MAX, i32::MAX), |(lx, lz), cell| {
            (lx.min(at[cell].0), lz.min(at[cell].1))
        });
        for (cell, place) in at.iter_mut().enumerate() {
            if self.shapes[cell].side == Side::West {
                place.0 = 0;
            } else {
                place.0 -= low_x - EDGE_MARGIN;
                place.1 -= low_z;
            }
        }
        let (width, _) = self.dimensions(at, true);
        for (cell, place) in at.iter_mut().enumerate() {
            match self.shapes[cell].side {
                Side::West => place.0 = 0,
                Side::East => place.0 = width + self.shapes[cell].size().0 + EDGE_MARGIN,
                Side::Free => {}
            }
        }
    }
}

fn distance(a: (i32, i32), b: (i32, i32)) -> f64 {
    f64::from((a.0 - b.0).pow(2) + (a.1 - b.1).pow(2)).sqrt()
}

/// Each cell's origin, by dewey's annealing.
pub fn place(shapes: &[Shape]) -> Vec<(i32, i32)> {
    let mut nets: Vec<Vec<(usize, (i32, i32))>> = Vec::new();
    for (cell, shape) in shapes.iter().enumerate() {
        for &(net, offset) in &shape.pins {
            if nets.len() <= net {
                nets.resize(net + 1, Vec::new());
            }
            nets[net].push((cell, offset));
        }
    }
    let placer = Placer { shapes, nets };
    // dewey's initial placement: inputs down the west column, outputs down
    // the east one, everything else in a square between.
    let count = shapes.len();
    let side = (count as f64).sqrt().ceil() as i32;
    let (mut west, mut east, mut free) = (0, 0, 0);
    let mut at: Vec<(i32, i32)> = shapes
        .iter()
        .map(|shape| match shape.side {
            Side::West => {
                west += 1;
                (0, EDGE_MARGIN * (west - 1))
            }
            Side::East => {
                east += 1;
                ((side + 2) * EDGE_MARGIN, EDGE_MARGIN * (east - 1))
            }
            Side::Free => {
                free += 1;
                (
                    ((free - 1) % side + 1) * EDGE_MARGIN,
                    (free - 1) / side * EDGE_MARGIN,
                )
            }
        })
        .collect();
    if count < 2 || shapes.iter().all(|shape| shape.side != Side::Free) {
        return origins(shapes, &at);
    }
    let started = Instant::now();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut temperature = T0;
    let mut old_score = placer.score(&at);
    let (mut held, mut match_score) = (0, old_score);
    let mut displace = true;
    let mut round = 0;
    loop {
        let mut taken = old_score;
        let mut generation = 0;
        while generation < GENERATIONS {
            let dims = placer.dimensions(&at, false);
            let scale = temperature.ln() / T0.ln();
            let window = |d: i32| {
                ((f64::from(d.max(WANTED)) * scale).round() as i64).clamp(MIN_WINDOW, MAX_WINDOW)
            };
            let (window_x, window_z) = (window(dims.0), window(dims.1));
            let mut next = at.clone();
            let mut a = rng.below(count as u64) as usize;
            let moved = if rng.unit() > 1.0 / INTERCHANGE_RATIO {
                while shapes[a].side != Side::Free {
                    a = rng.below(count as u64) as usize;
                }
                let mut b = rng.below(count as u64) as usize;
                while b == a {
                    b = rng.below(count as u64) as usize;
                }
                if i64::from((next[a].0 - next[b].0).abs()) <= window_x
                    && i64::from((next[a].1 - next[b].1).abs()) <= window_z
                {
                    next.swap(a, b);
                    displace = true;
                    true
                } else {
                    false
                }
            } else if displace {
                // dewey draws dz first and keeps dx at zero whenever dz is not.
                let (mut dx, mut dz) = (0, 0);
                while dz == 0 && dx == 0 {
                    dz = rng.gauss(window_z as f64).round() as i32;
                    if dz == 0 {
                        dx = rng.gauss(window_x as f64).round() as i32;
                    }
                }
                next[a].1 += dz;
                if shapes[a].side == Side::Free {
                    next[a].0 += dx;
                }
                displace = false;
                true
            } else {
                // A reorient: tiles are never turned.
                displace = true;
                false
            };
            if !moved {
                continue;
            }
            generation += 1;
            placer.recentre(&mut next);
            let score = placer.score(&next);
            let accept = rng.unit() < (-(score - old_score) / temperature).exp().min(1.0);
            if accept {
                at = next;
                taken = score;
            } else {
                taken = old_score;
            }
        }
        if taken == match_score {
            held += 1;
        } else {
            match_score = taken;
            held = 0;
        }
        old_score = taken;
        let (violations, _) = placer.overlap_penalty(&at);
        temperature *= ALPHA;
        round += 1;
        let done = round > TEMPERATURES && held >= HOLD && violations == 0;
        if done || started.elapsed() > DEADLINE {
            break;
        }
    }
    origins(shapes, &at)
}

fn origins(shapes: &[Shape], at: &[(i32, i32)]) -> Vec<(i32, i32)> {
    shapes
        .iter()
        .zip(at)
        .map(|(shape, &(x, z))| (x - shape.low_x, z - shape.low_z))
        .collect()
}

// ------------------------------------------------------------------ routing

const VIOLATION: u64 = 1000;
const TURN: u64 = 5;
const VIA: u64 = 20;
const EDGE: u64 = 4;
const AGAINST: u64 = 10;
const BOUND_MARGIN: i32 = 2;
const MIN_STRENGTH: u8 = 3;

const HORIZONTAL: [(Facing, i32, i32); 4] = [
    (Facing::North, 0, -1),
    (Facing::South, 0, 1),
    (Facing::East, 1, 0),
    (Facing::West, -1, 0),
];

fn down(at: Anchor) -> Anchor {
    Anchor { y: at.y - 1, ..at }
}

fn up(at: Anchor) -> Anchor {
    Anchor { y: at.y + 1, ..at }
}

fn chebyshev(a: Anchor, b: Anchor) -> i32 {
    (a.x - b.x)
        .abs()
        .max((a.y - b.y).abs())
        .max((a.z - b.z).abs())
}

fn manhattan(a: Anchor, b: Anchor) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()
}

#[derive(Clone, Copy)]
struct Cell {
    at: Anchor,
    /// The cell a stair into this one needs left as air.
    air: Option<Anchor>,
    repeater: Option<Facing>,
    strength: u8,
}

/// One routed segment: from a tree cell (`cells[0]`, not placed) to a
/// sink's approach (the last cell, part of the tile, not placed).
#[derive(Clone)]
struct Wire {
    net: usize,
    goal: Goal,
    cells: Vec<Cell>,
    repeatable: bool,
    score: u64,
    violations: u64,
    alive: bool,
}

impl Wire {
    fn placed(&self) -> &[Cell] {
        &self.cells[1..self.cells.len() - 1]
    }

    fn tap(&self) -> Anchor {
        self.cells[0].at
    }
}

/// Every wire's blocks (cells and floors), which may overlap -- a
/// violation, not an impossibility -- and the stair air they reserve.
#[derive(Default)]
struct Layer {
    blocks: CellMap<Anchor, Vec<usize>>,
    air: CellMap<Anchor, Vec<usize>>,
}

struct Router<'a> {
    grid: &'a Grid,
    layer: Layer,
    wires: Vec<Wire>,
    /// Each net's tile taps.
    sources: Vec<Vec<(Anchor, u8)>>,
    low: Anchor,
    high: Anchor,
}

impl Router<'_> {
    fn insert(&mut self, wire: usize) {
        for cell in self.wires[wire].placed().to_vec() {
            for at in [cell.at, down(cell.at)] {
                self.layer.blocks.entry(at).or_default().push(wire);
            }
            if let Some(air) = cell.air {
                self.layer.air.entry(air).or_default().push(wire);
            }
        }
    }

    fn remove(&mut self, wire: usize) {
        for cell in self.wires[wire].placed().to_vec() {
            for at in [cell.at, down(cell.at)] {
                let list = self.layer.blocks.get_mut(&at).unwrap();
                list.retain(|&other| other != wire);
                if list.is_empty() {
                    self.layer.blocks.remove(&at);
                }
            }
            if let Some(air) = cell.air {
                let list = self.layer.air.get_mut(&air).unwrap();
                list.retain(|&other| other != wire);
                if list.is_empty() {
                    self.layer.air.remove(&air);
                }
            }
        }
        self.wires[wire].alive = false;
    }

    /// Whether a block of `net` at `at` breaks the gap rule against the
    /// tiles and every wire but `own`; `first` lets it stand beside the
    /// tree cell `tap` its wire leaves from.
    fn violates(
        &self,
        at: Anchor,
        net: usize,
        own: Option<usize>,
        tap: Anchor,
        first: bool,
        goal: &Goal,
    ) -> bool {
        let owner = Owner::Net(net);
        if !self.grid.is_free(at) || self.grid.foreign_near(at, owner) {
            return true;
        }
        let allowed =
            |other: Anchor| (first && chebyshev(other, tap) <= 1) || goal.stub.contains(&other);
        if self.grid.own_near(at, owner) && !self.grid.own_blocks_near(at, owner).all(allowed) {
            return true;
        }
        if self
            .layer
            .blocks
            .get(&at)
            .is_some_and(|list| list.iter().any(|&other| Some(other) != own))
        {
            return true;
        }
        around(at).any(|cell| {
            self.layer.blocks.get(&cell).is_some_and(|list| {
                list.iter().any(|&other| {
                    Some(other) != own
                        && (self.wires[other].net != net || !(first && chebyshev(cell, tap) <= 1))
                })
            })
        })
    }

    fn air_taken(&self, air: Anchor, own: Option<usize>) -> bool {
        !self.grid.is_free(air)
            || self
                .layer
                .blocks
                .get(&air)
                .is_some_and(|list| list.iter().any(|&other| Some(other) != own))
    }

    /// The tree a new segment of `net` may leave from.
    fn taps(&self, net: usize) -> Vec<(Anchor, u8)> {
        let mut taps = self.sources[net].clone();
        for wire in self
            .wires
            .iter()
            .filter(|wire| wire.alive && wire.net == net)
        {
            taps.extend(
                wire.cells[1..]
                    .iter()
                    .filter(|cell| cell.repeater.is_none())
                    .map(|cell| (cell.at, cell.strength)),
            );
        }
        taps
    }

    fn movement_cost(
        &self,
        from: Anchor,
        heading: Option<(i32, i32)>,
        (dx, dz): (i32, i32),
        dy: i32,
    ) -> u64 {
        let turn = if dy == 0 && heading.is_some_and(|heading| heading != (dx, dz)) {
            TURN
        } else {
            0
        };
        let via = if dy != 0 { VIA } else { 0 };
        let level = ((from.y - LOWEST) / 2) as u64;
        let edge = if from.x < self.low.x + BOUND_MARGIN
            || from.x > self.high.x - BOUND_MARGIN
            || from.z < self.low.z + BOUND_MARGIN
            || from.z > self.high.z - BOUND_MARGIN
        {
            EDGE
        } else {
            0
        };
        let band = (from.y - LOWEST) / 3;
        let against = if (band % 2 == 0 && dz != 0) || (band % 2 == 1 && dx != 0) {
            AGAINST
        } else {
            0
        };
        1 + turn + via + level + edge + against
    }

    /// Lee's algorithm from `taps` to `goal`, violations priced not
    /// forbidden.
    fn maze(&self, net: usize, taps: &[(Anchor, u8)], goal: &Goal) -> Option<Vec<Cell>> {
        #[derive(Clone, Copy)]
        struct Node {
            at: Anchor,
            parent: Option<usize>,
            heading: Option<(i32, i32)>,
            air: Option<Anchor>,
            tap: Anchor,
            depth: usize,
        }
        let tree: CellSet<Anchor> = taps.iter().map(|(at, _)| *at).collect();
        let target = goal.approach;
        let mut nodes: Vec<Node> = Vec::new();
        let mut best: CellMap<Anchor, u64> = CellMap::default();
        let mut open = BinaryHeap::new();
        for &(at, _) in taps {
            nodes.push(Node {
                at,
                parent: None,
                heading: None,
                air: None,
                tap: at,
                depth: 0,
            });
            best.insert(at, 0);
            open.push(Reverse((0u64, nodes.len() - 1)));
        }
        while let Some(Reverse((cost, index))) = open.pop() {
            let node = nodes[index];
            if node.at == target {
                let mut cells = Vec::new();
                let mut cursor = Some(index);
                while let Some(at) = cursor {
                    let node = nodes[at];
                    cells.push(Cell {
                        at: node.at,
                        air: node.air,
                        repeater: None,
                        strength: 0,
                    });
                    cursor = node.parent;
                }
                cells.reverse();
                return Some(cells);
            }
            if best.get(&node.at).is_some_and(|&known| known < cost) {
                continue;
            }
            // The path behind, for the clashes the grid cannot see yet: its
            // blocks with how many steps back they are, and its stair air.
            let mut behind: Vec<(Anchor, usize)> = Vec::new();
            let mut behind_air: Vec<Anchor> = Vec::new();
            let mut cursor = Some(index);
            while let Some(at) = cursor {
                let step = nodes[at];
                if step.parent.is_none() || node.depth - step.depth > 8 {
                    break;
                }
                behind.push((step.at, node.depth - step.depth));
                behind.push((down(step.at), node.depth - step.depth));
                behind_air.extend(step.air);
                cursor = step.parent;
            }
            for (_, dx, dz) in HORIZONTAL {
                for dy in [0, 1, -1] {
                    if dy != 0 && node.parent.is_none() {
                        continue;
                    }
                    let next = Anchor {
                        x: node.at.x + dx,
                        y: node.at.y + dy,
                        z: node.at.z + dz,
                    };
                    if !(LOWEST..=HIGHEST).contains(&next.y)
                        || next.x < self.low.x
                        || next.x > self.high.x
                        || next.z < self.low.z
                        || next.z > self.high.z
                        || (next != target && tree.contains(&next))
                    {
                        continue;
                    }
                    let air = match dy {
                        1 => Some(up(node.at)),
                        -1 => Some(up(next)),
                        _ => None,
                    };
                    // The approach is the tile's; only a stair's air into it
                    // is the path's to keep.
                    let clash = next != target
                        && [next, down(next)].into_iter().any(|new| {
                            behind_air.contains(&new)
                                || behind.iter().any(|&(old, depth)| {
                                    new == old || (depth >= 2 && chebyshev(new, old) <= 1)
                                })
                        });
                    if clash || air.is_some_and(|air| behind.iter().any(|&(old, _)| old == air)) {
                        continue;
                    }
                    let first = node.parent.is_none();
                    let violation = (next != target
                        && [next, down(next)]
                            .into_iter()
                            .any(|at| self.violates(at, net, None, node.tap, first, goal)))
                        || air.is_some_and(|air| self.air_taken(air, None));
                    let step = self.movement_cost(node.at, node.heading, (dx, dz), dy)
                        + if violation { VIOLATION } else { 0 };
                    let next_cost = cost + step;
                    if best.get(&next).is_some_and(|&known| known <= next_cost) {
                        continue;
                    }
                    best.insert(next, next_cost);
                    nodes.push(Node {
                        at: next,
                        parent: Some(index),
                        heading: (dy == 0).then_some((dx, dz)),
                        air,
                        tap: node.tap,
                        depth: node.depth + 1,
                    });
                    open.push(Reverse((next_cost, nodes.len() - 1)));
                }
            }
        }
        None
    }

    /// dewey's city-block run from the nearest tree cell, ignoring
    /// everything; across levels it is not even connected, a violation
    /// for the rip-up loop to clear.
    fn dumb(&self, taps: &[(Anchor, u8)], goal: &Goal) -> Vec<Cell> {
        let target = goal.approach;
        let &(tap, _) = taps
            .iter()
            .min_by_key(|(at, _)| manhattan(*at, target))
            .expect("a driven net has a tap");
        let mut cells = vec![tap];
        let mut at = tap;
        while at.x != target.x {
            at.x += (target.x - at.x).signum();
            cells.push(at);
        }
        while at.z != target.z {
            at.z += (target.z - at.z).signum();
            cells.push(at);
        }
        if at.y != target.y {
            cells.push(target);
        }
        cells
            .into_iter()
            .map(|at| Cell {
                at,
                air: None,
                repeater: None,
                strength: 0,
            })
            .collect()
    }

    /// dewey's repeater insertion: walk from the tap and, where the signal
    /// would fall below [`MIN_STRENGTH`], put a repeater on the last cell
    /// before that point that runs straight and flat between its
    /// neighbours. `false` when none fits.
    fn repeat(cells: &mut [Cell], tap_strength: u8) -> bool {
        let straight = |cells: &[Cell], at: usize| {
            at >= 1 && at + 1 < cells.len() && {
                let (before, here, after) = (cells[at - 1].at, cells[at].at, cells[at + 1].at);
                before.y == here.y
                    && here.y == after.y
                    && here.x - before.x == after.x - here.x
                    && here.z - before.z == after.z - here.z
            }
        };
        cells[0].strength = tap_strength;
        let (mut strength, mut from, mut at) = (tap_strength, 1, 1);
        while at < cells.len() {
            if strength > MIN_STRENGTH {
                strength -= 1;
                cells[at].strength = strength;
                at += 1;
                continue;
            }
            let Some(socket) = (from..=at).rev().find(|&socket| straight(cells, socket)) else {
                // Leave the rest as dust at the floor strength it would need.
                for cell in &mut cells[at..] {
                    cell.strength = 1;
                }
                return false;
            };
            let (here, after) = (cells[socket].at, cells[socket + 1].at);
            let facing = HORIZONTAL
                .iter()
                .find(|&&(_, dx, dz)| after.x - here.x == dx && after.z - here.z == dz)
                .map(|&(facing, _, _)| facing)
                .unwrap();
            cells[socket].repeater = Some(facing);
            cells[socket + 1].strength = 15;
            strength = 15;
            from = socket + 1;
            at = socket + 2;
        }
        true
    }

    /// Count `wire`'s violating cells and score it as dewey does.
    fn evaluate(&mut self, wire: usize) {
        let w = &self.wires[wire];
        let tap = w.tap();
        let path: CellMap<Anchor, usize> = w
            .cells
            .iter()
            .enumerate()
            .skip(1)
            .take(w.cells.len().saturating_sub(2))
            .flat_map(|(index, cell)| [(cell.at, index), (down(cell.at), index)])
            .collect();
        let mut violations = 0;
        for (index, cell) in w.cells.iter().enumerate().skip(1).take(w.cells.len() - 2) {
            let blocks = [cell.at, down(cell.at)];
            let against_others = blocks
                .iter()
                .any(|&at| self.violates(at, w.net, Some(wire), tap, index == 1, &w.goal))
                || cell
                    .air
                    .is_some_and(|air| self.air_taken(air, Some(wire)) || path.contains_key(&air));
            // A block may touch only the two cells either side of its own.
            let against_itself = blocks.iter().any(|&at| {
                around(at).any(|near| {
                    path.get(&near)
                        .is_some_and(|&other| other + 2 < index || other > index + 2)
                })
            });
            if against_others || against_itself {
                violations += 1;
            }
        }
        let last = w.cells[w.cells.len() - 1];
        if last
            .air
            .is_some_and(|air| self.air_taken(air, Some(wire)) || path.contains_key(&air))
        {
            violations += 1;
        }
        if !w.repeatable {
            violations += 1;
        }
        let length = (w.cells.len() - 1) as u64;
        let w = &mut self.wires[wire];
        w.violations = violations;
        w.score = violations * VIOLATION + length;
    }

    fn evaluate_all(&mut self) -> (u64, u64) {
        let alive: Vec<usize> = (0..self.wires.len())
            .filter(|&wire| self.wires[wire].alive)
            .collect();
        for &wire in &alive {
            self.evaluate(wire);
        }
        alive.iter().fold((0, 0), |(violations, score), &wire| {
            (
                violations + self.wires[wire].violations,
                score + self.wires[wire].score,
            )
        })
    }

    /// Route one sink of `net` and lay the wire.
    fn connect(&mut self, net: usize, goal: Goal, dumb: bool) {
        let taps = self.taps(net);
        let found = if dumb {
            None
        } else {
            self.maze(net, &taps, &goal)
        };
        let mut cells = found.unwrap_or_else(|| self.dumb(&taps, &goal));
        let tap_strength = taps
            .iter()
            .find(|(at, _)| *at == cells[0].at)
            .map_or(15, |&(_, strength)| strength);
        let connected = cells.windows(2).all(|pair| {
            let (a, b) = (pair[0].at, pair[1].at);
            (a.x - b.x).abs() + (a.z - b.z).abs() == 1 && (a.y - b.y).abs() <= 1
        });
        let repeatable = Self::repeat(&mut cells, tap_strength) && connected;
        self.wires.push(Wire {
            net,
            goal,
            cells,
            repeatable,
            score: 0,
            violations: 0,
            alive: true,
        });
        self.insert(self.wires.len() - 1);
    }

    /// Take `wire` out, and every wire leaving from it; their goals come
    /// back to be routed again.
    fn rip(&mut self, wire: usize, goals: &mut Vec<(usize, Goal)>) {
        if !self.wires[wire].alive {
            return;
        }
        let net = self.wires[wire].net;
        let cells: CellSet<Anchor> = self.wires[wire].cells[1..]
            .iter()
            .map(|cell| cell.at)
            .collect();
        self.remove(wire);
        goals.push((net, self.wires[wire].goal.clone()));
        let dependents: Vec<usize> = (0..self.wires.len())
            .filter(|&other| {
                self.wires[other].alive
                    && self.wires[other].net == net
                    && cells.contains(&self.wires[other].tap())
            })
            .collect();
        for dependent in dependents {
            self.rip(dependent, goals);
        }
    }

    /// Route every goal in `goals`, each net's nearest its tree first.
    fn reroute(&mut self, mut goals: Vec<(usize, Goal)>) {
        goals.sort_by_key(|(net, goal)| {
            let nearest = self.sources[*net]
                .iter()
                .map(|(at, _)| manhattan(*at, goal.approach))
                .min()
                .unwrap_or(0);
            (*net, nearest)
        });
        for (net, goal) in goals {
            self.connect(net, goal, false);
        }
    }
}

/// Route every net of `builder` by dewey's method and lay the result in its
/// grid.
pub(super) fn route(builder: &mut Builder) -> Result<(), String> {
    let started = Instant::now();
    let mut goals: Vec<(usize, Goal)> = Vec::new();
    for (net, state) in builder.nets.iter_mut().enumerate() {
        for goal in std::mem::take(&mut state.waiting) {
            goals.push((net, goal));
        }
    }
    let (low, high) = builder
        .grid
        .extent()
        .expect("tiles are placed before routing");
    let span = |a: i32, b: i32| (a - BOUND_MARGIN - 2, b + BOUND_MARGIN + 2);
    let (low_x, high_x) = span(low.x, high.x);
    let (low_z, high_z) = span(low.z, high.z);
    let mut router = Router {
        grid: &builder.grid,
        layer: Layer::default(),
        wires: Vec::new(),
        sources: builder.nets.iter().map(|net| net.taps.clone()).collect(),
        low: Anchor {
            x: low_x,
            y: LOWEST,
            z: low_z,
        },
        high: Anchor {
            x: high_x,
            y: HIGHEST,
            z: high_z,
        },
    };
    // dewey's initial route: every sink a straight city-block run.
    goals.sort_by_key(|(net, goal)| {
        let home = builder.nets[*net]
            .home
            .expect("every read net has a driver");
        (*net, manhattan(home, goal.approach))
    });
    for (net, goal) in goals {
        router.connect(net, goal, true);
    }
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let (mut violations, _) = router.evaluate_all();
    // Natural selection until nothing violates.
    while violations > 0 {
        if started.elapsed() > DEADLINE {
            return Err(format!(
                "dewey: {violations} violations left at the deadline"
            ));
        }
        let alive: Vec<usize> = (0..router.wires.len())
            .filter(|&wire| router.wires[wire].alive)
            .collect();
        let scores = alive.iter().map(|&wire| router.wires[wire].score);
        let (low, high) = scores.fold((u64::MAX, 0), |(low, high), score| {
            (low.min(score), high.max(score))
        });
        // dewey divides by this range; with every score equal it would
        // divide by zero, so the bias is kept at one or more.
        let bias = ((high - low) / 8).max(1);
        let range = bias * 10;
        let mut doomed: Vec<usize> = alive
            .into_iter()
            .filter(|&wire| rng.below(range) < router.wires[wire].score - low + bias)
            .collect();
        doomed.sort_by_key(|&wire| Reverse(router.wires[wire].score));
        let mut goals = Vec::new();
        for wire in doomed {
            router.rip(wire, &mut goals);
        }
        router.reroute(goals);
        violations = router.evaluate_all().0;
    }
    // Then reroute every net in random order while that helps.
    let (_, mut score) = router.evaluate_all();
    loop {
        let mut nets: Vec<usize> = (0..router.sources.len()).collect();
        for index in (1..nets.len()).rev() {
            nets.swap(index, rng.below(index as u64 + 1) as usize);
        }
        let mut changed = false;
        for net in nets {
            if started.elapsed() > DEADLINE {
                break;
            }
            let own: Vec<usize> = (0..router.wires.len())
                .filter(|&wire| router.wires[wire].alive && router.wires[wire].net == net)
                .collect();
            if own.is_empty() {
                continue;
            }
            let kept = router.wires.len();
            let mut goals = Vec::new();
            for &wire in &own {
                router.rip(wire, &mut goals);
            }
            router.reroute(goals);
            let (new_violations, new_score) = router.evaluate_all();
            if new_violations < violations || (new_violations == violations && new_score < score) {
                violations = new_violations;
                score = new_score;
                changed = true;
            } else {
                for wire in kept..router.wires.len() {
                    if router.wires[wire].alive {
                        router.remove(wire);
                    }
                }
                router.wires.truncate(kept);
                for &wire in &own {
                    router.wires[wire].alive = true;
                    router.insert(wire);
                }
                router.evaluate_all();
            }
        }
        if !changed || started.elapsed() > DEADLINE {
            break;
        }
    }
    if violations > 0 {
        return Err(format!(
            "dewey: {violations} violations left after optimising"
        ));
    }
    // Lay the wires, each checked by the same rules the constructive
    // router lays by, in the order they were made (a wire's tap is laid
    // before it).
    let laid: Vec<Wire> = router.wires.into_iter().filter(|wire| wire.alive).collect();
    for (branch, wire) in laid.iter().enumerate() {
        let steps: Vec<route::Step> = wire
            .cells
            .iter()
            .map(|cell| route::Step::new(cell.at, cell.repeater, cell.strength, cell.air))
            .collect();
        let laid = route::check_and_place(
            &mut builder.grid,
            Owner::Net(wire.net),
            &steps,
            &builder.nets[wire.net].segments,
            &wire.goal,
            branch,
        )
        .map_err(|at| {
            format!(
                "dewey: a wire of net {} breaks the gap rule at {at:?}",
                wire.net
            )
        })?;
        let net = &mut builder.nets[wire.net];
        laid.stretch(&mut net.segments, &mut net.stretches);
    }
    Ok(())
}
