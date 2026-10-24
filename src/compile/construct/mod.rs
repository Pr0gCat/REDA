//! Constructive generation: place one netlist element at a time, route its
//! connections at once, and freeze both.
//!
//! Elements are placed in dependency order -- primary inputs, flip-flops,
//! then every combinational gate after the gates feeding it, then the output
//! ports. Placing an element means trying positions near the things it
//! connects to, nearest first, until one fits and every route it needs can
//! be laid: each of its inputs from that net's tree, and its own output net
//! to every sink already waiting for it (a flip-flop's D, placed before its
//! driver). Nothing laid is ever moved or rewritten; a position that does
//! not work is undone whole and the next one tried.
//!
//! Electrical correctness is a property of each step: tiles are fixed known
//! patterns, routes carry their signal strength in the search, and nets keep
//! a one-cell gap from each other ([`grid`]). Quality is not a goal.

mod dewey;
mod grid;
mod hierarchy;
mod placement;
mod route;
mod tile;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

use grid::{CellMap, Grid, Owner};
pub use placement::Side;
use route::Goal;
pub use hierarchy::construct_hierarchical;
pub use tile::DFF_SITES;
use tile::{Role, Tile};

use super::geometry::Anchor;
use super::topology::GateKind;
use super::Netlist;
use crate::redstone::world::block::{BlockKind, BlockState};
use crate::redstone::world::storage::World;

/// The level every tile stands on (its floors one below).
const BASE: i32 = 2;
/// How far from its target an element may be placed.
const REACH: i32 = 48;
/// Tiles with this many cells or more are blocks: they span lattice slots
/// rather than set the slot size.
const BLOCK_CELLS: usize = 64;
/// Positions that fit but whose routes failed, tried before giving up.
const ATTEMPTS: usize = 24;
/// How many rip-ups routing may spend per branch it lays, and how often one
/// branch may be retried before routing gives up.
const RIPS_PER_GOAL: usize = 4;
const TRIES: usize = 8;
/// How often a branch fails before routing rips every route near its door.
const DOOR_AFTER: usize = 3;
/// How finely routing order tells criticalities apart.
const CRITICALITY_BANDS: f64 = 10.0;
/// How many route searches one batch holds. Fixed, not the core count, so
/// every machine lays the same routes; rayon spreads a batch over the
/// threads there are.
const WORKERS: usize = 16;
/// How far down the queue a batch looks for goals of other nets: a wide
/// net queues its goals together, and a batch takes only one of each net's.
const LOOKAHEAD: usize = 4 * WORKERS;
/// How many placements the top level routes at once. Fixed, like
/// [`WORKERS`], so every machine keeps the same one.
const WAVE: usize = 4;
/// How far a new tile keeps from everything it does not own, so a route can
/// always pass between it and its neighbours (a route cell needs a one-cell
/// gap on either side).
const MOAT: i32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConstructError {
    #[error("gate {gate} has kind {kind:?}, which has no tile; lower the netlist first")]
    NoTile { gate: String, kind: GateKind },
    #[error("the netlist has a loop that no flip-flop breaks")]
    CombinationalLoop,
    #[error("signal {signal} is read but nothing drives it")]
    Undriven { signal: String },
    #[error("no position within reach of its connections fits {element} and routes; nearest try: {reason}")]
    Unplaceable { element: String, reason: String },
}

/// Which placement and routing method generation uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Method {
    /// Annealed slots, longest-first routing with rip-up (this module).
    #[default]
    Constructive,
    /// The baseline: dewey's placement and routing (Nguyen 2018), see
    /// [`dewey`].
    Dewey,
    /// dewey's placement, then this module's routing.
    DeweyPlace,
}

/// What generation produced: every block, and where each port is.
pub struct Construction {
    pub blocks: Vec<(Anchor, BlockState)>,
    /// Where each primary input is driven from: the clear cell beyond its
    /// port's stub, which a lever there would power.
    pub inputs: BTreeMap<String, Anchor>,
    /// Where each primary output can be read: the clear cell its port's
    /// dust points into, which a lamp there would light from.
    pub outputs: BTreeMap<String, Anchor>,
    /// What a test bench adds to drive and read the ports: a lever on a
    /// floor at every input, a lamp at every output. Not part of the
    /// circuit.
    pub bench: Vec<(Anchor, BlockState)>,
    /// What takes these coordinates to the caller's, when ports were given
    /// cells in them.
    pub shift: Option<Anchor>,
    /// Where each gate's own output value can be read.
    pub gate_outputs: BTreeMap<String, Anchor>,
}

#[derive(Clone, Default)]
struct Net {
    /// Tappable cells and their strengths; empty until the driver is placed.
    taps: Vec<(Anchor, u8)>,
    /// Where the driver sits, for placing things near it.
    home: Option<Anchor>,
    /// Sinks placed before the driver, waiting for it.
    waiting: Vec<Goal>,
    /// A block port's outer side: the parent routes it, so here its stub's
    /// way in and its exit only stay clear.
    external: bool,
    /// Sinks not yet connected, placed or not.
    unconnected: usize,
    /// The driver's exit clear cells, released with the last sink.
    exit: Vec<Anchor>,
    /// How critical its most critical connection is, `0..=1`: the most
    /// critical nets are routed first, before others take the straight
    /// ways, and their branches leave the tree near its root.
    criticality: f64,
    /// How many cells of tree lie between the driver and each tap.
    depth: HashMap<Anchor, u32>,
    /// Which stretch of the tree each live cell is on (see
    /// [`route::Segments`]), and how many stretches there are.
    segments: HashMap<Anchor, u32>,
    stretches: u32,
}

impl Net {
    /// A branch's starting cost at each tap: the tree it rides on before
    /// leaving, in route steps, as much as the net is critical.
    fn start(&self) -> impl Fn(Anchor) -> u64 + Sync + '_ {
        move |at| (self.criticality * f64::from(self.depth.get(&at).copied().unwrap_or(0))) as u64
    }
}

#[derive(Clone)]
struct Builder {
    grid: Grid,
    nets: Vec<Net>,
    net_of: HashMap<String, usize>,
    branches: Vec<Branch>,
}

/// One laid route branch, kept so it can be ripped up again.
#[derive(Clone)]
struct Branch {
    net: usize,
    goal: Goal,
    /// The tree cell it left from.
    tap: Anchor,
    live: Vec<(Anchor, u8)>,
    cells: Vec<Anchor>,
    alive: bool,
}

/// One element to place: its tile, the nets its inputs read, the net it
/// drives (if any), and which gate owns its body.
#[derive(Clone)]
struct Element<'a> {
    name: &'a str,
    tile: Tile,
    inputs: Vec<usize>,
    /// Whether inputs may be given to any stub (NOR and OR).
    commutative: bool,
    /// The nets it drives, by output.
    outputs: Vec<usize>,
    body: usize,
}

impl Construction {
    /// The blocks as a world whose lowest corner is the origin -- once bare
    /// and once with the bench -- and the port and probe positions moved
    /// with them.
    pub fn into_world(self) -> Placed {
        let low = self
            .blocks
            .iter()
            .chain(&self.bench)
            .fold(None, |low: Option<Anchor>, (at, _)| {
                Some(low.map_or(*at, |low| Anchor {
                    x: low.x.min(at.x),
                    y: low.y.min(at.y),
                    z: low.z.min(at.z),
                }))
            });
        let low = low.unwrap_or(Anchor { x: 0, y: 0, z: 0 });
        let shift = |at: &Anchor| (at.x - low.x, at.y - low.y, at.z - low.z);
        let size = self.blocks.iter().chain(&self.bench).fold((1, 1, 1), |size, (at, _)| {
            let (x, y, z) = shift(at);
            (size.0.max(x + 1), size.1.max(y + 1), size.2.max(z + 1))
        });
        let mut world = World::new(size.0, size.1, size.2);
        for (at, block) in self.blocks {
            let (x, y, z) = shift(&at);
            world.set(x, y, z, block);
        }
        let origin = self
            .shift
            .map_or((0, 0, 0), |shift| (low.x + shift.x, low.y + shift.y, low.z + shift.z));
        let mut bench = world.clone();
        for (at, block) in self.bench {
            let (x, y, z) = shift(&at);
            bench.set(x, y, z, block);
        }
        let moved = |ports: BTreeMap<String, Anchor>| {
            ports
                .iter()
                .map(|(name, at)| (name.clone(), shift(at)))
                .collect()
        };
        Placed {
            world,
            bench,
            origin,
            inputs: moved(self.inputs),
            outputs: moved(self.outputs),
            gate_outputs: moved(self.gate_outputs),
        }
    }
}

/// A [`Construction`] as a world.
pub struct Placed {
    pub world: World,
    pub bench: World,
    pub origin: (i32, i32, i32),
    pub inputs: BTreeMap<String, (i32, i32, i32)>,
    pub outputs: BTreeMap<String, (i32, i32, i32)>,
    pub gate_outputs: BTreeMap<String, (i32, i32, i32)>,
}

impl Builder {
    fn net(&mut self, signal: &str) -> usize {
        if let Some(&net) = self.net_of.get(signal) {
            return net;
        }
        self.nets.push(Net::default());
        self.net_of.insert(signal.to_string(), self.nets.len() - 1);
        self.nets.len() - 1
    }

    fn owner(element: &Element, role: Role) -> Owner {
        match role {
            Role::Body => Owner::Gate(element.body),
            Role::Output(output) => Owner::Net(element.outputs[output]),
            Role::Input(input) => Owner::Net(element.inputs[input]),
        }
    }

