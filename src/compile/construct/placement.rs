//! Where each element goes, decided before anything is placed: elements on
//! a lattice of equal slots, arranged by simulated annealing so the nets
//! between them are short.
//!
//! Placing one element at a time beside what it connects to is fast but
//! myopic: an element read from two places far apart lands near one of them,
//! and a ROM's minterms, all reading the same address lines, crowd one spot
//! until the rest are pushed hundreds of cells away. Annealing sees every
//! net at once. It works on slot indices only -- no blocks -- so it costs
//! milliseconds; the slots are spaced so any tile fits in one.
//!
//! Short nets alone leave delay to chance: a ROM's critical path crosses the
//! layout while wide, slack-rich nets are kept tight. So the cost also
//! weighs each connection's estimated delay by how critical it is
//! (timing-driven placement as in VPR: Marquardt, Betz and Rose, 2000),
//! re-timed at every temperature.

use std::sync::OnceLock;

use rayon::prelude::*;

/// The xorshift generator the router uses too: deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How much lattice per slot of element area there is when blocks share it,
/// and what one overlapped slot costs, in slots of span, at the start.
const MIXED_SLACK: f64 = 1.6;
const OVERLAP: f64 = 4.0;
/// Independent annealing runs, and the generator seed they derive from.
const RUNS: usize = 8;
const SEED: u64 = 0x2545_F491_4F6C_DD1D;
/// Moves tried per element at each temperature, and how fast it falls.
const MOVES: usize = 50;
/// Moves per element per temperature for a rough placement.
const ROUGH_MOVES: usize = 10;
const COOLING: f64 = 0.95;
/// Nets read by more elements than this weigh less: a wide net's tree
/// spans the layout whatever the placement does.
const WIDE: usize = 8;
/// Estimated game ticks for a gate, and for a connection per slot it
/// crosses (a slot is about 18 cells: a repeater and change).
pub const GATE_TICKS: f64 = 2.0;
pub const SLOT_TICKS: f64 = 2.4;
/// How sharply criticality singles out the near-critical connections, and
/// how the cost splits between delay and wire length.
const SHARPNESS: i32 = 8;
const TIMING_SHARE: f64 = 0.5;
/// Greedy moves tried per element on the longest path after annealing,
/// and how much more wire they may cost in all.
const QUENCH_MOVES: usize = 10;
const QUENCH_WIRE: f64 = 0.05;
/// Connections weighing less than this are left out of a temperature's
/// moves: a wide net's slack-rich readers would cost every move of its
/// driver hundreds of terms for nothing.
const NEGLIGIBLE: f64 = 0.01;

/// What timing sees of the elements: every driver-to-reader connection,
/// and which elements are registers (paths start at their outputs and end
/// at their inputs).
pub struct Timing {
    pub edges: Vec<(usize, usize)>,
    pub registered: Vec<bool>,
    /// Ticks an element adds on top of a gate's before its output moves: a
    /// placed block's own critical delay.
    pub intrinsic: Vec<f64>,
    /// Ticks after the start an element's output moves at the earliest: a
    /// block's input port, by when its parent expects the signal.
    pub arrival: Vec<f64>,
    /// Where each connection leaves its driver and reaches its reader,
    /// from each one's middle, in slots: a block's port, not its middle.
    pub ends: Vec<(Offset, Offset)>,
    /// The graph's shape, worked out once: placement re-times it at every
    /// move but never changes it.
    topology: OnceLock<Topology>,
}

/// The elements in topological order (a register's output starting a
/// path afresh), and the connections leaving each.
#[derive(Clone)]
struct Topology {
    count: usize,
    order: Vec<usize>,
    out: Vec<Vec<usize>>,
}

/// A point relative to an element's middle, in slots.
pub type Offset = (f64, f64);

/// An edge of the lattice. Slot rows count northward, so row zero is the
/// south edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    West,
    East,
    North,
    South,
}

/// Where an element must keep to: a port on an edge anywhere along it; a
/// port the caller put at a given cell, or a point a port leans toward, a
/// fixed point in slot units off the lattice.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pin {
    Free,
    Edge(Side),
    Point(f64, f64),
}

