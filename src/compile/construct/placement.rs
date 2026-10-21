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

/// Independent annealing runs, and the generator seed they derive from.
const RUNS: usize = 8;
const SEED: u64 = 0x2545_F491_4F6C_DD1D;
/// Moves tried per element at each temperature, and how fast it falls.
const MOVES: usize = 50;
const COOLING: f64 = 0.95;
/// Nets read by more elements than this weigh less: a wide net's tree
/// spans the layout whatever the placement does.
const WIDE: usize = 8;
/// Estimated game ticks for a gate, and for a connection per slot it
/// crosses (a slot is about 18 cells: a repeater and change).
const GATE_TICKS: f64 = 2.0;
const SLOT_TICKS: f64 = 2.4;
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
}

impl Timing {
    /// Each connection's criticality once the elements sit at `slots`.
    pub fn criticality_at(&self, slots: &[(i32, i32)]) -> Vec<f64> {
        self.timed(slots.len(), |edge| {
            let (from, to) = self.edges[edge];
            let (a, b) = (slots[from], slots[to]);
            GATE_TICKS + SLOT_TICKS * ((a.0 - b.0).abs() + (a.1 - b.1).abs()) as f64
        })
        .0
    }

    /// Each connection's criticality in `0..=1` for a placement whose
    /// connections take `delay` ticks -- one minus its slack over the
    /// critical delay -- and that critical delay.
    fn timed(&self, count: usize, delay: impl Fn(usize) -> f64) -> (Vec<f64>, f64) {
        // Arrival at each element's output, in topological order; a
        // register's output starts afresh.
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
        let mut ready = vec![0.0f64; count];
        let mut reached = vec![0.0f64; count];
        for &element in &order {
            if !self.registered[element] {
                ready[element] = reached[element];
            }
            for &edge in &out[element] {
                let to = self.edges[edge].1;
                reached[to] = reached[to].max(ready[element] + delay(edge));
            }
        }
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

/// Slot coordinates for `count` elements joined by `nets` (each a list of
/// element indices). `order` lists the elements in the order to seed the
/// lattice with, so related elements start close.
///
/// Where annealing lands depends on its dice, so it runs [`RUNS`] times in
/// parallel (one after another where there are no threads, as on wasm);
/// the placements come back best first -- shortest
/// estimated critical delay, then least wire -- for the caller to fall back
/// on when one will not route. A fixed run count, not the core count, keeps
/// the result the same on every machine.
pub fn anneal(
    count: usize,
    nets: &[Vec<usize>],
    timing: &Timing,
    order: &[usize],
) -> Vec<Vec<(i32, i32)>> {
    let mut runs: Vec<Run> = (0..RUNS as u64)
        .into_par_iter()
        .map(|run| {
            anneal_once(
                count,
                nets,
                timing,
                order,
                SEED ^ run.wrapping_mul(0x9E37_79B9_7F4A_7C15),
            )
        })
        .collect();
    runs.sort_by(|a, b| (a.1, a.2).partial_cmp(&(b.1, b.2)).expect("finite costs"));
    runs.into_iter().map(|(slots, _, _)| slots).collect()
}

/// One annealing run's slots, their estimated critical delay and their
/// wire.
type Run = (Vec<(i32, i32)>, f64, f64);

/// One annealing run from `seed`.
fn anneal_once(
    count: usize,
    nets: &[Vec<usize>],
    timing: &Timing,
    order: &[usize],
    seed: u64,
) -> Run {
    let side = ((count as f64 * 1.15).sqrt().ceil() as usize).max(1);
    let mut slot_of = vec![0usize; count];
    let mut held: Vec<Option<usize>> = vec![None; side * side];
    // Seed in a serpentine so consecutive elements are neighbours.
    for (rank, &element) in order.iter().enumerate() {
        let row = rank / side;
        let column = if row.is_multiple_of(2) {
            rank % side
        } else {
            side - 1 - rank % side
        };
        let slot = row * side + column;
        slot_of[element] = slot;
        held[slot] = Some(element);
    }
    let mut nets_of: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (net, members) in nets.iter().enumerate() {
        for &element in members {
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
    let span = |slot_of: &[usize], net: usize| -> f64 {
        let (mut low_x, mut low_z, mut high_x, mut high_z) = (usize::MAX, usize::MAX, 0, 0);
        for &element in &nets[net] {
            let (x, z) = (slot_of[element] % side, slot_of[element] / side);
            low_x = low_x.min(x);
            high_x = high_x.max(x);
            low_z = low_z.min(z);
            high_z = high_z.max(z);
        }
        weight(net) * (high_x - low_x + high_z - low_z) as f64
    };
    let delay = |slot_of: &[usize], edge: usize| -> f64 {
        let (from, to) = timing.edges[edge];
        let (a, b) = (slot_of[from], slot_of[to]);
        let distance = (a % side).abs_diff(b % side) + (a / side).abs_diff(b / side);
        GATE_TICKS + SLOT_TICKS * distance as f64
    };
    let mut rng = Rng(seed);
    let mut wire: f64 = (0..nets.len()).map(|net| span(&slot_of, net)).sum();
    let mut temperature = (wire / nets.len().max(1) as f64).max(1.0);
    let mut window = side;
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
        for _ in 0..MOVES * count {
            let element = rng.below(count);
            let from = slot_of[element];
            let (x, z) = ((from % side) as i64, (from / side) as i64);
            let reach = window as i64;
            let to_x = (x + rng.below(2 * window + 1) as i64 - reach).clamp(0, side as i64 - 1);
            let to_z = (z + rng.below(2 * window + 1) as i64 - reach).clamp(0, side as i64 - 1);
            let to = to_z as usize * side + to_x as usize;
            if to == from {
                continue;
            }
            let other = held[to];
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
            slot_of[element] = to;
            if let Some(other) = other {
                slot_of[other] = from;
            }
            let after = cost(&slot_of);
            let delta = (after.0 - before.0) * wire_scale + (after.1 - before.1) * timing_scale;
            if delta <= 0.0 || rng.unit() < (-delta / scaled_temperature).exp() {
                wire += after.0 - before.0;
                held[to] = Some(element);
                held[from] = other;
            } else {
                slot_of[element] = from;
                if let Some(other) = other {
                    slot_of[other] = to;
                }
            }
        }
        temperature *= COOLING;
        window = (window * 9 / 10).max(2);
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
    for _ in 0..QUENCH_MOVES * count {
        if count < 2 || timing.edges.is_empty() {
            break;
        }
        let on_path: Vec<usize> = (0..timing.edges.len())
            .filter(|&edge| criticality[edge] > 0.999)
            .collect();
        let edge = on_path[rng.below(on_path.len())];
        let element = if rng.below(2) == 0 {
            timing.edges[edge].0
        } else {
            timing.edges[edge].1
        };
        let from = slot_of[element];
        let (x, z) = ((from % side) as i64, (from / side) as i64);
        let to_x = (x + rng.below(5) as i64 - 2).clamp(0, side as i64 - 1);
        let to_z = (z + rng.below(5) as i64 - 2).clamp(0, side as i64 - 1);
        let to = to_z as usize * side + to_x as usize;
        if to == from {
            continue;
        }
        let other = held[to];
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
        let (new_critical, new_near, new_criticality) = score(&slot_of);
        let better = new_critical < critical - 1e-9
            || (new_critical <= critical + 1e-9 && new_near < near - 1e-9);
        if better && wire + after - before <= budget {
            held[to] = Some(element);
            held[from] = other;
            wire += after - before;
            (critical, near, criticality) = (new_critical, new_near, new_criticality);
        } else {
            slot_of[element] = from;
            if let Some(other) = other {
                slot_of[other] = to;
            }
        }
    }
    let slots = slot_of
        .iter()
        .map(|&slot| ((slot % side) as i32, (slot / side) as i32))
        .collect();
    (slots, critical, wire)
}