    /// Whether the tile fits at `origin` against everything already placed.
    fn fits(&self, element: &Element, origin: Anchor) -> bool {
        let mut blocks = element
            .tile
            .cells
            .iter()
            .map(|cell| (cell.offset, cell.role, cell.block.is_some()));
        let mut clear: Vec<((i32, i32, i32), Role)> = element
            .tile
            .exits
            .iter()
            .enumerate()
            .flat_map(|(output, exit)| exit.iter().map(move |&offset| (offset, Role::Output(output))))
            .collect();
        for stub in &element.tile.stubs {
            clear.extend(
                stub.entry
                    .iter()
                    .map(|&offset| (offset, Role::Input(stub.input))),
            );
        }
        element.tile.cells.iter().all(|cell| self.grid.is_free(tile::at(origin, cell.offset)))
            && clear.iter().all(|&(offset, _)| self.grid.is_free(tile::at(origin, offset)))
            && blocks.all(|(offset, role, solid)| {
                let at = tile::at(origin, offset);
                let owner = Self::owner(element, role);
                !solid
                    || (!self.grid.foreign_within(at, owner, MOAT)
                        && !self.grid.foreign_near(at, owner)
                        && !self.grid.clear_near(at, owner)
                        && !self.grid.own_near(at, owner)
                        && !self.grid.own_clear_near(at, owner))
            })
            // A corridor may not run into anything, its own net's corridors
            // included: a route along one would use the other's cells.
            && clear.iter().all(|&(offset, role)| {
                let at = tile::at(origin, offset);
                let owner = Self::owner(element, role);
                !self.grid.foreign_near(at, owner)
                    && !self.grid.clear_near(at, owner)
                    && !self.grid.own_near(at, owner)
                    && !self.grid.own_clear_near(at, owner)
            })
    }

    /// Put the tile down at `origin` and lay every route it completes. An
    /// error leaves the builder as it was.
    fn try_place(&mut self, element: &Element, origin: Anchor) -> Result<(), String> {
        let mark = self.grid.mark();
        let touched: Vec<usize> = element
            .inputs
            .iter()
            .copied()
            .chain(element.outputs.iter().copied())
            .collect();
        let saved: Vec<(usize, Net)> = touched
            .iter()
            .map(|&net| (net, self.nets[net].clone()))
            .collect();
        let placed = self.place(element, origin);
        if placed.is_err() {
            self.grid.undo(mark);
            for (net, state) in saved {
                self.nets[net] = state;
            }
        }
        placed
    }

    fn place(&mut self, element: &Element, origin: Anchor) -> Result<(), String> {
        for cell in &element.tile.cells {
            let owner = Self::owner(element, cell.role);
            self.grid
                .place(tile::at(origin, cell.offset), owner, cell.block.clone());
        }
        for (output, &net) in element.outputs.iter().enumerate() {
            let exit: Vec<Anchor> = element.tile.exits[output]
                .iter()
                .map(|&offset| tile::at(origin, offset))
                .collect();
            for &at in &exit {
                self.grid.keep_clear(at, Owner::Net(net));
            }
            self.nets[net].exit = exit;
            let taps: Vec<(Anchor, u8)> = element
                .tile
                .source
                .iter()
                .filter(|source| source.output == output && source.tappable)
                .map(|source| (tile::at(origin, source.offset), source.strength))
                .collect();
            // A block's outputs leave from ports far from its origin.
            let home = if element.outputs.len() > 1 { taps.first().map_or(origin, |(at, _)| *at) } else { origin };
            self.nets[net].home = Some(home);
            self.nets[net].taps = taps;
            // The driver's own cells are the first stretch of its tree.
            for source in element.tile.source.iter().filter(|source| source.output == output) {
                self.nets[net].segments.insert(tile::at(origin, source.offset), 0);
            }
            self.nets[net].stretches = 1;
        }
        for stub in &element.tile.stubs {
            let net = element.inputs[stub.input];
            let goal = Goal {
                approach: tile::at(origin, stub.approach),
                stub: stub
                    .cells
                    .iter()
                    .map(|&offset| tile::at(origin, offset))
                    .collect(),
                clear: stub
                    .entry
                    .iter()
                    .map(|&offset| tile::at(origin, offset))
                    .collect(),
                need: stub.need,
            };
            for &at in &goal.clear {
                self.grid.keep_clear(at, Owner::Net(net));
            }
            if !self.nets[net].external {
                self.nets[net].waiting.push(goal);
            }
        }
        Ok(())
    }

    /// Lay every net, once everything is placed: the shortest first, each
    /// net's sinks nearest its driver first. A branch whose search is walled
    /// in rips up the branch walling it in most and goes again; what was
    /// ripped goes to the back of the queue. It gives up as soon as `beaten`
    /// says another placement has already won.
    fn route_all(&mut self, beaten: impl Fn() -> bool) -> Result<(), String> {
        let span = |net: &Net| {
            let home = net.home.expect("every read net has a driver");
            net.waiting
                .iter()
                .map(|goal| distance(goal.approach, home))
                .max()
                .unwrap_or(0)
        };
        let mut order: Vec<usize> = (0..self.nets.len())
            .filter(|&net| !self.nets[net].waiting.is_empty())
            .collect();
        // Nets in bands of criticality, most critical first; longest first
        // within a band, so the long ones still find room.
        let band = |net: &Net| (net.criticality * CRITICALITY_BANDS) as i64;
        order.sort_by_key(|&net| {
            (
                std::cmp::Reverse(band(&self.nets[net])),
                std::cmp::Reverse(span(&self.nets[net])),
                net,
            )
        });
        let mut queue = std::collections::VecDeque::new();
        for net in order {
            let home = self.nets[net].home.expect("every read net has a driver");
            let mut goals = std::mem::take(&mut self.nets[net].waiting);
            goals.sort_by_key(|goal| distance(goal.approach, home));
            queue.extend(goals.into_iter().map(|goal| (net, goal)));
        }
        let mut rips = RIPS_PER_GOAL * queue.len();
        let mut tries: HashMap<Anchor, usize> = HashMap::new();
        while !queue.is_empty() {
            if beaten() {
                return Err("a better placement routed".to_string());
            }
            // Search a batch of goals of different nets at once against the
            // grid as it stands, then lay them in queue order; one whose
            // path a batch-mate took is laid again alone.
            let mut batch: Vec<(usize, Goal)> = Vec::new();
            while batch.len() < WORKERS {
                let Some(position) = queue
                    .iter()
                    .position(|(net, _)| batch.iter().all(|(other, _)| other != net))
                else {
                    break;
                };
                if position > LOOKAHEAD {
                    break;
                }
                batch.push(queue.remove(position).expect("a found position"));
            }
            let found: Vec<Result<Vec<route::Step>, Vec<Anchor>>> = {
                let grid = &self.grid;
                let nets = &self.nets;
                batch
                    .par_iter()
                    .map(|(net, goal)| {
                        route::find(
                            grid,
                            Owner::Net(*net),
                            &nets[*net].taps,
                            &nets[*net].start(),
                            &nets[*net].segments,
                            goal,
                        )
                    })
                    .collect()
            };
            for ((net, goal), found) in batch.into_iter().zip(found) {
                let owner = Owner::Net(net);
                let branch = self.branches.len();
                // A batch-mate's rip-up may have taken the tap this path
                // left from; searched again, it cannot join a dead stub
                // that a later re-laid branch would then feed from it.
                let tapped = |steps: &[route::Step]| {
                    self.nets[net]
                        .taps
                        .iter()
                        .any(|(at, _)| *at == steps[0].at())
                };
                let laid = match found {
                    Ok(steps) if !tapped(&steps) => route::lay(
                        &mut self.grid,
                        owner,
                        &self.nets[net].taps,
                        &self.nets[net].start(),
                        &self.nets[net].segments,
                        &goal,
                        branch,
                    ),
                    Ok(steps) => match route::place(&mut self.grid, owner, &steps, &self.nets[net].segments, &goal, branch) {
                        Some(laid) => Ok(laid),
                        None => route::lay(
                            &mut self.grid,
                            owner,
                            &self.nets[net].taps,
                            &self.nets[net].start(),
                            &self.nets[net].segments,
                            &goal,
                            branch,
                        ),
                    },
                    Err(walled) => Err(walled),
                };
                match laid {
                    Ok(laid) => {
                        for &at in &goal.clear {
                            self.grid.release(at, owner);
                        }
                        let state = &mut self.nets[net];
                        state.taps.extend(laid.live.iter().copied());
                        let base = state.depth.get(&laid.tap).copied().unwrap_or(0);
                        for (index, (at, _)) in laid.live.iter().enumerate() {
                            state.depth.insert(*at, base + index as u32 + 1);
                        }
                        laid.stretch(&mut state.segments, &mut state.stretches);
                        state.unconnected -= 1;
                        if state.unconnected == 0 {
                            for at in std::mem::take(&mut state.exit) {
                                self.grid.release(at, owner);
                            }
                        }
                        self.branches.push(Branch {
                            net,
                            goal,
                            tap: laid.tap,
                            live: laid.live,
                            cells: laid.cells,
                            alive: true,
                        });
                    }
                    Err(walled) => {
                        let tried = tries.entry(goal.approach).or_default();
                        *tried += 1;
                        // What walls a goal in is mostly what stands at its
                        // door, so that goes first; failing that, what the
                        // search ran into most.
                        let door: Vec<Anchor> = goal.clear.iter().copied().chain(std::iter::once(goal.approach)).collect();
                        let victim = (rips > 0 && *tried <= TRIES)
                            .then(|| self.blocker(net, &door).or_else(|| self.blocker(net, &walled)))
                            .flatten();
                        let Some(victim) = victim else {
                            return Err(format!(
                                "no route for net {net} from {} taps to {:?}",
                                self.nets[net].taps.len(),
                                goal.approach
                            ));
                        };
                        rips -= 1;
                        let mut ripped = Vec::new();
                        self.rip(victim, &mut ripped);
                        // A goal walled in again and again is walled in by
                        // routes that come straight back when ripped one at
                        // a time: clear everything a cell further out at once.
                        if *tried >= DOOR_AFTER {
                            let porch: Vec<Anchor> = door.iter().flat_map(|&at| grid::around(at)).collect();
                            while let Some(victim) = self.blocker(net, &porch) {
                                rips = rips.saturating_sub(1);
                                self.rip(victim, &mut ripped);
                            }
                        }
                        queue.push_front((net, goal));
                        queue.extend(ripped);
                    }
                }
            }
        }
        Ok(())
    }

