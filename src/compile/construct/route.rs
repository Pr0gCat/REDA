//! One branch of a net: from any tappable cell of the net's frozen tree to
//! one sink stub.
//!
//! The search moves one dust cell at a time (flat, or up or down one level)
//! and places no repeaters: they go in afterwards, on cells of the found
//! path that run straight and flat between their neighbours (see
//! [`repeaters`]). The search only keeps the path repeatable: each state
//! carries the strength the path would have with a repeater on every such
//! cell, and a move that would leave it dead is refused. The search sees
//! only the frozen grid; how the new path sits against itself is checked
//! when it is committed, and a cell that fails there is forbidden and the
//! search rerun.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::grid::{CellMap, CellSet, Grid, Owner};
use crate::compile::geometry::Anchor;
use crate::compile::{dust, repeater, stone};
use crate::redstone::world::block::Facing;

/// The lowest and highest level a route cell may stand on (its floor is one
/// below).
pub const LOWEST: i32 = 1;
pub const HIGHEST: i32 = 17;
/// States a single search may expand before it gives up: a little room
/// per cell of distance, up to a cap. A walled-in search otherwise floods
/// the whole open space before it admits defeat, which is most of what
/// routing a large circuit used to cost.
const BUDGET_PER_CELL: usize = 2_000;
const BUDGET: usize = 600_000;
/// How many times a branch is searched again after its commit refused a cell.
const RETRIES: usize = 12;
/// Weight on the distance estimate: a found path, fast, beats the shortest.
const GREED: u64 = 3;
const STEP: u64 = 1;
const TRESPASS: u64 = 12;
const STAIR: u64 = 2;
/// Levels below this are where tiles stand; a route crossing them costs
/// more, so long runs climb to the open levels above and leave the ground
/// for the connections that have to be made there.
pub const OPEN: i32 = 5;
const CROWDED: u64 = 3;
/// Above the tiles, each net has a home level where travel is cheapest and
/// pays this much more elsewhere. Without it every long run settles on the
/// lowest open level and together they roof the ground over: an output
/// placed later climbs its ramp into that roof and cannot get out.
const AWAY: u64 = 2;

/// The level `owner` prefers to travel on, `lift` levels above the usual.
fn home_level(owner: Owner, lift: i32) -> i32 {
    let levels = (HIGHEST - OPEN) / 3 + 1;
    let index = match owner {
        Owner::Net(net) | Owner::Gate(net) => net as i32,
    };
    lift + OPEN + 3 * (index.wrapping_mul(7).rem_euclid(levels))
}

/// What one unit of travel costs at level `y` for a net living on `home`.
fn level_cost(y: i32, home: i32) -> u64 {
    if y < OPEN {
        CROWDED
    } else if (y - home).abs() <= 1 {
        1
    } else {
        AWAY
    }
}

const HORIZONTAL: [(Facing, i32, i32); 4] = [
    (Facing::North, 0, -1),
    (Facing::South, 0, 1),
    (Facing::East, 1, 0),
    (Facing::West, -1, 0),
];

#[derive(Clone)]
pub struct Goal {
    pub approach: Anchor,
    /// The stub's own cells, which a path arriving at `approach` may stand
    /// beside.
    pub stub: Vec<Anchor>,
    /// Clear cells protecting the way in, released once connected.
    pub clear: Vec<Anchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    Dust,
    Repeater(Facing),
}

#[derive(Debug, Clone, Copy)]
pub struct Step {
    at: Anchor,
    piece: Piece,
    strength: u8,
    /// A cell that has to stay air for a stair to connect.
    air: Option<Anchor>,
}

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

fn manhattan(a: Anchor, b: Anchor) -> u64 {
    ((a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()) as u64
}

struct Search<'a> {
    grid: &'a Grid,
    owner: Owner,
    goal: &'a Goal,
    forbidden: &'a CellSet<Anchor>,
    /// Cells refused because something foreign stood beside them: what a
    /// failed search was walled in by.
    walled: RefCell<Vec<Anchor>>,
}