impl Clone for Timing {
    fn clone(&self) -> Timing {
        Timing::new(self.edges.clone(), self.registered.clone(), self.intrinsic.clone(), self.arrival.clone(), self.ends.clone())
    }
}

impl Timing {
    pub fn new(
        edges: Vec<(usize, usize)>,
        registered: Vec<bool>,
        intrinsic: Vec<f64>,
        arrival: Vec<f64>,
        ends: Vec<(Offset, Offset)>,
    ) -> Timing {
        Timing {
            edges,
            registered,
            intrinsic,
            arrival,
            ends,
            topology: OnceLock::new(),
        }
    }

    /// The graph's topology for `count` elements: worked out once, and
    /// again only if elements were added since.
    fn topology(&self, count: usize) -> std::borrow::Cow<'_, Topology> {
        let fresh = || {
            let mut fanin = vec![0usize; count];
            let mut out: Vec<Vec<usize>> = vec![Vec::new(); count];
            for (edge, &(from, to)) in self.edges.iter().enumerate() {
                out[from].push(edge);
                if !self.registered[to] {
                    fanin[to] += 1;
                }
            }
            let mut order: Vec<usize> = (0..count).filter(|&element| fanin[element] == 0).collect();
            let mut next = 0;
            while next < order.len() {
                for &edge in &out[order[next]] {
                    let to = self.edges[edge].1;
                    if !self.registered[to] {
                        fanin[to] -= 1;
                        if fanin[to] == 0 {
                            order.push(to);
                        }
                    }
                }
                next += 1;
            }
            Topology { count, order, out }
        };
        let cached = self.topology.get_or_init(fresh);
        if cached.count == count {
            std::borrow::Cow::Borrowed(cached)
        } else {
            std::borrow::Cow::Owned(fresh())
        }
    }

    /// Ticks connection `edge` takes once the elements' middles sit at
    /// `centres`.
    pub fn delay_at(&self, centres: &[(f64, f64)], edge: usize) -> f64 {
        let (from, to) = self.edges[edge];
        let ((fx, fz), (tx, tz)) = self.ends[edge];
        let (a, b) = (centres[from], centres[to]);
        let distance = (a.0 + fx - b.0 - tx).abs() + (a.1 + fz - b.1 - tz).abs();
        GATE_TICKS + self.intrinsic[from] + SLOT_TICKS * distance
    }

    /// Each connection's criticality once the elements' middles sit at
    /// `centres`.
    pub fn criticality_at(&self, centres: &[(f64, f64)]) -> Vec<f64> {
        self.timed(centres.len(), |edge| self.delay_at(centres, edge)).0
    }

    /// The estimated critical delay once the elements' middles sit at
    /// `centres`.
    pub fn critical_at(&self, centres: &[(f64, f64)]) -> f64 {
        self.timed(centres.len(), |edge| self.delay_at(centres, edge)).1
    }

    /// When each element's output moves, for a placement whose connections
    /// take `delay` ticks.
    pub fn ready(&self, count: usize, delay: impl Fn(usize) -> f64) -> Vec<f64> {
        self.forward(count, &delay).1
    }

    /// The graph's topology, when each element's output moves, and when its
    /// last input arrives.
    fn forward(&self, count: usize, delay: &impl Fn(usize) -> f64) -> (std::borrow::Cow<'_, Topology>, Vec<f64>, Vec<f64>) {
        // Arrival at each element's output, in topological order; a
        // register's output starts afresh.
        let topology = self.topology(count);
        let (order, out) = (&topology.order, &topology.out);
        let mut ready = vec![0.0f64; count];
        let mut reached = self.arrival.clone();
        for &element in order {
            if !self.registered[element] {
                ready[element] = reached[element];
            }
            for &edge in &out[element] {
                let to = self.edges[edge].1;
                reached[to] = reached[to].max(ready[element] + delay(edge));
            }
        }
        (topology, ready, reached)
    }

    /// The critical delay alone, for a placement whose connections take
    /// `delay` ticks: what [`Timing::timed`] gives, for a forward pass only.
    fn critical(&self, count: usize, delay: impl Fn(usize) -> f64) -> f64 {
        let (_, _, reached) = self.forward(count, &delay);
        reached.iter().copied().fold(0.0, f64::max).max(1.0)
    }

    /// Each connection's criticality in `0..=1` for a placement whose
    /// connections take `delay` ticks -- one minus its slack over the
    /// critical delay -- and that critical delay.
    fn timed(&self, count: usize, delay: impl Fn(usize) -> f64) -> (Vec<f64>, f64) {
        let (topology, ready, reached) = self.forward(count, &delay);
        let (order, out) = (&topology.order, &topology.out);
        let critical = reached.iter().copied().fold(0.0, f64::max).max(1.0);
        // Required time at each element's output, walking back.
        let mut needed = vec![critical; count];
        for &element in order.iter().rev() {
            for &edge in &out[element] {
                let to = self.edges[edge].1;
                let by = if self.registered[to] {
                    critical
                } else {
                    needed[to]
                };
                needed[element] = needed[element].min(by - delay(edge));
            }
        }
        let criticality = self
            .edges
            .iter()
            .enumerate()
            .map(|(edge, &(from, to))| {
                let by = if self.registered[to] {
                    critical
                } else {
                    needed[to]
                };
                let slack = by - ready[from] - delay(edge);
                (1.0 - slack / critical).clamp(0.0, 1.0)
            })
            .collect();
        (criticality, critical)
    }
}