    /// The live branch of another net with the most blocks beside `walled`.
    fn blocker(&self, net: usize, walled: &[Anchor]) -> Option<usize> {
        let mut count: HashMap<usize, usize> = HashMap::new();
        for &cell in walled {
            for at in grid::around(cell) {
                if let Some(branch) = self.grid.branch_at(at) {
                    if self.branches[branch].alive && self.branches[branch].net != net {
                        *count.entry(branch).or_default() += 1;
                    }
                }
            }
        }
        count
            .into_iter()
            .max_by_key(|&(branch, hits)| (hits, branch))
            .map(|(branch, _)| branch)
    }

    /// Take `branch` out of the grid -- and every branch tapped off it --
    /// handing their goals back to be laid again.
    fn rip(&mut self, branch: usize, ripped: &mut Vec<(usize, Goal)>) {
        let net = self.branches[branch].net;
        let live: Vec<Anchor> = self.branches[branch]
            .live
            .iter()
            .map(|(at, _)| *at)
            .collect();
        let dependents: Vec<usize> = (0..self.branches.len())
            .filter(|&other| {
                other != branch
                    && self.branches[other].alive
                    && self.branches[other].net == net
                    && live.contains(&self.branches[other].tap)
            })
            .collect();
        for dependent in dependents {
            self.rip(dependent, ripped);
        }
        for &at in self.branches[branch].cells.iter().rev() {
            self.grid.remove(at);
        }
        self.branches[branch].alive = false;
        let goal = self.branches[branch].goal.clone();
        let owner = Owner::Net(net);
        for &at in &goal.clear {
            self.grid.keep_clear(at, owner);
        }
        let state = &mut self.nets[net];
        state.taps.retain(|(at, _)| !live.contains(at));
        state.unconnected += 1;
        ripped.push((net, goal));
    }

    /// Give each input the stub nearest its driver.
    fn assign_stubs(&self, element: &mut Element, origin: Anchor) {
        if !element.commutative || element.inputs.len() < 2 {
            return;
        }
        let homes: Vec<Option<Anchor>> = element
            .inputs
            .iter()
            .map(|&net| self.nets[net].home)
            .collect();
        let approaches: Vec<Anchor> = element
            .tile
            .stubs
            .iter()
            .map(|stub| tile::at(origin, stub.approach))
            .collect();
        let cost = |order: &[usize]| -> i32 {
            order
                .iter()
                .enumerate()
                .map(|(stub, &input)| {
                    homes[input].map_or(0, |home| distance(home, approaches[stub]))
                })
                .sum()
        };
        let mut best: Vec<usize> = (0..element.inputs.len()).collect();
        let mut best_cost = cost(&best);
        for order in permutations(element.inputs.len()) {
            let candidate = cost(&order);
            if candidate < best_cost {
                best_cost = candidate;
                best = order;
            }
        }
        let original = element.inputs.clone();
        element.inputs = best.iter().map(|&input| original[input]).collect();
    }

    /// Positions to try, best first: rings around `target`, then -- when
    /// the neighbourhood is full -- fresh space just beyond everything placed.
    fn candidates(&self, target: Anchor) -> Vec<Anchor> {
        let at = |x, z| Anchor { x, y: target.y, z };
        let mut near: Vec<Anchor> = (0..=REACH)
            .flat_map(ring_offsets)
            .map(|(dx, dz)| at(target.x + dx, target.z + dz))
            .collect();
        if let Some((low, high)) = self.grid.extent() {
            let margin = 2 * MOAT + 4;
            let mut edge: Vec<Anchor> = (low.x - margin..=high.x + margin)
                .step_by(2)
                .map(|x| at(x, low.z - margin))
                .chain(
                    (low.z - margin..=high.z + margin)
                        .step_by(2)
                        .flat_map(|z| [at(low.x - margin, z), at(high.x + margin, z)]),
                )
                .collect();
            edge.sort_by_key(|origin| (distance(*origin, target), origin.x, origin.z));
            near.extend(edge);
        }
        near
    }

    /// Place `element` at the first position from `target` outward that fits.
    fn place_around(
        &mut self,
        element: &Element,
        target: Anchor,
    ) -> Result<Anchor, ConstructError> {
        let mut element = element.clone();
        let element = &mut element;
        let mut attempts = 0;
        let mut first = None;
        for origin in self.candidates(target) {
            self.assign_stubs(element, origin);
            if !self.fits(element, origin) {
                continue;
            }
            match self.try_place(element, origin) {
                Ok(()) => return Ok(origin),
                Err(reason) => {
                    first.get_or_insert(format!("at {origin:?}: {reason}"));
                }
            }
            attempts += 1;
            if attempts >= ATTEMPTS {
                break;
            }
        }
        Err(ConstructError::Unplaceable {
            element: element.name.to_string(),
            reason: first.unwrap_or_else(|| "no position fits".to_string()),
        })
    }
}

/// One element to place, before it is an [`Element`].
enum What {
    Input(String),
    Gate(usize),
    /// A finished block, by its index among the blocks given.
    Block(usize),
    Output(String),
}

/// The signals an element reads and drives.
fn signals<'a>(netlist: &'a Netlist, blocks: &'a [BlockUse], what: &'a What) -> (Vec<&'a str>, Vec<&'a str>) {
    match what {
        What::Input(name) => (Vec::new(), vec![name.as_str()]),
        &What::Gate(index) => {
            let gate = &netlist.gates[index];
            (gate.inputs.iter().map(String::as_str).collect(), vec![gate.output.as_str()])
        }
        &What::Block(index) => (
            blocks[index].inputs.iter().map(String::as_str).collect(),
            blocks[index].outputs.iter().map(String::as_str).collect(),
        ),
        What::Output(name) => (vec![name.as_str()], Vec::new()),
    }
}

/// Where on element `element` signal `signal` attaches, from its middle.
type Attach<'a> = &'a dyn Fn(usize, &str) -> placement::Offset;

/// Attaching every signal at an element's middle.
fn middle(_: usize, _: &str) -> placement::Offset {
    (0.0, 0.0)
}

/// Each net as the elements it joins, and where on each.
fn memberships(netlist: &Netlist, blocks: &[BlockUse], whats: &[What], at: Attach) -> Vec<Vec<(usize, placement::Offset)>> {
    let mut members: BTreeMap<&str, Vec<(usize, placement::Offset)>> = BTreeMap::new();
    for (index, what) in whats.iter().enumerate() {
        let (reads, drives) = signals(netlist, blocks, what);
        for signal in reads.into_iter().chain(drives) {
            let list = members.entry(signal).or_default();
            if list.last().map(|&(element, _)| element) != Some(index) {
                list.push((index, at(index, signal)));
            }
        }
    }
    members.into_values().collect()
}

/// Where a placement put the ports, probes and flip-flops, every element's
/// origin, and the placement's estimated critical delay.
#[derive(Default)]
struct Ports {
    origins: Vec<Anchor>,
    /// Each element's tile as placed: a port turned to face its way out.
    tiles: Vec<Tile>,
    critical: f64,
    inputs: BTreeMap<String, Anchor>,
    gate_outputs: BTreeMap<String, Anchor>,
    outputs: BTreeMap<String, Anchor>,
    flip_flops: Vec<(usize, Anchor)>,
}

/// Every driver-to-reader connection between elements, and where on each it
/// attaches, and which elements start and end timing paths on their own
/// (flip-flops).
fn timing_graph(netlist: &Netlist, blocks: &[BlockUse], whats: &[What], at: Attach) -> placement::Timing {
    let mut driver: HashMap<&str, usize> = HashMap::new();
    for (index, what) in whats.iter().enumerate() {
        for signal in signals(netlist, blocks, what).1 {
            driver.insert(signal, index);
        }
    }
    let mut edges = Vec::new();
    let mut ends = Vec::new();
    for (index, what) in whats.iter().enumerate() {
        for signal in signals(netlist, blocks, what).0 {
            if let Some(&from) = driver.get(signal) {
                edges.push((from, index));
                ends.push((at(from, signal), at(index, signal)));
            }
        }
    }
    let registered = whats
        .iter()
        .map(|what| matches!(what, &What::Gate(index) if netlist.gates[index].kind.is_sequential()))
        .collect();
    let intrinsic = whats
        .iter()
        .map(|what| match what {
            &What::Block(index) => blocks[index].delay,
            _ => 0.0,
        })
        .collect();
    placement::Timing::new(edges, registered, intrinsic, vec![0.0; whats.len()], ends)
}