/// Most walled cells a failed search records, and reports: those nearest
/// the goal, since what walls a search in sits around where it was going.
const WALL: usize = 1 << 16;
const REPORTED: usize = 4096;

impl Search<'_> {
    /// Whether a route block may stand at `at` (on a new floor), against the
    /// frozen grid. `recent` are the path cells just before it; `tap` is the
    /// tree cell this move leaves from, if any -- a branch's first cell
    /// necessarily stands beside its own tree there.
    fn can_stand(&self, at: Anchor, tap: Option<Anchor>, recent: &[Anchor]) -> bool {
        if !(LOWEST..=HIGHEST + self.grid.raise).contains(&at.y)
            || (!self.forbidden.is_empty() && self.forbidden.contains(&at))
        {
            return false;
        }
        // Below the open levels the ground is the tiles': a route runs there
        // only along its own net's ramps.
        let floor = down(at);
        if !self.grid.is_free(at) || !self.grid.is_free(floor) {
            return false;
        }
        [at, floor].into_iter().all(|cell| {
            if self.grid.foreign_near(cell, self.owner) {
                let mut walled = self.walled.borrow_mut();
                if walled.len() < WALL {
                    walled.push(cell);
                }
                return false;
            }
            !self.grid.own_near(cell, self.owner)
                || self.grid.own_blocks_near(cell, self.owner).all(|own| {
                    tap.is_some_and(|tap| chebyshev(own, tap) <= 1)
                        || recent.iter().any(|&cell| own == cell || own == down(cell))
                        || self.goal.stub.contains(&own)
                })
        })
    }

    /// What standing at `at` costs in someone else's kept-clear way: a
    /// route may take it, but the one it was kept for may later rip it up.
    fn trespass(&self, at: Anchor) -> u64 {
        if self.grid.clear_near(at, self.owner) || self.grid.clear_near(down(at), self.owner) {
            TRESPASS
        } else {
            0
        }
    }

    fn air_free(&self, at: Anchor) -> bool {
        self.grid.is_free(at) && (self.forbidden.is_empty() || !self.forbidden.contains(&at))
    }
}

#[derive(Clone, Copy)]
struct Node {
    at: Anchor,
    /// With a repeater on every cell of the path that could take one; 0
    /// only on a cell that must take one itself.
    strength: u8,
    parent: Option<usize>,
    /// The flat direction this node was entered in, if it was.
    heading: Option<(i32, i32)>,
    air: Option<Anchor>,
    cost: u64,
}

/// What leaving the tree from a tap costs before the first step: how much
/// signal delay the tree has already spent getting there, weighed by how
/// much the goal cares.
pub type Start<'a> = &'a (dyn Fn(Anchor) -> u64 + Sync);

