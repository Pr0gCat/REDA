//! Constructive generation: place one netlist element at a time, route its
//! connections at once, and freeze both.
//!
//! Elements are placed in dependency order -- primary inputs, flip-flops,
//! then every combinational gate after the gates feeding it, then the output
//! lamps. Placing an element means trying positions near the things it
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
mod placement;
mod route;
mod tile;

use std::collections::{BTreeMap, HashMap};

use rayon::prelude::*;

use grid::{CellMap, Grid, Owner};
use route::Goal;
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
/// Positions that fit but whose routes failed, tried before giving up.
const ATTEMPTS: usize = 24;
/// How many rip-ups routing may spend per branch it lays, and how often one
/// branch may be retried before routing gives up.
const RIPS_PER_GOAL: usize = 4;
const TRIES: usize = 8;
/// How finely routing order tells criticalities apart.
const CRITICALITY_BANDS: f64 = 10.0;
/// How many route searches one batch holds. Fixed, not the core count, so
/// every machine lays the same routes; rayon spreads a batch over the
/// threads there are.
const WORKERS: usize = 16;
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
    /// Each primary input's lever.
    pub inputs: BTreeMap<String, Anchor>,
    /// Each primary output's lamp.
    pub outputs: BTreeMap<String, Anchor>,
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
    output: Option<usize>,
    body: usize,
}

impl Construction {
    /// The blocks as a world whose lowest corner is the origin, and the port
    /// and probe positions moved with them.
    pub fn into_world(self) -> Placed {
        let low = self
            .blocks
            .iter()
            .fold(None, |low: Option<Anchor>, (at, _)| {
                Some(low.map_or(*at, |low| Anchor {
                    x: low.x.min(at.x),
                    y: low.y.min(at.y),
                    z: low.z.min(at.z),
                }))
            });
        let low = low.unwrap_or(Anchor { x: 0, y: 0, z: 0 });
        let shift = |at: &Anchor| (at.x - low.x, at.y - low.y, at.z - low.z);
        let size = self.blocks.iter().fold((1, 1, 1), |size, (at, _)| {
            let (x, y, z) = shift(at);
            (size.0.max(x + 1), size.1.max(y + 1), size.2.max(z + 1))
        });
        let mut world = World::new(size.0, size.1, size.2);
        for (at, block) in self.blocks {
            let (x, y, z) = shift(&at);
            world.set(x, y, z, block);
        }
        let moved = |ports: BTreeMap<String, Anchor>| {
            ports
                .iter()
                .map(|(name, at)| (name.clone(), shift(at)))
                .collect()
        };
        Placed {
            world,
            inputs: moved(self.inputs),
            outputs: moved(self.outputs),
            gate_outputs: moved(self.gate_outputs),
        }
    }
}

/// A [`Construction`] as a world.
pub struct Placed {
    pub world: World,
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
            Role::Output => Owner::Net(
                element
                    .output
                    .expect("a tile with output cells drives a net"),
            ),
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
            .exit
            .iter()
            .map(|&offset| (offset, Role::Output))
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
            .chain(element.output)
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
        if let Some(net) = element.output {
            let exit: Vec<Anchor> = element
                .tile
                .exit
                .iter()
                .map(|&offset| tile::at(origin, offset))
                .collect();
            for &at in &exit {
                self.grid.keep_clear(at, Owner::Net(net));
            }
            self.nets[net].exit = exit;
        }
        if let Some(net) = element.output {
            let taps: Vec<(Anchor, u8)> = element
                .tile
                .source
                .iter()
                .filter(|source| source.tappable)
                .map(|source| (tile::at(origin, source.offset), source.strength))
                .collect();
            self.nets[net].home = Some(origin);
            self.nets[net].taps = taps;
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
            };
            for &at in &goal.clear {
                self.grid.keep_clear(at, Owner::Net(net));
            }
            self.nets[net].waiting.push(goal);
        }
        Ok(())
    }

    /// Lay every net, once everything is placed: the shortest first, each
    /// net's sinks nearest its driver first. A branch whose search is walled
    /// in rips up the branch walling it in most and goes again; what was
    /// ripped goes to the back of the queue.
    fn route_all(&mut self) -> Result<(), String> {
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
                if position > WORKERS {
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
                        &goal,
                        branch,
                    ),
                    Ok(steps) => match route::place(&mut self.grid, owner, &steps, &goal, branch) {
                        Some(laid) => Ok(laid),
                        None => route::lay(
                            &mut self.grid,
                            owner,
                            &self.nets[net].taps,
                            &self.nets[net].start(),
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
                        let victim = (rips > 0 && *tried <= TRIES)
                            .then(|| self.blocker(net, &walled))
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
        let at = |x, z| Anchor { x, y: BASE, z };
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
    Output(String),
}

/// The signals an element reads and drives.
fn signals<'a>(netlist: &'a Netlist, what: &'a What) -> (Vec<&'a str>, Option<&'a str>) {
    match what {
        What::Input(name) => (Vec::new(), Some(name)),
        &What::Gate(index) => {
            let gate = &netlist.gates[index];
            (
                gate.inputs.iter().map(String::as_str).collect(),
                Some(&gate.output),
            )
        }
        What::Output(name) => (vec![name.as_str()], None),
    }
}

/// Each net as the elements it joins.
fn memberships(netlist: &Netlist, whats: &[What]) -> Vec<Vec<usize>> {
    let mut members: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, what) in whats.iter().enumerate() {
        let (reads, drives) = signals(netlist, what);
        for signal in reads.into_iter().chain(drives) {
            let list = members.entry(signal).or_default();
            if list.last() != Some(&index) {
                list.push(index);
            }
        }
    }
    members.into_values().collect()
}