/// The combinational gates in an order that places each one shortly before
/// what reads it: every sink (a primary output, a flip-flop input, a gate
/// nothing reads) pulls in its fan-in cone depth first. Plain topological
/// order places a whole level before the next, so a wide level's outputs
/// all wait unconnected while the rest is placed and get walled in.
fn cone_order(netlist: &Netlist) -> Vec<usize> {
    let producer: HashMap<&str, usize> = netlist
        .gates
        .iter()
        .enumerate()
        .filter(|(_, gate)| !gate.kind.is_sequential())
        .map(|(index, gate)| (gate.output.as_str(), index))
        .collect();
    let read: std::collections::HashSet<&str> = netlist
        .gates
        .iter()
        .filter(|gate| !gate.kind.is_sequential())
        .flat_map(|gate| gate.inputs.iter().map(String::as_str))
        .collect();
    let sinks = netlist
        .outputs
        .iter()
        .map(String::as_str)
        .chain(
            netlist
                .gates
                .iter()
                .filter(|gate| gate.kind.is_sequential())
                .flat_map(|gate| gate.inputs.iter().map(String::as_str)),
        )
        .chain(
            netlist
                .gates
                .iter()
                .filter(|gate| !gate.kind.is_sequential() && !read.contains(gate.output.as_str()))
                .map(|gate| gate.output.as_str()),
        );
    let mut placed = vec![false; netlist.gates.len()];
    let mut order = Vec::new();
    for sink in sinks {
        let Some(&root) = producer.get(sink) else {
            continue;
        };
        // Iterative post-order: a gate goes out once all its producers have.
        let mut stack = vec![(root, false)];
        while let Some((index, expanded)) = stack.pop() {
            if placed[index] {
                continue;
            }
            if expanded {
                placed[index] = true;
                order.push(index);
                continue;
            }
            stack.push((index, true));
            for input in netlist.gates[index].inputs.iter().rev() {
                if let Some(&producer) = producer.get(input.as_str()) {
                    if !placed[producer] {
                        stack.push((producer, false));
                    }
                }
            }
        }
    }
    order
}

/// Every signal's value at power-on: inputs low, flip-flops holding zero.
fn power_on(netlist: &Netlist, order: &[usize]) -> HashMap<String, bool> {
    let mut value: HashMap<String, bool> = netlist
        .inputs
        .iter()
        .map(|name| (name.clone(), false))
        .collect();
    for gate in netlist
        .gates
        .iter()
        .filter(|gate| gate.kind.is_sequential())
    {
        value.insert(gate.output.clone(), false);
    }
    for &index in order {
        let gate = &netlist.gates[index];
        if !gate.kind.is_sequential() {
            let operands: Vec<bool> = gate
                .inputs
                .iter()
                .map(|input| value.get(input).copied().unwrap_or(false))
                .collect();
            value.insert(gate.output.clone(), gate.kind.evaluate(&operands));
        }
    }
    value
}

/// Every ordering of `0..count` (count is at most three).
fn permutations(count: usize) -> Vec<Vec<usize>> {
    match count {
        2 => vec![vec![0, 1], vec![1, 0]],
        3 => vec![
            vec![0, 1, 2],
            vec![0, 2, 1],
            vec![1, 0, 2],
            vec![1, 2, 0],
            vec![2, 0, 1],
            vec![2, 1, 0],
        ],
        _ => vec![(0..count).collect()],
    }
}

fn distance(a: Anchor, b: Anchor) -> i32 {
    (a.x - b.x).abs() + (a.y - b.y).abs() + (a.z - b.z).abs()
}

/// The square ring of offsets at Chebyshev distance `ring`, in a fixed order.
fn ring_offsets(ring: i32) -> Vec<(i32, i32)> {
    if ring == 0 {
        return vec![(0, 0)];
    }
    let mut out = Vec::with_capacity(8 * ring as usize);
    for d in -ring..=ring {
        out.push((d, -ring));
        out.push((d, ring));
    }
    for d in -ring + 1..ring {
        out.push((-ring, d));
        out.push((ring, d));
    }
    out.sort_by_key(|&(dx, dz)| (dx.abs() + dz.abs(), dz, dx));
    out
}

/// Each element as a box with pins, for dewey's placer.
fn shapes(elements: &[Element], whats: &[What]) -> Vec<dewey::Shape> {
    elements
        .iter()
        .zip(whats)
        .map(|(element, what)| {
            let (low_x, high_x, low_z, high_z) = tile::extent(&element.tile);
            let (low_y, high_y) = element
                .tile
                .cells
                .iter()
                .fold((i32::MAX, i32::MIN), |(low, high), cell| {
                    (low.min(cell.offset.1), high.max(cell.offset.1))
                });
            let output = element.outputs.iter().enumerate().flat_map(|(output, &net)| {
                let source = element.tile.source.iter().find(|source| source.output == output && source.tappable);
                source.map(|source| (net, (source.offset.0, source.offset.2)))
            });
            let inputs = element.tile.stubs.iter().map(|stub| {
                (
                    element.inputs[stub.input],
                    (stub.approach.0, stub.approach.2),
                )
            });
            dewey::Shape {
                low_x,
                high_x,
                low_z,
                high_z,
                height: high_y - low_y + 1,
                pins: output.chain(inputs).collect(),
                side: match what {
                    What::Input(_) => dewey::Side::West,
                    What::Output(_) => dewey::Side::East,
                    What::Gate(_) | What::Block(_) => dewey::Side::Free,
                },
            }
        })
        .collect()
}

fn tile_for(kind: GateKind) -> Option<Tile> {
    match kind {
        GateKind::Nor(arity) if arity <= 3 => Some(tile::nor(arity)),
        GateKind::Or(arity) if (2..=3).contains(&arity) => Some(tile::or(arity)),
        GateKind::DffPosedge => Some(tile::dff()),
        _ => None,
    }
}

/// Generate a world for a lowered netlist (NOR, OR and positive-edge DFF
/// gates only). Its inputs and outputs are ports: each where `places` puts
/// its signal, else wherever placement puts it.
pub fn construct(netlist: &Netlist, places: &BTreeMap<String, PortPlace>) -> Result<Construction, ConstructError> {
    construct_with(netlist, places, Method::Constructive)
}

/// [`construct`], by the given method.
pub fn construct_with(
    netlist: &Netlist,
    places: &BTreeMap<String, PortPlace>,
    method: Method,
) -> Result<Construction, ConstructError> {
    generate(netlist, &[], &Frame::top(netlist, places), false, method).map(|generated| generated.construction)
}

/// A finished block used whole inside a larger construction: its tile, the
/// signals its input ports read and its output ports drive, in port order,
/// and its estimated critical delay in game ticks.
#[derive(Clone)]
pub(super) struct BlockUse {
    pub tile: std::sync::Arc<Tile>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub delay: f64,
}

/// Every element, in the order placement seeds the lattice with: inputs,
/// blocks, flip-flops, gates cone by cone, output ports.
fn whats(netlist: &Netlist, blocks: usize) -> Vec<What> {
    let mut whats: Vec<What> = netlist.inputs.iter().cloned().map(What::Input).collect();
    whats.extend((0..blocks).map(What::Block));
    let sequential =
        (0..netlist.gates.len()).filter(|&index| netlist.gates[index].kind.is_sequential());
    whats.extend(sequential.chain(cone_order(netlist)).map(What::Gate));
    let mut ports = std::collections::HashSet::new();
    for name in &netlist.outputs {
        if ports.insert(name) {
            whats.push(What::Output(name.clone()));
        }
    }
    whats
}

/// Where each input port and each output port in `frame` goes.
fn places(frame: &Frame) -> (HashMap<&str, PortPlace>, HashMap<&str, PortPlace>) {
    fn map(ports: &[(String, PortPlace)]) -> HashMap<&str, PortPlace> {
        ports.iter().map(|(name, place)| (name.as_str(), *place)).collect()
    }
    (map(&frame.inputs), map(&frame.outputs))
}

/// Which edge each element keeps to: the ports on edges to theirs. Ports
/// at cells are fixed points, which [`generate`] fills in.
fn pins(whats: &[What], frame: &Frame) -> Vec<placement::Pin> {
    let (place_in, place_out) = places(frame);
    let edge = |place: PortPlace| match place {
        PortPlace::Edge(side) => placement::Pin::Edge(side),
        PortPlace::Free | PortPlace::Cell(..) => placement::Pin::Free,
    };
    whats
        .iter()
        .map(|what| match what {
            What::Input(name) => edge(place_in[name.as_str()]),
            What::Output(name) => edge(place_out[name.as_str()]),
            _ => placement::Pin::Free,
        })
        .collect()
}

/// The edge a port at a cell faces: away from the middle of what it
/// connects to, whose middles sit at `neighbours` (the port itself at `at`), on a
/// lattice whose rows are `aspect` columns long.
fn facing(at: (f64, f64), neighbours: &[(f64, f64)], aspect: f64, default: Side) -> Side {
    if neighbours.is_empty() {
        return default;
    }
    let count = neighbours.len() as f64;
    let dx = at.0 - neighbours.iter().map(|n| n.0).sum::<f64>() / count;
    let dz = at.1 - neighbours.iter().map(|n| n.1).sum::<f64>() / count;
    match (dx.abs() >= aspect * dz.abs(), dx > 0.0, dz > 0.0) {
        (true, true, _) => Side::East,
        (true, false, _) => Side::West,
        // Slot rows count northward.
        (false, _, true) => Side::North,
        (false, _, false) => Side::South,
    }
}