/// Search one branch. `taps` are the tree's tappable cells with their
/// strengths.
fn search(
    grid: &Grid,
    owner: Owner,
    taps: &[(Anchor, u8)],
    start: Start,
    goal: &Goal,
    forbidden: &CellSet<Anchor>,
) -> Result<Vec<Step>, Vec<Anchor>> {
    let ctx = Search {
        grid,
        owner,
        goal,
        forbidden,
        walled: RefCell::new(Vec::new()),
    };
    let tree: CellSet<Anchor> = taps.iter().map(|(at, _)| *at).collect();
    let target = goal.approach;
    let home = home_level(owner, grid.lift);
    let mut nodes: Vec<Node> = Vec::new();
    // The Pareto front of (cost, strength) reached at each cell: an arrival
    // is kept only if no other is both as cheap and as strong.
    let mut best: CellMap<Anchor, Vec<(u64, u8)>> = CellMap::default();
    let mut open = BinaryHeap::new();
    let push = |best: &mut CellMap<Anchor, Vec<(u64, u8)>>,
                nodes: &mut Vec<Node>,
                open: &mut BinaryHeap<_>,
                next: Node| {
        let front = best.entry(next.at).or_default();
        if front
            .iter()
            .any(|&(cost, strength)| cost <= next.cost && strength >= next.strength)
        {
            return;
        }
        front.retain(|&(cost, strength)| !(next.cost <= cost && next.strength >= strength));
        front.push((next.cost, next.strength));
        nodes.push(next);
        open.push(Reverse((
            next.cost + GREED * manhattan(next.at, target),
            next.cost,
            nodes.len() - 1,
        )));
    };
    for &(at, strength) in taps {
        push(
            &mut best,
            &mut nodes,
            &mut open,
            Node {
                at,
                strength,
                parent: None,
                heading: None,
                air: None,
                cost: start(at),
            },
        );
    }
    let reach = taps
        .iter()
        .map(|(at, _)| manhattan(*at, target))
        .min()
        .unwrap_or(0) as usize;
    let budget = (BUDGET_PER_CELL * (reach + 10)).min(BUDGET);
    let mut expanded = 0;
    while let Some(Reverse((_, cost, index))) = open.pop() {
        let node = nodes[index];
        if node.at == target {
            return Ok(unwind(&nodes, index));
        }
        if !best[&node.at].contains(&(node.cost, node.strength)) {
            continue;
        }
        expanded += 1;
        if expanded > budget {
            return Err(nearest(ctx.walled.into_inner(), target));
        }
        let tap = tree.contains(&node.at).then_some(node.at);
        let behind = Behind::of(&nodes, index);
        let parent = node.parent.map(|parent| nodes[parent].at);
        let recent: Vec<Anchor> = [Some(node.at), parent].into_iter().flatten().collect();
        for (_, dx, dz) in HORIZONTAL {
            for dy in [0, 1, -1] {
                // This cell can take a repeater when the path runs straight
                // and flat through it; the next cell then reads 15.
                let socket = dy == 0 && node.parent.is_some() && node.heading == Some((dx, dz));
                let strength = if socket {
                    15
                } else if node.strength > 0 {
                    node.strength - 1
                } else {
                    continue;
                };
                let next = Anchor {
                    x: node.at.x + dx,
                    y: node.at.y + dy,
                    z: node.at.z + dz,
                };
                if next == target && strength == 0 {
                    continue;
                }
                let air = match dy {
                    1 => Some(up(node.at)),
                    -1 => Some(up(next)),
                    _ => None,
                };
                if air.is_some_and(|air| !ctx.air_free(air)) {
                    continue;
                }
                let placed: &[Anchor] = if next == target {
                    &[]
                } else {
                    std::slice::from_ref(&next)
                };
                if (next != target && !ctx.can_stand(next, tap, &recent))
                    || behind.clashes(placed, air)
                {
                    continue;
                }
                let step = if dy == 0 { STEP } else { STAIR } + ctx.trespass(next);
                let crowd = level_cost(next.y, home);
                push(
                    &mut best,
                    &mut nodes,
                    &mut open,
                    Node {
                        at: next,
                        strength,
                        parent: Some(index),
                        heading: (dy == 0).then_some((dx, dz)),
                        air,
                        cost: cost + step * crowd,
                    },
                );
            }
        }
    }
    Err(nearest(ctx.walled.into_inner(), target))
}

fn nearest(mut walled: Vec<Anchor>, target: Anchor) -> Vec<Anchor> {
    walled.sort_by_key(|&cell| manhattan(cell, target));
    walled.truncate(REPORTED);
    walled
}

/// How many cells back a move checks the path behind it for clashes the
/// grid cannot see yet. Anything older is caught at commit.
const LOOKBACK: usize = 8;

/// The path behind one search node, gathered once per expansion: its
/// blocks (each cell and its floor) with how many steps
/// back they are, and the stair air it reserved.
struct Behind {
    blocks: Vec<(Anchor, usize)>,
    air: Vec<Anchor>,
}