/// Slot coordinates for each element of `problem`.
///
/// Where annealing lands depends on its dice, so it runs [`RUNS`] times in
/// parallel (one after another where there are no threads, as on wasm);
/// the placements come back best first -- shortest estimated critical
/// delay, then least wire -- with that delay, for the caller to fall back on
/// when one will not route. A fixed run count, not the core count, keeps the
/// result the same on every machine.
pub fn anneal(problem: &Problem) -> Vec<(Vec<(i32, i32)>, f64)> {
    let mut runs: Vec<Run> = (0..RUNS as u64)
        .into_par_iter()
        .map(|run| anneal_once(problem, SEED ^ run.wrapping_mul(0x9E37_79B9_7F4A_7C15)))
        .collect();
    runs.sort_by(|a, b| (a.1, a.2).partial_cmp(&(b.1, b.2)).expect("finite costs"));
    runs.into_iter().map(|(slots, critical, _)| (slots, critical)).collect()
}

/// One annealing run's slots, their estimated critical delay and their
/// wire.
type Run = (Vec<(i32, i32)>, f64, f64);

/// What to place: `count` elements joined by `nets` (each a list of element
/// indices, with where on each the net attaches), timed by `timing`, kept
/// to `pins`, each spanning `sizes` slots, on a lattice of about `shape`
/// columns and rows if given, else a square. `order` lists the elements in
/// the order to seed the lattice with, so related elements start close.
/// A `rough` placement tries fewer moves: enough to plan by.
pub struct Problem<'a> {
    pub count: usize,
    pub nets: &'a [Vec<(usize, Offset)>],
    pub timing: &'a Timing,
    pub pins: &'a [Pin],
    pub sizes: &'a [(usize, usize)],
    pub shape: Option<(usize, usize)>,
    pub order: &'a [usize],
    pub rough: bool,
}