/// Each of `blocks`' frame, from placements of
/// `netlist` with block `i` a soft block of about `areas[i]` slots, tried at
/// every ratio in [`RATIOS`]: the shape whose placement times shortest, and
/// when each input arrives. `frame` is the netlist's own.
pub(super) fn plan(
    netlist: &Netlist,
    blocks: &[BlockUse],
    areas: &[usize],
    frame: &Frame,
) -> Vec<Frame> {
    let whats = whats(netlist, blocks.len());
    let nets = memberships(netlist, blocks, &whats, &middle);
    let timing = timing_graph(netlist, blocks, &whats, &middle);
    let pins = pins(&whats, frame);
    let order: Vec<usize> = (0..whats.len()).collect();
    let shaped = |ratio: f64| -> Vec<(usize, usize)> {
        whats
            .iter()
            .map(|what| match *what {
                What::Block(index) => {
                    let area = areas[index] as f64;
                    let w = (area * ratio).sqrt().round().max(1.0);
                    (w as usize, (area / w).ceil().max(1.0) as usize)
                }
                _ => (1, 1),
            })
            .collect()
    };
    // Every ratio at once, then the first of the fastest, so the choice is
    // the same however the work was spread.
    let tried: Vec<Tried> = RATIOS
        .par_iter()
        .map(|&ratio| {
            let spans = shaped(ratio);
            let (slots, _, critical) = placement::anneal(&placement::Problem {
                count: whats.len(),
                nets: &nets,
                timing: &timing,
                pins: &pins,
                sizes: &spans,
                shape: frame.shape,
                order: &order,
                rough: true,
            })
            .swap_remove(0);
            (spans, slots, critical)
        })
        .collect();
    let (spans, slots, _) = tried
        .into_iter()
        .min_by(|a, b| a.2.partial_cmp(&b.2).expect("finite delays"))
        .expect("some ratio");
    let centres: Vec<(f64, f64)> = (0..whats.len()).map(|element| centre_of(slots[element], spans[element])).collect();
    frames_of(netlist, blocks, &whats, &timing, &spans, &centres)
}

/// The middle of an element of `span` slots with its corner at `slot`.
fn centre_of((x, z): (i32, i32), (w, h): (usize, usize)) -> (f64, f64) {
    (f64::from(x) + (w - 1) as f64 / 2.0, f64::from(z) + (h - 1) as f64 / 2.0)
}

/// Each of `blocks`' frame, from a placement of their parent: every
/// element `spans` slots with its middle at `centres`. A block's ports lean
/// toward what each one joins, and its inputs arrive as that placement
/// times them.
fn frames_of(
    netlist: &Netlist,
    blocks: &[BlockUse],
    whats: &[What],
    timing: &placement::Timing,
    spans: &[(usize, usize)],
    centres: &[(f64, f64)],
) -> Vec<Frame> {
    let mut joins: HashMap<&str, Vec<usize>> = HashMap::new();
    for (element, what) in whats.iter().enumerate() {
        let (reads, drives) = signals(netlist, blocks, what);
        for signal in reads.into_iter().chain(drives) {
            joins.entry(signal).or_default().push(element);
        }
    }
    // When each element's inputs arrive, by the edges into it from each
    // driver.
    let ready = timing.ready(whats.len(), |edge| timing.delay_at(centres, edge));
    let mut arrives: HashMap<(usize, usize), f64> = HashMap::new();
    for (edge, &(from, to)) in timing.edges.iter().enumerate() {
        let at = ready[from] + timing.delay_at(centres, edge);
        let entry = arrives.entry((from, to)).or_insert(at);
        *entry = entry.max(at);
    }
    let driver: HashMap<&str, usize> = whats
        .iter()
        .enumerate()
        .flat_map(|(element, what)| signals(netlist, blocks, what).1.into_iter().map(move |signal| (signal, element)))
        .collect();
    whats
        .iter()
        .enumerate()
        .filter_map(|(element, what)| match *what {
            What::Block(index) => Some((element, index)),
            _ => None,
        })
        .map(|(element, index)| {
            let (w, h) = spans[element];
            let block = &blocks[index];
            // A block puts its ports wherever suits its own gates, leaning
            // toward the middle of what else each one's signal joins.
            let free = |signal: &String| (signal.clone(), PortPlace::Free);
            let inputs: Vec<(String, PortPlace)> = block.inputs.iter().map(free).collect();
            let outputs: Vec<(String, PortPlace)> = block.outputs.iter().map(free).collect();
            let (x, z) = centres[element];
            let toward = block
                .inputs
                .iter()
                .chain(&block.outputs)
                .filter_map(|signal| {
                    let others: Vec<(f64, f64)> = joins[signal.as_str()]
                        .iter()
                        .filter(|&&other| other != element)
                        .map(|&other| centres[other])
                        .collect();
                    if others.is_empty() {
                        return None;
                    }
                    let count = others.len() as f64;
                    let dx = (others.iter().map(|o| o.0).sum::<f64>() / count - x) / (w as f64 / 2.0).max(0.5);
                    let dz = (others.iter().map(|o| o.1).sum::<f64>() / count - z) / (h as f64 / 2.0).max(0.5);
                    // Onto the block's outline, in quarters.
                    let reach = dx.abs().max(dz.abs()).max(1e-9);
                    let quarter = |d: f64| (d / reach * f64::from(TOWARD_STEPS)).round() as i8;
                    Some((signal.clone(), (quarter(dx), quarter(dz))))
                })
                .collect();
            // Lateness past the earliest input, capped at the block's own
            // delay -- later than that and the input is critical anyway.
            let at: Vec<f64> = block
                .inputs
                .iter()
                .map(|signal| driver.get(signal.as_str()).map_or(0.0, |&from| arrives.get(&(from, element)).copied().unwrap_or(0.0)))
                .collect();
            let earliest = at.iter().copied().fold(f64::INFINITY, f64::min);
            let step = block.delay / ARRIVAL_STEPS;
            let arrivals = block
                .inputs
                .iter()
                .zip(&at)
                .map(|(signal, &at)| {
                    let late = (at - earliest).min(block.delay);
                    let ticks = if step > 0.0 { (late / step).round() * step } else { 0.0 };
                    (signal.clone(), ticks.round() as u32)
                })
                .collect();
            Frame {
                inputs,
                outputs,
                shape: Some((w, h)),
                arrivals,
                toward,
            }
        })
        .collect()
}

/// One ratio's placement in planning: every element's span and slot, and
/// the estimated critical delay.
type Tried = (Vec<(usize, usize)>, Vec<(i32, i32)>, f64);

/// Where a placement puts every element: its target origin, its slot, and
/// whether it is mirrored.
type Placement = (Vec<Anchor>, Vec<(i32, i32)>, Vec<placement::Mirror>);

/// What [`generate`] produced: the world, and -- when generating a block --
/// the block as a tile for its parent, with its estimated critical delay.
pub(super) struct Generated {
    pub construction: Construction,
    pub block: Option<(Tile, f64)>,
}

/// What a parent fixes about a block before it is generated: the edge
/// each port sits on, about how many slots wide and tall it is, and how
/// many ticks after its earliest input each input arrives.
#[derive(Clone, Debug)]
pub(super) struct Frame {
    pub inputs: Vec<(String, PortPlace)>,
    pub outputs: Vec<(String, PortPlace)>,
    /// None leaves the lattice square.
    pub shape: Option<(usize, usize)>,
    pub arrivals: Vec<(String, u32)>,
    /// Which way each port's outside connections lie, as a point on the
    /// block's outline from its middle, in [`TOWARD_STEPS`]ths of its half
    /// width and half height.
    pub toward: Vec<(String, (i8, i8))>,
}

/// How finely planning tells a block which way its ports' connections lie
/// -- coarse, so repeated slices still share one layout.
const TOWARD_STEPS: i8 = 2;

/// Where one port of a circuit goes: anywhere along an edge, or exactly at
/// a cell -- the clear cell outside the port that an input is driven from
/// or an output read at -- in the caller's own coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortPlace {
    /// Wherever placement puts it among the gates, facing away from them.
    Free,
    Edge(Side),
    Cell(i32, i32, i32),
}

impl Frame {
    /// The top's frame: each port where `places` puts its signal, else
    /// free.
    pub fn top(netlist: &Netlist, places: &BTreeMap<String, PortPlace>) -> Frame {
        let place = |name: &String| (name.clone(), places.get(name).copied().unwrap_or(PortPlace::Free));
        Frame {
            inputs: netlist.inputs.iter().map(place).collect(),
            outputs: netlist.outputs.iter().map(place).collect(),
            shape: None,
            arrivals: Vec::new(),
            toward: Vec::new(),
        }
    }
}

/// How finely planning tells a block when its inputs arrive, as a share of
/// the block's own estimated delay -- coarse, so repeated slices of a chain
/// still share one layout.
const ARRIVAL_STEPS: f64 = 4.0;

/// The level a block's ports stand at, over a grid without blocks of its
/// own: above its routes, so its parent reaches them from above.
const PORT_LEVEL: i32 = 12;