impl Behind {
    fn of(nodes: &[Node], index: usize) -> Behind {
        let mut behind = Behind {
            blocks: Vec::with_capacity(4 * (LOOKBACK + 1)),
            air: Vec::with_capacity(LOOKBACK + 1),
        };
        let mut cursor = Some(index);
        let mut depth = 0;
        while let Some(at) = cursor {
            let node = nodes[at];
            behind.blocks.push((node.at, depth));
            behind.blocks.push((down(node.at), depth));
            behind.air.extend(node.air);
            depth += 1;
            if depth > LOOKBACK {
                break;
            }
            cursor = node.parent;
        }
        behind
    }

    /// Whether new blocks `cells` (and the stair air `air`) collide with the
    /// path: they may not take any of its cells, and may not stand beside
    /// any of them but the last two.
    fn clashes(&self, cells: &[Anchor], air: Option<Anchor>) -> bool {
        self.blocks.iter().any(|&(old, depth)| {
            air == Some(old)
                || cells.iter().any(|&cell| {
                    [cell, down(cell)]
                        .into_iter()
                        .any(|new| new == old || (depth >= 2 && chebyshev(new, old) <= 1))
                })
        }) || self.air.iter().any(|&old| {
            air == Some(old) || cells.iter().any(|&cell| cell == old || down(cell) == old)
        })
    }
}

fn unwind(nodes: &[Node], mut index: usize) -> Vec<Step> {
    let mut steps = Vec::new();
    loop {
        let node = nodes[index];
        steps.push(Step {
            at: node.at,
            piece: Piece::Dust,
            strength: node.strength,
            air: node.air,
        });
        match node.parent {
            Some(parent) => index = parent,
            None => break,
        }
    }
    steps.reverse();
    // The tap itself leads the path and has no stair to keep clear.
    steps[0].air = None;
    repeaters(&mut steps);
    steps
}

/// Put repeaters on a found path, each as late as it can go: walking from
/// the tap, a cell the signal would reach dead takes one on the last cell
/// before it that runs straight and flat between its neighbours. The search
/// kept every cell live with a repeater on each such cell, so one is always
/// found.
fn repeaters(steps: &mut [Step]) {
    let straight = |steps: &[Step], at: usize| {
        at >= 1 && at + 1 < steps.len() && {
            let (before, here, after) = (steps[at - 1].at, steps[at].at, steps[at + 1].at);
            before.y == here.y
                && here.y == after.y
                && here.x - before.x == after.x - here.x
                && here.z - before.z == after.z - here.z
        }
    };
    let mut strength = steps[0].strength;
    let mut from = 1;
    let mut at = 1;
    while at < steps.len() {
        if strength > 1 {
            strength -= 1;
            steps[at].strength = strength;
            at += 1;
            continue;
        }
        let socket = (from..=at)
            .rev()
            .find(|&socket| straight(steps, socket))
            .expect("the search keeps every path repeatable");
        let (here, after) = (steps[socket].at, steps[socket + 1].at);
        let facing = HORIZONTAL
            .iter()
            .find(|&&(_, dx, dz)| after.x - here.x == dx && after.z - here.z == dz)
            .map(|&(facing, _, _)| facing)
            .unwrap();
        steps[socket].piece = Piece::Repeater(facing);
        steps[socket + 1].strength = 15;
        strength = 15;
        from = socket + 1;
        at = socket + 2;
    }
}