/// One annealing run from `seed`.
fn anneal_once(problem: &Problem, seed: u64) -> Run {
    let &Problem {
        count,
        nets,
        timing,
        pins,
        sizes,
        shape,
        order,
        rough,
    } = problem;
    let moves = if rough { ROUGH_MOVES } else { MOVES };
    let pinned = |edge: Side| {
        pins.iter()
            .filter(|&&pin| pin == Pin::Edge(edge))
            .count()
    };
    let off_corners = |count: usize| if count == 0 { 0 } else { count + 2 };
    // Blocks span several slots and must end up apart, so they get room to
    // move around each other.
    let point = |element: usize| match pins[element] {
        Pin::Point(x, z) => Some((x, z)),
        _ => None,
    };
    let on_lattice = (0..count).filter(|&element| point(element).is_none()).count();
    let area: usize = (0..count).filter(|&element| point(element).is_none()).map(|element| sizes[element].0 * sizes[element].1).sum();
    let mixed = area > on_lattice;
    let slack = if mixed { MIXED_SLACK } else { 1.15 };
    // The lattice is square unless the caller asks for a shape, which is
    // scaled up to hold everything. West and east ports run up whole
    // columns; north and south ones keep off the corners.
    let room = area as f64 * slack;
    let (cols, rows) = match shape {
        None => {
            let side = room.sqrt().ceil();
            (side, side)
        }
        Some((w, h)) => {
            let scale = (room / (w * h) as f64).sqrt().max(1.0);
            ((w as f64 * scale).ceil(), (h as f64 * scale).ceil())
        }
    };
    let (mut cols, mut rows) = (cols as usize, rows as usize);
    if shape.is_none() {
        let side = cols
            .max(pinned(Side::West))
            .max(pinned(Side::East))
            .max(off_corners(pinned(Side::North)))
            .max(off_corners(pinned(Side::South)))
            .max(sizes.iter().map(|&(w, h)| w.max(h)).max().unwrap_or(1))
            .max(1);
        (cols, rows) = (side, side);
    }
    cols = cols
        .max(off_corners(pinned(Side::North)))
        .max(off_corners(pinned(Side::South)))
        .max(sizes.iter().map(|&(w, _)| w).max().unwrap_or(1))
        .max(1);
    rows = rows
        .max(pinned(Side::West))
        .max(pinned(Side::East))
        .max(sizes.iter().map(|&(_, h)| h).max().unwrap_or(1))
        .max(1);
    // Whether `element` may have its corner at `slot`: inside the lattice,
    // and on its edge if pinned.
    let allowed = |element: usize, slot: usize| {
        let (w, h) = sizes[element];
        let (x, z) = (slot % cols, slot / cols);
        x + w <= cols
            && z + h <= rows
            && match pins[element] {
                Pin::Free => true,
                Pin::Edge(Side::West) => x == 0,
                Pin::Edge(Side::East) => x + w == cols,
                Pin::Edge(Side::South) => z == 0,
                Pin::Edge(Side::North) => z + h == rows,
                Pin::Point(..) => false,
            }
    };
    // The slots an element covers with its corner at `slot`.
    let footprint = move |element: usize, slot: usize| {
        let (w, h) = sizes[element];
        let (x, z) = (slot % cols, slot / cols);
        (z..z + h).flat_map(move |row| (x..x + w).map(move |column| row * cols + column))
    };
    let mut slot_of = vec![0usize; count];
    let mut held: Vec<Option<usize>> = vec![None; cols * rows];
    let mut cover: Vec<u16> = vec![0; cols * rows];
    // Pinned elements first, along their edges in order; then the rest in
    // a serpentine through the slots left, so consecutive elements are
    // neighbours.
    let serpentine = (0..cols * rows).map(|rank| {
        let row = rank / cols;
        let column = if row.is_multiple_of(2) {
            rank % cols
        } else {
            cols - 1 - rank % cols
        };
        row * cols + column
    });
    for edge in [Side::West, Side::East, Side::South, Side::North] {
        for (rank, &element) in order.iter().filter(|&&element| pins[element] == Pin::Edge(edge)).enumerate() {
            let (column, row) = match edge {
                Side::West => (0, rank),
                Side::East => (cols - 1, rank),
                Side::South => (1 + rank, 0),
                Side::North => (1 + rank, rows - 1),
            };
            let slot = row * cols + column;
            slot_of[element] = slot;
            held[slot] = Some(element);
            for covered in footprint(element, slot) {
                cover[covered] += 1;
            }
        }
    }
    let serpentine: Vec<usize> = serpentine.collect();
    for &element in order.iter().filter(|&&element| pins[element] == Pin::Free) {
        let fits = |slot: usize| allowed(element, slot) && footprint(element, slot).all(|covered| cover[covered] == 0);
        let slot = serpentine
            .iter()
            .copied()
            .find(|&slot| fits(slot))
            .or_else(|| serpentine.iter().copied().find(|&slot| allowed(element, slot) && held[slot].is_none()))
            .expect("the lattice holds every element");
        slot_of[element] = slot;
        held[slot] = Some(element);
        for covered in footprint(element, slot) {
            cover[covered] += 1;
        }
    }
    let mut nets_of: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (net, members) in nets.iter().enumerate() {
        for &(element, _) in members {
            nets_of[element].push(net);
        }
    }
    let weight = |net: usize| {
        let size = nets[net].len();
        if size <= WIDE {
            1.0
        } else {
            WIDE as f64 / size as f64
        }
    };
    // Where an element's middle is, in slots.
    let centre = |slot_of: &[usize], element: usize| {
        if let Some(at) = point(element) {
            return at;
        }
        let (w, h) = sizes[element];
        let slot = slot_of[element];
        (
            (slot % cols) as f64 + (w - 1) as f64 / 2.0,
            (slot / cols) as f64 + (h - 1) as f64 / 2.0,
        )
    };
    let span = |slot_of: &[usize], net: usize| -> f64 {
        let (mut low_x, mut low_z, mut high_x, mut high_z) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(element, (dx, dz)) in &nets[net] {
            let (x, z) = centre(slot_of, element);
            let (x, z) = (x + dx, z + dz);
            low_x = low_x.min(x);
            high_x = high_x.max(x);
            low_z = low_z.min(z);
            high_z = high_z.max(z);
        }
        weight(net) * (high_x - low_x + high_z - low_z)
    };
    let delay = |slot_of: &[usize], edge: usize| -> f64 {
        let (from, to) = timing.edges[edge];
        let ((fx, fz), (tx, tz)) = timing.ends[edge];
        let (a, b) = (centre(slot_of, from), centre(slot_of, to));
        let distance = (a.0 + fx - b.0 - tx).abs() + (a.1 + fz - b.1 - tz).abs();
        GATE_TICKS + timing.intrinsic[from] + SLOT_TICKS * distance
    };
    // How many slots more than one element covers, among `slots`.
    let overlap = |cover: &[u16], slots: &[usize]| -> f64 {
        slots.iter().map(|&slot| f64::from(cover[slot].saturating_sub(1))).sum()
    };
    let mut region: Vec<usize> = Vec::new();
    let mut rng = Rng(seed);
    let mut wire: f64 = (0..nets.len()).map(|net| span(&slot_of, net)).sum();
    let mut temperature = (wire / nets.len().max(1) as f64).max(1.0);
    let mut window = cols.max(rows);
    let mut touched: Vec<usize> = Vec::new();
    let mut moved_edges: Vec<usize> = Vec::new();
    while temperature > 0.05 && count > 1 {
        // Re-time, and weigh wire and delay by their totals so each counts
        // for its share whatever its scale.
        let (criticality, _) = timing.timed(count, |edge| delay(&slot_of, edge));
        let weight_of: Vec<f64> = criticality.iter().map(|c| c.powi(SHARPNESS)).collect();
        let timed: f64 = (0..timing.edges.len())
            .map(|edge| weight_of[edge] * delay(&slot_of, edge))
            .sum();
        let mut edges_of: Vec<Vec<usize>> = vec![Vec::new(); count];
        for (edge, &(from, to)) in timing.edges.iter().enumerate() {
            if weight_of[edge] >= NEGLIGIBLE {
                edges_of[from].push(edge);
                if to != from {
                    edges_of[to].push(edge);
                }
            }
        }
        let wire_scale = (1.0 - TIMING_SHARE) / wire.max(1.0);
        let timing_scale = if timed > 0.0 {
            TIMING_SHARE / timed
        } else {
            0.0
        };
        let scaled_temperature = temperature * wire_scale;
        // An overlapped slot costs as much wire as a few slots of span, and
        // more as the lattice cools, so blocks end up apart.
        let penalty = OVERLAP * (1.0 + 1.0 / temperature.max(1e-3)) * wire_scale;
        for _ in 0..moves * count {
            let element = rng.below(count);
            let from = slot_of[element];
            let (w, h) = sizes[element];
            let (x, z) = ((from % cols) as i64, (from / cols) as i64);
            let reach = window as i64;
            let to_x = (x + rng.below(2 * window + 1) as i64 - reach).clamp(0, (cols - w) as i64);
            let to_z = (z + rng.below(2 * window + 1) as i64 - reach).clamp(0, (rows - h) as i64);
            let to = to_z as usize * cols + to_x as usize;
            if to == from {
                continue;
            }
            let other = held[to];
            if !allowed(element, to)
                || other.is_some_and(|other| sizes[other] != sizes[element] || !allowed(other, from))
            {
                continue;
            }
            if mixed {
                region.clear();
                region.extend(footprint(element, from));
                region.extend(footprint(element, to));
                region.sort_unstable();
                region.dedup();
            }
            touched.clear();
            touched.extend(&nets_of[element]);
            if let Some(other) = other {
                touched.extend(&nets_of[other]);
            }
            touched.sort_unstable();
            touched.dedup();
            moved_edges.clear();
            moved_edges.extend(&edges_of[element]);
            if let Some(other) = other {
                moved_edges.extend(&edges_of[other]);
            }
            moved_edges.sort_unstable();
            moved_edges.dedup();
            let cost = |slot_of: &[usize]| -> (f64, f64) {
                (
                    touched.iter().map(|&net| span(slot_of, net)).sum(),
                    moved_edges
                        .iter()
                        .map(|&edge| weight_of[edge] * delay(slot_of, edge))
                        .sum(),
                )
            };
            let before = cost(&slot_of);
            let overlapped = if mixed { overlap(&cover, &region) } else { 0.0 };
            slot_of[element] = to;
            if let Some(other) = other {
                slot_of[other] = from;
            }
            if mixed && other.is_none() {
                for covered in footprint(element, from) {
                    cover[covered] -= 1;
                }
                for covered in footprint(element, to) {
                    cover[covered] += 1;
                }
            }
            let after = cost(&slot_of);
            let overlapping = if mixed { overlap(&cover, &region) } else { 0.0 };
            let delta = (after.0 - before.0) * wire_scale
                + (after.1 - before.1) * timing_scale
                + (overlapping - overlapped) * penalty;
            if delta <= 0.0 || rng.unit() < (-delta / scaled_temperature).exp() {
                wire += after.0 - before.0;
                held[to] = Some(element);
                held[from] = other;
            } else {
                slot_of[element] = from;
                if let Some(other) = other {
                    slot_of[other] = to;
                }
                if mixed && other.is_none() {
                    for covered in footprint(element, to) {
                        cover[covered] -= 1;
                    }
                    for covered in footprint(element, from) {
                        cover[covered] += 1;
                    }
                }
            }
        }
        temperature *= COOLING;
        window = (window * 9 / 10).max(2);
    }

    // Then move whatever still overlaps to the nearest corner where it
    // covers nothing, biggest first.
    if mixed {
        let mut biggest: Vec<usize> = (0..count).collect();
        biggest.sort_by_key(|&element| std::cmp::Reverse(sizes[element].0 * sizes[element].1));
        for element in biggest {
            let from = slot_of[element];
            if point(element).is_some() || footprint(element, from).all(|covered| cover[covered] == 1) {
                continue;
            }
            for covered in footprint(element, from) {
                cover[covered] -= 1;
            }
            held[from] = None;
            let (x, z) = ((from % cols) as i64, (from / cols) as i64);
            let to = (0..cols.max(rows) as i64)
                .flat_map(|ring| {
                    (-ring..=ring).flat_map(move |dz| (-ring..=ring).filter(move |dx| dx.abs().max(dz.abs()) == ring).map(move |dx| (dx, dz)))
                })
                .map(|(dx, dz)| (x + dx, z + dz))
                .filter(|&(x, z)| x >= 0 && z >= 0 && x < cols as i64 && z < rows as i64)
                .map(|(x, z)| z as usize * cols + x as usize)
                .find(|&slot| {
                    allowed(element, slot) && held[slot].is_none() && footprint(element, slot).all(|covered| cover[covered] == 0)
                })
                .unwrap_or(from);
            slot_of[element] = to;
            held[to] = Some(element);
            for covered in footprint(element, to) {
                cover[covered] += 1;
            }
        }
        wire = (0..nets.len()).map(|net| span(&slot_of, net)).sum();
    }

    // Annealing shortens the critical-weighted sum of delays, which only
    // approximates the longest path, and lands wherever the dice say. So
    // finish greedily on the longest path itself: move an element on it
    // one or two slots, keeping the move only if the critical delay falls
    // (or holds while the near-critical paths shorten), for a little wire.
    let budget = wire * (1.0 + QUENCH_WIRE);
    let score = |slot_of: &[usize]| {
        let (criticality, critical) = timing.timed(count, |edge| delay(slot_of, edge));
        let near: f64 = criticality
            .iter()
            .enumerate()
            .map(|(edge, c)| c.powi(SHARPNESS) * delay(slot_of, edge))
            .sum();
        (critical, near, criticality)
    };
    let (mut critical, mut near, mut criticality) = score(&slot_of);
    let on = |criticality: &[f64]| -> Vec<usize> {
        (0..timing.edges.len()).filter(|&edge| criticality[edge] > 0.999).collect()
    };
    let mut on_path = on(&criticality);
    for _ in 0..QUENCH_MOVES * count {
        if count < 2 || timing.edges.is_empty() {
            break;
        }
        let edge = on_path[rng.below(on_path.len())];
        let element = if rng.below(2) == 0 {
            timing.edges[edge].0
        } else {
            timing.edges[edge].1
        };
        let from = slot_of[element];
        let (w, h) = sizes[element];
        let (x, z) = ((from % cols) as i64, (from / cols) as i64);
        let to_x = (x + rng.below(5) as i64 - 2).clamp(0, (cols - w) as i64);
        let to_z = (z + rng.below(5) as i64 - 2).clamp(0, (rows - h) as i64);
        let to = to_z as usize * cols + to_x as usize;
        if to == from {
            continue;
        }
        let other = held[to];
        if !allowed(element, to)
            || other.is_some_and(|other| sizes[other] != sizes[element] || !allowed(other, from))
        {
            continue;
        }
        // A lone move may not land on anything.
        if mixed && other.is_none() {
            for covered in footprint(element, from) {
                cover[covered] -= 1;
            }
            let clear = footprint(element, to).all(|covered| cover[covered] == 0);
            for covered in footprint(element, from) {
                cover[covered] += 1;
            }
            if !clear {
                continue;
            }
        }
        touched.clear();
        touched.extend(&nets_of[element]);
        if let Some(other) = other {
            touched.extend(&nets_of[other]);
        }
        touched.sort_unstable();
        touched.dedup();
        let before: f64 = touched.iter().map(|&net| span(&slot_of, net)).sum();
        slot_of[element] = to;
        if let Some(other) = other {
            slot_of[other] = from;
        }
        let after: f64 = touched.iter().map(|&net| span(&slot_of, net)).sum();
        // Cheapest test first: the wire allowed, then the longest path,
        // then everything near it.
        let affordable = wire + after - before <= budget;
        let no_longer = affordable && timing.critical(count, |edge| delay(&slot_of, edge)) <= critical + 1e-9;
        let scored = no_longer.then(|| score(&slot_of)).filter(|&(new_critical, new_near, _)| {
            new_critical < critical - 1e-9 || (new_critical <= critical + 1e-9 && new_near < near - 1e-9)
        });
        if let Some((new_critical, new_near, new_criticality)) = scored {
            if mixed && other.is_none() {
                for covered in footprint(element, from) {
                    cover[covered] -= 1;
                }
                for covered in footprint(element, to) {
                    cover[covered] += 1;
                }
            }
            held[to] = Some(element);
            held[from] = other;
            wire += after - before;
            (critical, near, criticality) = (new_critical, new_near, new_criticality);
            on_path = on(&criticality);
        } else {
            slot_of[element] = from;
            if let Some(other) = other {
                slot_of[other] = to;
            }
        }
    }
    let slots = slot_of
        .iter()
        .enumerate()
        .map(|(element, &slot)| match point(element) {
            Some((x, z)) => (x.round() as i32, z.round() as i32),
            None => ((slot % cols) as i32, (slot / cols) as i32),
        })
        .collect();
    (slots, critical, wire)
}