/// The aspect ratios, width over height, planning tries its blocks at.
const RATIOS: [f64; 5] = [0.25, 0.5, 1.0, 2.0, 4.0];

/// Generate `netlist` with `blocks` placed whole among its gates, its
/// inputs and outputs ports on the edges `frame` names; with `block`, the
/// result is also a block for a parent.
pub(super) fn generate(
    netlist: &Netlist,
    blocks: &[BlockUse],
    frame: &Frame,
    block: bool,
    method: Method,
) -> Result<Generated, ConstructError> {
    let mut builder = Builder {
        grid: Grid::default(),
        nets: Vec::new(),
        net_of: HashMap::new(),
        branches: Vec::new(),
    };
    for gate in &netlist.gates {
        if tile_for(gate.kind).is_none() {
            return Err(ConstructError::NoTile {
                gate: gate.name.clone(),
                kind: gate.kind,
            });
        }
    }
    let driven: std::collections::HashSet<&str> = netlist
        .inputs
        .iter()
        .map(String::as_str)
        .chain(netlist.gates.iter().map(|gate| gate.output.as_str()))
        .chain(blocks.iter().flat_map(|block| block.outputs.iter().map(String::as_str)))
        .collect();
    for signal in netlist
        .gates
        .iter()
        .flat_map(|gate| gate.inputs.iter())
        .chain(blocks.iter().flat_map(|block| block.inputs.iter()))
        .chain(netlist.outputs.iter())
    {
        if !driven.contains(signal.as_str()) {
            return Err(ConstructError::Undriven {
                signal: signal.clone(),
            });
        }
    }

    if netlist.combinational_order().is_none() {
        return Err(ConstructError::CombinationalLoop);
    }

    let whats = whats(netlist, blocks.len());
    let (place_in, place_out) = places(frame);
    let edge_or = |place: PortPlace, default: Side| match place {
        PortPlace::Edge(side) => side,
        PortPlace::Free | PortPlace::Cell(..) => default,
    };
    let free = |what: &What| match what {
        What::Input(name) => place_in[name.as_str()] == PortPlace::Free,
        What::Output(name) => place_out[name.as_str()] == PortPlace::Free,
        _ => false,
    };

    // Where each goes: annealed slots on a lattice wide enough for any tile.
    // dewey places the elements themselves, further down.
    let mut pins = pins(&whats, frame);
    let slots = (method == Method::Constructive).then_some(());
    let order = netlist.combinational_order().expect("checked above");

    for signal in netlist
        .gates
        .iter()
        .flat_map(|gate| gate.inputs.iter())
        .chain(blocks.iter().flat_map(|block| block.inputs.iter()))
    {
        let net = builder.net(signal);
        builder.nets[net].unconnected += 1;
    }
    for what in &whats {
        if let What::Output(name) = what {
            let net = builder.net(name);
            builder.nets[net].unconnected += 1;
        }
    }

    // Bodies are numbered after the gates; inputs and outputs have none,
    // blocks follow.
    let no_body = netlist.gates.len();
    let external = |builder: &mut Builder| {
        builder.nets.push(Net {
            external: true,
            ..Net::default()
        });
        builder.nets.len() - 1
    };
    let elements: Vec<Element> = whats
        .iter()
        .map(|what| match what {
            What::Input(name) => Element {
                name,
                tile: tile::port_in(edge_or(place_in[name.as_str()], Side::West), block),
                inputs: vec![external(&mut builder)],
                commutative: false,
                outputs: vec![builder.net(name)],
                body: no_body,
            },
            &What::Gate(index) => {
                let gate = &netlist.gates[index];
                Element {
                    name: &gate.name,
                    tile: tile_for(gate.kind).unwrap(),
                    inputs: gate
                        .inputs
                        .iter()
                        .map(|signal| builder.net(signal))
                        .collect(),
                    commutative: !gate.kind.is_sequential(),
                    outputs: vec![builder.net(&gate.output)],
                    body: index,
                }
            }
            &What::Block(index) => {
                let block = &blocks[index];
                Element {
                    name: "a block",
                    tile: (*block.tile).clone(),
                    inputs: block.inputs.iter().map(|signal| builder.net(signal)).collect(),
                    commutative: false,
                    outputs: block.outputs.iter().map(|signal| builder.net(signal)).collect(),
                    body: no_body + 1 + index,
                }
            }
            What::Output(name) => Element {
                name,
                tile: tile::port_out(edge_or(place_out[name.as_str()], Side::East), block),
                inputs: vec![builder.net(name)],
                commutative: false,
                outputs: vec![external(&mut builder)],
                body: no_body,
            },
        })
        .collect();
    // The lattice is sized for the gates and ports; a block spans as many
    // slots as it needs.
    let small = |element: &&Element| element.tile.cells.len() < BLOCK_CELLS;
    let (low_x, high_x, low_z, high_z) = elements
        .iter()
        .filter(small)
        .map(|element| tile::extent(&element.tile))
        .fold(
            (i32::MAX, i32::MIN, i32::MAX, i32::MIN),
            |(a, b, c, d), (lx, hx, lz, hz)| (a.min(lx), b.max(hx), c.min(lz), d.max(hz)),
        );
    let (low_x, high_x, low_z, high_z) = if low_x > high_x { (0, 0, 0, 0) } else { (low_x, high_x, low_z, high_z) };
    let pitch_x = high_x - low_x + 2 * MOAT + 2;
    let pitch_z = high_z - low_z + 2 * MOAT + 2;
    let sizes: Vec<(usize, usize)> = elements
        .iter()
        .map(|element| {
            if small(&element) {
                return (1, 1);
            }
            let (lx, hx, lz, hz) = tile::extent(&element.tile);
            let span = |extent: i32, pitch: i32| ((extent + 2 * MOAT + 2 + pitch - 1) / pitch).max(1) as usize;
            (span(hx - lx + 1, pitch_x), span(hz - lz + 1, pitch_z))
        })
        .collect();
    // A block's signals attach at its ports, in slots from its middle.
    let attach = |element: usize, signal: &str| -> placement::Offset {
        let What::Block(index) = whats[element] else { return (0.0, 0.0) };
        let block = &blocks[index];
        let tile = &elements[element].tile;
        let port = block.inputs.iter().chain(&block.outputs).position(|port| port == signal);
        let Some(&(x, _, z)) = port.and_then(|port| tile.ports.get(port)) else { return (0.0, 0.0) };
        let (lx, hx, lz, hz) = tile::extent(tile);
        (
            (f64::from(x) - f64::from(lx + hx) / 2.0) / f64::from(pitch_x),
            -(f64::from(z) - f64::from(lz + hz) / 2.0) / f64::from(pitch_z),
        )
    };
    let mut timing = timing_graph(netlist, blocks, &whats, &attach);
    timing.aspect = f64::from(pitch_z) / f64::from(pitch_x);
    let arrival: HashMap<&str, u32> = frame.arrivals.iter().map(|(port, ticks)| (port.as_str(), *ticks)).collect();
    for (element, what) in whats.iter().enumerate() {
        if let What::Input(name) = what {
            timing.arrival[element] = f64::from(arrival.get(name.as_str()).copied().unwrap_or(0));
        }
    }
    // What the block adds to its parent's paths leaves its inputs' lateness
    // out: the parent counts that itself.
    let mut bare = timing.clone();
    bare.arrival = vec![0.0; whats.len()];
    // Ports at given cells: those cells, moved so the lowest stands at the
    // base level and their middle sits over the lattice's, become fixed
    // points among the slots. `shift` takes the caller's coordinates to
    // ours.
    let cell_of = |what: &What| match what {
        What::Input(name) => match place_in[name.as_str()] {
            PortPlace::Cell(x, y, z) => Some(Anchor { x, y, z }),
            PortPlace::Free | PortPlace::Edge(_) => None,
        },
        What::Output(name) => match place_out[name.as_str()] {
            PortPlace::Cell(x, y, z) => Some(Anchor { x, y, z }),
            PortPlace::Free | PortPlace::Edge(_) => None,
        },
        _ => None,
    };
    let cells: Vec<Anchor> = whats.iter().filter_map(cell_of).collect();
    let shift = (!cells.is_empty()).then(|| {
        let mean = |axis: fn(&Anchor) -> i32| {
            (cells.iter().map(|cell| f64::from(axis(cell))).sum::<f64>() / cells.len() as f64).round() as i32
        };
        // The lattice is about square in cells (see `placement`).
        let half = ((whats.len() - cells.len()) as f64 * 1.15 * f64::from(pitch_x * pitch_z)).sqrt() / 2.0;
        let half = half.round() as i32;
        Anchor {
            x: mean(|cell| cell.x) - half,
            y: cells.iter().map(|cell| cell.y).min().expect("some cell") - BASE,
            z: mean(|cell| cell.z) + half,
        }
    });
    let ours = |cell: Anchor| {
        let shift = shift.expect("cells give a shift");
        Anchor {
            x: cell.x - shift.x,
            y: cell.y - shift.y,
            z: cell.z - shift.z,
        }
    };
    let point = |cell: Anchor| {
        let at = ours(cell);
        (f64::from(at.x) / f64::from(pitch_x), -f64::from(at.z) / f64::from(pitch_z))
    };
    for (pin, what) in pins.iter_mut().zip(&whats) {
        if let Some(cell) = cell_of(what) {
            let (x, z) = point(cell);
            *pin = placement::Pin::Point(x, z);
        }
    }
    if let Some(highest) = cells.iter().map(|&cell| ours(cell).y).max() {
        builder.grid.raise = (highest + 6 - route::HIGHEST).max(0);
    }
    // Routes travel over the blocks: from two levels clear of the tallest
    // up, which a route standing beside nothing foreign needs, through as
    // many levels as a grid without blocks has.
    let tallest = blocks
        .iter()
        .filter_map(|block| block.tile.cells.iter().map(|cell| BASE + cell.offset.1).max())
        .max();
    if let Some(tallest) = tallest {
        builder.grid.lift = (tallest + 3 - route::OPEN).max(0);
        builder.grid.raise = builder.grid.raise.max(builder.grid.lift);
    }
    // A block's own routes keep below its ports, which keep below its
    // parent's.
    let port_level = PORT_LEVEL + builder.grid.lift;
    if block {
        builder.grid.raise = port_level - route::HIGHEST;
    }
    if !cells.is_empty() && method != Method::Constructive {
        return Err(ConstructError::Unplaceable {
            element: "the ports".to_string(),
            reason: "only the constructive generator places ports at given cells".to_string(),
        });
    }
    // A block's ports lean toward their outside connections: each is tied
    // to a fixed point just past the lattice's outline that way.
    let mut nets = memberships(netlist, blocks, &whats, &attach);
    let (mut leaning, mut lean_pins, mut lean_sizes) = (timing.clone(), pins.clone(), sizes.clone());
    if let Some((w, h)) = frame.shape {
        let toward: HashMap<&str, (i8, i8)> = frame.toward.iter().map(|(port, at)| (port.as_str(), *at)).collect();
        for (element, what) in whats.iter().enumerate() {
            let (What::Input(name) | What::Output(name)) = what else { continue };
            let Some(&(dx, dz)) = toward.get(name.as_str()) else { continue };
            // The shape is in square slots; the lattice's rows may be
            // shorter.
            let past = |d: i8, span: f64| (span - 1.0) / 2.0 + f64::from(d) / f64::from(TOWARD_STEPS) * (span / 2.0 + 1.0);
            nets.push(vec![(element, (0.0, 0.0)), (lean_pins.len(), (0.0, 0.0))]);
            lean_pins.push(placement::Pin::Point(past(dx, w as f64), past(dz, h as f64 / timing.aspect)));
            lean_sizes.push((1, 1));
            leaning.registered.push(false);
            leaning.intrinsic.push(0.0);
            leaning.arrival.push(0.0);
        }
    }
    let slots = slots.map(|_| {
        let mut runs = placement::anneal(&placement::Problem {
            count: lean_pins.len(),
            nets: &nets,
            timing: &leaning,
            pins: &lean_pins,
            sizes: &lean_sizes,
            shape: frame.shape,
            order: &(0..lean_pins.len()).collect::<Vec<_>>(),
            rough: false,
        });
        for (slots, mirrors, _) in &mut runs {
            slots.truncate(whats.len());
            mirrors.truncate(whats.len());
        }
        runs
    });
    // A slot's origin; a block's corner goes to the slot's corner, its
    // lowest row the one furthest north.
    let origin_at = |element: &Element, size: (usize, usize), (slot_x, slot_z): (i32, i32)| {
        if size == (1, 1) {
            return Anchor {
                x: slot_x * pitch_x,
                y: BASE,
                z: -slot_z * pitch_z,
            };
        }
        let (lx, _, lz, _) = tile::extent(&element.tile);
        Anchor {
            x: slot_x * pitch_x + low_x - lx,
            y: BASE,
            z: -(slot_z + size.1 as i32 - 1) * pitch_z + low_z - lz,
        }
    };
    let placements: Vec<Placement> = match slots {
        Some(runs) => runs
            .into_iter()
            .map(|(slots, mirrors, _)| {
                let targets = elements
                    .iter()
                    .zip(&sizes)
                    .zip(&slots)
                    .map(|((element, &size), &slot)| origin_at(element, size, slot))
                    .collect();
                (targets, slots, mirrors)
            })
            .collect(),
        None => {
            let targets: Vec<Anchor> = dewey::place(&shapes(&elements, &whats))
                .into_iter()
                .map(|(x, z)| Anchor { x, y: BASE, z })
                .collect();
            let slots = targets.iter().map(|at| (at.x / pitch_x, at.z / pitch_z)).collect();
            vec![(targets, slots, vec![(false, false); elements.len()])]
        }
    };

    // Place and route each placement in turn, best first, until one routes.
    let fresh = builder;
    // The first placement in the list that has routed: those after it need
    // not finish.
    let routed_first = AtomicUsize::new(usize::MAX);
    let attempt = |(index, (targets, slots, mirrors)): (usize, &Placement)| -> Result<(Builder, Ports), ConstructError> {
        // Blocks turned over the way placement chose.
        let elements: Vec<Element> = elements
            .iter()
            .zip(mirrors)
            .map(|(element, &mirror)| Element {
                tile: tile::mirror(&element.tile, mirror),
                ..element.clone()
            })
            .collect();
        let timing = timing.mirrored(mirrors);
        let bare = bare.mirrored(mirrors);
        let mut builder = fresh.clone();
        let mut placed = Ports::default();
        let mut tiles: Vec<Tile> = elements.iter().map(|element| element.tile.clone()).collect();
        // Ports at given cells go first, exactly there, facing away from
        // what they join.
        // Where an element's middle is, in the slot units a port at a cell
        // is placed in: its slot's corner is its origin, not its middle.
        let middle = |index: usize| {
            let (x, z) = centre_of(slots[index], sizes[index]);
            (
                x + f64::from(low_x + high_x) / 2.0 / f64::from(pitch_x),
                z - f64::from(low_z + high_z) / 2.0 / f64::from(pitch_z),
            )
        };
        // Where each connection of an element meets what is at its other
        // end: a block's port, not its middle.
        let neighbours = |index: usize| -> Vec<(f64, f64)> {
            let plus = |(x, z): (f64, f64), (dx, dz): placement::Offset| (x + dx, z + dz);
            timing
                .edges
                .iter()
                .zip(&timing.ends)
                .filter_map(|(&(from, to), &(start, end))| match (from == index, to == index) {
                    (true, false) => Some(plus(middle(to), end)),
                    (false, true) => Some(plus(middle(from), start)),
                    _ => None,
                })
                .collect()
        };
        // Port element `index`, at `at`, turned to face away from what it
        // joins.
        let turned = |index: usize, at: (f64, f64)| {
            let input = matches!(whats[index], What::Input(_));
            let side = facing(at, &neighbours(index), timing.aspect, if input { Side::West } else { Side::East });
            let mut element = elements[index].clone();
            element.tile = if input { tile::port_in(side, block) } else { tile::port_out(side, block) };
            (element, side)
        };
        let mut fixed: Vec<Option<(Anchor, Side)>> = vec![None; elements.len()];
        for (index, what) in whats.iter().enumerate() {
            let Some(cell) = cell_of(what) else { continue };
            let input = matches!(what, What::Input(_));
            let (mut element, side) = turned(index, point(cell));
            let ((dx, _, dz), _) = tile::across(side);
            let reach = if input { 3 } else { 2 };
            let at = ours(cell);
            let origin = Anchor {
                x: at.x - reach * dx,
                y: at.y,
                z: at.z - reach * dz,
            };
            builder.assign_stubs(&mut element, origin);
            let blocked = |reason: String| ConstructError::Unplaceable {
                element: element.name.to_string(),
                reason,
            };
            if !builder.fits(&element, origin) {
                return Err(blocked(format!("its port does not fit at the cell given, {cell:?}")));
            }
            builder.try_place(&element, origin).map_err(blocked)?;
            fixed[index] = Some((origin, side));
            tiles[index] = element.tile;
        }
        for (index, ((element, what), &target)) in elements.iter().zip(&whats).zip(targets).enumerate() {
            let (origin, side) = match fixed[index] {
                Some((origin, side)) => (origin, Some(side)),
                None if free(what) => {
                    let (turned, side) = turned(index, middle(index));
                    // Over blocks, ports stand by the blocks' own.
                    let level = if block || !blocks.is_empty() { port_level } else { target.y };
                    let origin = builder.place_around(&turned, Anchor { y: level, ..target })?;
                    tiles[index] = turned.tile;
                    (origin, Some(side))
                }
                None => (builder.place_around(element, target)?, None),
            };
            placed.origins.push(origin);
            // Ports keep their ways in and out, and a cell round them, as
            // air no route of its own may take: whatever drives or reads a
            // port from outside -- a block's parent coming in from above, a
            // lever, a lamp -- stands there beside nothing else.
            if matches!(what, What::Input(_) | What::Output(_)) {
                let (net, way): (usize, Vec<(i32, i32, i32)>) = match what {
                    What::Input(_) => (element.inputs[0], tiles[index].stubs.iter().flat_map(|stub| stub.entry.clone()).collect()),
                    _ => (element.outputs[0], tiles[index].exits[0].clone()),
                };
                for &offset in &way {
                    let at = tile::at(origin, offset);
                    for (dx, dz) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dz| (dx, dz))) {
                        let around = Anchor { x: at.x + dx, z: at.z + dz, ..at };
                        if builder.grid.is_free(around) {
                            builder.grid.place(around, Owner::Net(net), None);
                        }
                    }
                }
            }
            match *what {
                What::Input(ref name) => {
                    // Beyond the stub's approach, outside the edge.
                    let side = side.unwrap_or_else(|| edge_or(place_in[name.as_str()], Side::West));
                    let (out, _) = tile::across(side);
                    let drive = tile::at(origin, (3 * out.0, 0, 3 * out.2));
                    placed.inputs.insert(name.clone(), drive);
                }
                What::Gate(index) => {
                    let gate = &netlist.gates[index];
                    if let Some(probe) = element.tile.probe {
                        placed
                            .gate_outputs
                            .insert(gate.output.clone(), tile::at(origin, probe));
                    }
                    if gate.kind.is_sequential() {
                        placed.flip_flops.push((index, origin));
                    }
                }
                // A block's ports are read as `signal@block`.
                What::Block(index) => {
                    let block = &blocks[index];
                    for (signal, &probe) in block.inputs.iter().chain(&block.outputs).zip(&element.tile.ports) {
                        placed.gate_outputs.insert(format!("{signal}@{index}"), tile::at(origin, probe));
                    }
                }
                What::Output(ref name) => {
                    // In line past the port's outer dust.
                    let side = side.unwrap_or_else(|| edge_or(place_out[name.as_str()], Side::East));
                    let (out, _) = tile::across(side);
                    placed.outputs.insert(name.clone(), tile::at(origin, (2 * out.0, 0, 2 * out.2)));
                }
            }
        }

        // Criticality from where the elements went, in slot units.
        let centres: Vec<(f64, f64)> = slots
            .iter()
            .zip(&sizes)
            .map(|(&(x, z), &(w, h))| (f64::from(x) + (w - 1) as f64 / 2.0, f64::from(z) + (h - 1) as f64 / 2.0))
            .collect();
        for (edge, criticality) in timing.criticality_at(&centres).into_iter().enumerate() {
            for &net in &elements[timing.edges[edge].0].outputs {
                let net = &mut builder.nets[net];
                net.criticality = net.criticality.max(criticality);
            }
        }
        placed.critical = bare.critical_at(&centres);

        let routed = match method {
            Method::Constructive | Method::DeweyPlace => builder.route_all(|| routed_first.load(Ordering::Relaxed) < index),
            Method::Dewey => dewey::route(&mut builder),
        };
        routed.map_err(|reason| ConstructError::Unplaceable {
            element: "the routes".to_string(),
            reason,
        })?;
        placed.tiles = tiles;
        routed_first.fetch_min(index, Ordering::Relaxed);
        Ok((builder, placed))
    };
    // Route a wave of placements at once -- routing one leaves most cores
    // idle -- and keep the best that routes, as trying them in turn would.
    // Blocks are generated side by side already, and their first placement
    // nearly always routes, so they try theirs in turn.
    let mut failure = None;
    let mut done = None;
    let numbered: Vec<(usize, &Placement)> = placements.iter().enumerate().collect();
    for wave in numbered.chunks(if block { 1 } else { WAVE }) {
        let results: Vec<_> = wave.par_iter().map(|&numbered| attempt(numbered)).collect();
        for result in results {
            match result {
                Ok(result) => {
                    done = Some(result);
                    break;
                }
                Err(error) => failure = Some(error),
            }
        }
        if done.is_some() {
            break;
        }
    }
    let Some((builder, placed)) = done else {
        return Err(failure.expect("at least one placement"));
    };
    let block = block.then(|| (block_tile(&builder, &elements, &placed.tiles, &placed.origins), placed.critical));
    let Ports {
        inputs,
        gate_outputs,
        outputs,
        flip_flops,
        ..
    } = placed;

    // Every component starts in the state it settles to at power-on (all
    // inputs low, every flip-flop holding zero), so loading the world fires
    // no transient -- above all no clock edge into a flip-flop. Signals
    // only blocks drive start low here.
    let value = power_on(netlist, &order);
    let net_value: HashMap<usize, bool> = builder
        .net_of
        .iter()
        .map(|(signal, &net)| (net, value.get(signal).copied().unwrap_or(false)))
        .collect();
    let mut cells: CellMap<Anchor, BlockState> = builder
        .grid
        .blocks()
        .map(|(at, owner, block)| {
            let mut block = block.clone();
            if let Owner::Net(net) = owner {
                if matches!(
                    block.kind,
                    BlockKind::Repeater | BlockKind::WallTorch | BlockKind::Lamp
                ) {
                    block.lit = net_value.get(&net).copied().unwrap_or(false);
                }
            }
            (at, block)
        })
        .collect();
    let level = |signal: &String| value.get(signal).copied().unwrap_or(false);
    for (index, origin) in flip_flops {
        let gate = &netlist.gates[index];
        let (d, c) = (level(&gate.inputs[0]), level(&gate.inputs[1]));
        for (offset, lit) in [
            (tile::DFF_M_DATA, !c && d),
            (tile::DFF_S_DATA, false),
            (tile::DFF_M_LOCK, c),
            (tile::DFF_S_LOCK, !c),
            (tile::DFF_INVERTED_CLOCK, !c),
        ] {
            cells
                .get_mut(&tile::at(origin, offset))
                .expect("a placed DFF tile")
                .lit = lit;
        }
    }
    let mut blocks: Vec<(Anchor, BlockState)> = cells.into_iter().collect();
    blocks.sort_by_key(|(at, _)| *at);
    let mut bench = Vec::new();
    for at in inputs.values() {
        bench.push((Anchor { y: at.y - 1, ..*at }, super::stone()));
        bench.push((*at, super::lever(false)));
    }
    for at in outputs.values() {
        bench.push((*at, super::lamp()));
    }
    Ok(Generated {
        construction: Construction {
            blocks,
            inputs,
            outputs,
            bench,
            shift,
            gate_outputs,
        },
        block,
    })
}