/// Where a placement put the ports, probes and flip-flops.
#[derive(Default)]
struct Ports {
    inputs: BTreeMap<String, Anchor>,
    gate_outputs: BTreeMap<String, Anchor>,
    outputs: BTreeMap<String, Anchor>,
    flip_flops: Vec<(usize, Anchor)>,
}

/// Every driver-to-reader connection between elements, and which elements
/// start and end timing paths on their own (flip-flops).
fn timing_graph(netlist: &Netlist, whats: &[What]) -> placement::Timing {
    let mut driver: HashMap<&str, usize> = HashMap::new();
    for (index, what) in whats.iter().enumerate() {
        if let (_, Some(signal)) = signals(netlist, what) {
            driver.insert(signal, index);
        }
    }
    let mut edges = Vec::new();
    for (index, what) in whats.iter().enumerate() {
        for signal in signals(netlist, what).0 {
            if let Some(&from) = driver.get(signal) {
                edges.push((from, index));
            }
        }
    }
    let registered = whats
        .iter()
        .map(|what| matches!(what, &What::Gate(index) if netlist.gates[index].kind.is_sequential()))
        .collect();
    placement::Timing { edges, registered }
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
            let operands: Vec<bool> = gate.inputs.iter().map(|input| value[input]).collect();
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
            let output = element.output.into_iter().flat_map(|net| {
                let source = element.tile.source.iter().find(|source| source.tappable);
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
                    What::Gate(_) => dewey::Side::Free,
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
/// gates only).
pub fn construct(netlist: &Netlist) -> Result<Construction, ConstructError> {
    construct_with(netlist, Method::Constructive)
}

/// [`construct`], by the given method.
pub fn construct_with(netlist: &Netlist, method: Method) -> Result<Construction, ConstructError> {
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
        .collect();
    for signal in netlist
        .gates
        .iter()
        .flat_map(|gate| gate.inputs.iter())
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

    // Every element, in the order placement seeds the lattice with: inputs,
    // flip-flops, gates cone by cone, output lamps.
    let mut whats: Vec<What> = netlist.inputs.iter().cloned().map(What::Input).collect();
    let sequential =
        (0..netlist.gates.len()).filter(|&index| netlist.gates[index].kind.is_sequential());
    whats.extend(sequential.chain(cone_order(netlist)).map(What::Gate));
    let mut lamps = std::collections::HashSet::new();
    for name in &netlist.outputs {
        if lamps.insert(name) {
            whats.push(What::Output(name.clone()));
        }
    }

    // Where each goes: annealed slots on a lattice wide enough for any tile.
    // dewey places the elements themselves, further down.
    let timing = timing_graph(netlist, &whats);
    let slots = (method == Method::Constructive).then(|| {
        placement::anneal(
            whats.len(),
            &memberships(netlist, &whats),
            &timing,
            &(0..whats.len()).collect::<Vec<_>>(),
        )
    });
    let order = netlist.combinational_order().expect("checked above");

    for gate in &netlist.gates {
        for signal in &gate.inputs {
            let net = builder.net(signal);
            builder.nets[net].unconnected += 1;
        }
    }
    for what in &whats {
        if let What::Output(name) = what {
            let net = builder.net(name);
            builder.nets[net].unconnected += 1;
        }
    }

    // Bodies are numbered after the gates; inputs and outputs have none.
    let no_body = netlist.gates.len();
    let elements: Vec<Element> = whats
        .iter()
        .map(|what| match what {
            What::Input(name) => Element {
                name,
                tile: tile::input(),
                inputs: Vec::new(),
                commutative: false,
                output: Some(builder.net(name)),
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
                    output: Some(builder.net(&gate.output)),
                    body: index,
                }
            }
            What::Output(name) => Element {
                name,
                tile: tile::output(),
                inputs: vec![builder.net(name)],
                commutative: false,
                output: None,
                body: no_body,
            },
        })
        .collect();
    let (low_x, high_x, low_z, high_z) = elements
        .iter()
        .map(|element| tile::extent(&element.tile))
        .fold(
            (i32::MAX, i32::MIN, i32::MAX, i32::MIN),
            |(a, b, c, d), (lx, hx, lz, hz)| (a.min(lx), b.max(hx), c.min(lz), d.max(hz)),
        );
    let pitch_x = high_x - low_x + 2 * MOAT + 2;
    let pitch_z = high_z - low_z + 2 * MOAT + 2;
    let placements: Vec<Vec<Anchor>> = match slots {
        Some(runs) => runs
            .iter()
            .map(|slots| {
                slots
                    .iter()
                    .map(|&(slot_x, slot_z)| Anchor {
                        x: slot_x * pitch_x,
                        y: BASE,
                        z: -slot_z * pitch_z,
                    })
                    .collect()
            })
            .collect(),
        None => vec![dewey::place(&shapes(&elements, &whats))
            .into_iter()
            .map(|(x, z)| Anchor { x, y: BASE, z })
            .collect()],
    };

    // Place and route each placement in turn, best first, until one routes.
    let fresh = builder;
    let attempt = |targets: &[Anchor]| -> Result<(Builder, Ports), ConstructError> {
        let mut builder = fresh.clone();
        let mut ports = Ports::default();
        for ((element, what), &target) in elements.iter().zip(&whats).zip(targets) {
            let origin = builder.place_around(element, target)?;
            match *what {
                What::Input(ref name) => {
                    ports.inputs.insert(name.clone(), origin);
                }
                What::Gate(index) => {
                    let gate = &netlist.gates[index];
                    if let Some(probe) = element.tile.probe {
                        ports
                            .gate_outputs
                            .insert(gate.output.clone(), tile::at(origin, probe));
                    }
                    if gate.kind.is_sequential() {
                        ports.flip_flops.push((index, origin));
                    }
                }
                What::Output(ref name) => {
                    ports
                        .outputs
                        .insert(name.clone(), tile::at(origin, (0, -1, 0)));
                }
            }
        }

        // Criticality from where the elements went, in slot units.
        let slots: Vec<(i32, i32)> = targets
            .iter()
            .map(|at| (at.x / pitch_x, at.z / pitch_z))
            .collect();
        for (edge, criticality) in timing.criticality_at(&slots).into_iter().enumerate() {
            if let Some(net) = elements[timing.edges[edge].0].output {
                let net = &mut builder.nets[net];
                net.criticality = net.criticality.max(criticality);
            }
        }

        let routed = match method {
            Method::Constructive | Method::DeweyPlace => builder.route_all(),
            Method::Dewey => dewey::route(&mut builder),
        };
        routed.map_err(|reason| ConstructError::Unplaceable {
            element: "the routes".to_string(),
            reason,
        })?;
        Ok((builder, ports))
    };
    let mut failure = None;
    let mut done = None;
    for targets in &placements {
        match attempt(targets) {
            Ok(result) => {
                done = Some(result);
                break;
            }
            Err(error) => failure = Some(error),
        }
    }
    let Some((
        builder,
        Ports {
            inputs,
            gate_outputs,
            outputs,
            flip_flops,
        },
    )) = done
    else {
        return Err(failure.expect("at least one placement"));
    };

    // Every component starts in the state it settles to at power-on (all
    // inputs low, every flip-flop holding zero), so loading the world fires
    // no transient -- above all no clock edge into a flip-flop.
    let value = power_on(netlist, &order);
    let net_value: HashMap<usize, bool> = builder
        .net_of
        .iter()
        .map(|(signal, &net)| (net, value.get(signal).copied().unwrap_or(false)))
        .collect();
    let mut blocks: CellMap<Anchor, BlockState> = builder
        .grid
        .blocks()
        .map(|(at, owner, block)| {
            let mut block = block.clone();
            if let Owner::Net(net) = owner {
                if matches!(
                    block.kind,
                    BlockKind::Repeater | BlockKind::WallTorch | BlockKind::Lamp
                ) {
                    block.lit = net_value[&net];
                }
            }
            (at, block)
        })
        .collect();
    for (index, origin) in flip_flops {
        let gate = &netlist.gates[index];
        let (d, c) = (value[&gate.inputs[0]], value[&gate.inputs[1]]);
        for (offset, lit) in [
            (tile::DFF_M_DATA, !c && d),
            (tile::DFF_S_DATA, false),
            (tile::DFF_M_LOCK, c),
            (tile::DFF_S_LOCK, !c),
            (tile::DFF_INVERTED_CLOCK, !c),
        ] {
            blocks
                .get_mut(&tile::at(origin, offset))
                .expect("a placed DFF tile")
                .lit = lit;
        }
    }
    let mut blocks: Vec<(Anchor, BlockState)> = blocks.into_iter().collect();
    blocks.sort_by_key(|(at, _)| *at);
    Ok(Construction {
        blocks,
        inputs,
        outputs,
        gate_outputs,
    })
}