/// Place a found path, checking each new block against everything placed
/// before it, its own earlier cells included. The first refused cell comes
/// back for the search to avoid.
fn commit(
    grid: &mut Grid,
    owner: Owner,
    steps: &[Step],
    goal: &Goal,
    branch: usize,
) -> Result<Laid, Anchor> {
    let tap = steps[0].at;
    let mut live = Vec::new();
    let mut cells = Vec::new();
    let mut place = |grid: &mut Grid, at: Anchor, block| {
        grid.place_for(at, owner, block, Some(branch));
        cells.push(at);
    };
    for (index, step) in steps.iter().enumerate().skip(1) {
        let recent: Vec<Anchor> = steps[index.saturating_sub(2)..index]
            .iter()
            .flat_map(|before| [before.at, down(before.at)])
            .collect();
        let allowed = |own: Anchor| {
            recent.contains(&own)
                || goal.stub.contains(&own)
                || (index == 1 && chebyshev(own, tap) <= 1)
        };
        if let Some(air) = step.air {
            if !grid.is_free(air) {
                return Err(air);
            }
            place(grid, air, None);
        }
        if step.at == goal.approach {
            live.push((step.at, step.strength));
            continue;
        }
        for cell in [down(step.at), step.at] {
            let fine = grid.is_free(cell)
                && !grid.foreign_near(cell, owner)
                && grid.own_blocks_near(cell, owner).all(allowed);
            if !fine {
                return Err(step.at);
            }
        }
        place(grid, down(step.at), Some(stone()));
        match step.piece {
            Piece::Dust => {
                place(grid, step.at, Some(dust()));
                live.push((step.at, step.strength));
            }
            Piece::Repeater(facing) => place(grid, step.at, Some(repeater(facing))),
        }
    }
    Ok(Laid { tap, live, cells })
}

/// A laid branch: the tree cell it left from, the tappable cells it added
/// (the reached stub included), and every grid cell it placed.
pub struct Laid {
    pub tap: Anchor,
    pub live: Vec<(Anchor, u8)>,
    pub cells: Vec<Anchor>,
}

/// A path for one branch from `taps` to `goal`, searched against the grid as
/// it stands without touching it -- so several can be searched at once.
pub fn find(
    grid: &Grid,
    owner: Owner,
    taps: &[(Anchor, u8)],
    start: Start,
    goal: &Goal,
) -> Result<Vec<Step>, Vec<Anchor>> {
    search(grid, owner, taps, start, goal, &CellSet::default())
}

impl Step {
    /// Where this cell of the path is.
    pub fn at(&self) -> Anchor {
        self.at
    }

    /// A cell of a path found elsewhere: dust, or a repeater facing
    /// `repeater`, at `strength`, with the air a stair into it needs.
    pub fn new(at: Anchor, repeater: Option<Facing>, strength: u8, air: Option<Anchor>) -> Step {
        Step {
            at,
            piece: repeater.map_or(Piece::Dust, Piece::Repeater),
            strength,
            air,
        }
    }
}

/// Lay a path found elsewhere, checking each block as [`place`] does; on
/// failure the grid is left untouched and the first refused cell named.
pub fn check_and_place(
    grid: &mut Grid,
    owner: Owner,
    steps: &[Step],
    goal: &Goal,
    branch: usize,
) -> Result<Laid, Anchor> {
    let mark = grid.mark();
    commit(grid, owner, steps, goal, branch).inspect_err(|_| grid.undo(mark))
}

/// Lay a path [`find`] returned, if it still fits: another path laid since
/// may have taken its cells. On failure the grid is left untouched.
pub fn place(
    grid: &mut Grid,
    owner: Owner,
    steps: &[Step],
    goal: &Goal,
    branch: usize,
) -> Option<Laid> {
    let mark = grid.mark();
    match commit(grid, owner, steps, goal, branch) {
        Ok(laid) => Some(laid),
        Err(_) => {
            grid.undo(mark);
            None
        }
    }
}

/// Lay one branch from `taps` to `goal`, its cells tagged `branch`. A
/// failure leaves the grid untouched and names the cells the search was
/// walled in by.
pub fn lay(
    grid: &mut Grid,
    owner: Owner,
    taps: &[(Anchor, u8)],
    start: Start,
    goal: &Goal,
    branch: usize,
) -> Result<Laid, Vec<Anchor>> {
    let mut forbidden = CellSet::default();
    let mut walled = Vec::new();
    for _ in 0..=RETRIES {
        let steps = search(grid, owner, taps, start, goal, &forbidden)?;
        let mark = grid.mark();
        match commit(grid, owner, &steps, goal, branch) {
            Ok(laid) => return Ok(laid),
            Err(cell) => {
                grid.undo(mark);
                forbidden.insert(cell);
                walled.push(cell);
            }
        }
    }
    Err(walled)
}