/// A generated block as one tile for its parent. Everything it placed --
/// reserved air too -- is its body, except its ports: each input port's
/// stub is the parent's input net `k`, each output port's dust the parent's
/// output net `k`, with the stubs' ways in and the outputs' ways out kept
/// clear. Offsets run from its lowest corner at the base level.
fn block_tile(builder: &Builder, elements: &[Element], tiles: &[Tile], origins: &[Anchor]) -> Tile {
    let mut inputs = 0;
    let mut outputs = 0;
    let mut role_of: HashMap<usize, Role> = HashMap::new();
    let mut stubs = Vec::new();
    let mut exits = Vec::new();
    let mut source = Vec::new();
    let mut ins = Vec::new();
    let mut outs = Vec::new();
    for ((element, tile), &origin) in elements.iter().zip(tiles).zip(origins) {
        let port_in = element.inputs.first().is_some_and(|&net| builder.nets[net].external);
        let port_out = element.outputs.first().is_some_and(|&net| builder.nets[net].external);
        if port_in {
            role_of.insert(element.inputs[0], Role::Input(inputs));
            for stub in &tile.stubs {
                stubs.push((inputs, origin, stub.clone()));
            }
            ins.extend(tile.probe.map(|probe| tile::at(origin, probe)));
            inputs += 1;
        }
        if port_out {
            role_of.insert(element.outputs[0], Role::Output(outputs));
            exits.push((origin, tile.exits[0].clone()));
            for cell in tile.source.iter().filter(|cell| cell.output == 0) {
                source.push((outputs, tile::at(origin, cell.offset), cell.clone()));
            }
            outs.extend(tile.probe.map(|probe| tile::at(origin, probe)));
            outputs += 1;
        }
    }
    // The air kept round the ports' ways in and out is the parent's to
    // route through.
    let cells: Vec<(Anchor, Role, Option<BlockState>)> = builder
        .grid
        .cells()
        .filter(|&(_, owner, block)| !(block.is_none() && matches!(owner, Owner::Net(net) if builder.nets[net].external)))
        .map(|(at, owner, block)| {
            let role = match owner {
                Owner::Net(net) => role_of.get(&net).copied().unwrap_or(Role::Body),
                Owner::Gate(_) => Role::Body,
            };
            (at, role, block.cloned())
        })
        .collect();
    let low_x = cells.iter().map(|(at, _, _)| at.x).min().unwrap_or(0);
    let low_z = cells.iter().map(|(at, _, _)| at.z).min().unwrap_or(0);
    let offset = |at: Anchor| (at.x - low_x, at.y - BASE, at.z - low_z);
    let shift = |(x, y, z): (i32, i32, i32), origin: Anchor| offset(tile::at(origin, (x, y, z)));
    Tile {
        cells: cells
            .into_iter()
            .map(|(at, role, block)| tile::TileCell {
                offset: offset(at),
                role,
                block,
            })
            .collect(),
        exits: exits
            .into_iter()
            .map(|(origin, exit)| exit.into_iter().map(|cell| shift(cell, origin)).collect())
            .collect(),
        source: source
            .into_iter()
            .map(|(output, at, cell)| tile::SourceCell {
                output,
                offset: offset(at),
                strength: cell.strength,
                tappable: cell.tappable,
            })
            .collect(),
        stubs: stubs
            .into_iter()
            .map(|(input, origin, stub)| tile::Stub {
                input,
                approach: shift(stub.approach, origin),
                entry: stub.entry.iter().map(|&cell| shift(cell, origin)).collect(),
                cells: stub.cells.iter().map(|&cell| shift(cell, origin)).collect(),
                need: stub.need,
            })
            .collect(),
        probe: None,
        ports: ins.into_iter().chain(outs).map(offset).collect(),
    }
}
