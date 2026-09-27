//! **Risers: a signal's way between two heights, assembled from parts.**
//!
//! A packed parent's trunk leaves a child at terminal height and runs on a
//! lane far above it. The way up used to be one canonical straight staircase
//! per terminal, as long sideways as it was tall; on a dense node those ramps
//! filled every low cell between two children -- measured in `seven_segment`,
//! where 2716 held ramp cells climbed from y3 to lanes at y30..y36. A riser
//! climbs inside a small column beside its mouth instead, and it is not one
//! template: the planner combines parts, choosing per step whatever the cells
//! around it allow.
//!
//! * **Dust steps** -- flat, or one level up or down on a solid support. Free
//!   in delay, one strength per cell, and a step cannot hold a repeater.
//! * **Repeaters** -- not a search move: every stretch of dust is realised by
//!   [`realise_branch_cells`], which stands a repeater on the latest flat,
//!   straight cell before the signal would die. The search only keeps every
//!   run of cells that cannot hold one within `MAX_DUST_RUN`.
//! * **Torch pairs** -- block, torch, block, torch, block, with the climb's
//!   dust continuing on top: five levels in a one-cell column, restoring full
//!   strength, two inversions cancelling, four game ticks of delay. What a
//!   climb uses where there is no room to stair and it must refresh anyway.
//!   Upward flow only.
//! * **Cascade pairs** -- the downward counterpart: a torch hung on the block
//!   under the head's dust lights the dust beneath it, twice, across a
//!   one-by-two column. Four levels, full strength, four game ticks.
//!   Downward flow only.
//! * **Rungs** -- a dust step up on a glass support. Glass does not cut the
//!   dust beneath it, so rungs zig-zag up a one-by-two column where stone
//!   stairs would smother their own dust. No delay; upward flow only, since
//!   dust does not step down off glass.
//!
//! With room the search coils into a spiral; against an obstacle it bends,
//! turns or straightens; boxed in, it climbs rungs or stacks torches. Dust
//! steps follow the router's own rules ([`neighbours`],
//! [`self_obstructs_batch`], [`staircase_clearance_typed`]); rungs and torch
//! parts are fixed shapes checked cell by cell. Either way the simulator has
//! the last word: every riser [`realise_riser`] returns is one
//! [`carries_in_isolation`] has driven both ways.

// ponytail: realisation is exercised by the tests and waits for a parent
// that builds its own risers; drop this once one calls `realise_riser`.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use super::{
    horizontal_direction, neighbours, realise_branch_cells, self_obstructs_batch,
    staircase_clearance_typed, step, ReservePolicy,
};
use crate::compile::geometry::Anchor;
use crate::compile::{dust, redstone_block, stone, MAX_DUST_RUN};
use crate::redstone::simulator::propagate::MAX_SIGNAL_STRENGTH;
use crate::redstone::simulator::Simulator;
use crate::redstone::world::block::{BlockKind, BlockState, Facing};
use crate::redstone::world::storage::World;

/// Which way the signal travels through a riser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiserFlow {
    /// In at `mouth`, out at the top: a source's way up to its lane.
    Up,
    /// In at the top, out at `mouth`: a lane's way down to a sink.
    Down,
}

/// What a riser must connect, and how much room it has.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RiserRequest {
    /// The riser's first dust cell, reached along a straight runway.
    pub mouth: Anchor,
    /// The direction the runway runs into `mouth`, away from the terminal.
    pub entered: Facing,
    /// The height the riser ends at.
    pub top: i32,
    pub flow: RiserFlow,
    /// How far sideways any part may stand from `mouth`, in `x` and in `z`.
    pub reach: i32,
    /// Search nodes allowed before the column is declared to hold no riser.
    pub max_nodes: usize,
    /// Whether torch parts may be used: pairs going up, cascades going down.
    /// Each costs four game ticks of delay.
    pub torches: bool,
    /// Whether rungs on glass may be used. Only an upward riser can.
    pub ladders: bool,
}

/// One part of a planned riser, in order from `mouth` upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiserStep {
    /// A dust cell on a solid support one below it.
    Dust(Anchor),
    /// A dust cell one level above the previous one, on glass.
    Rung(Anchor),
    /// A downward torch cascade over two columns: `bottom`, where the signal
    /// leaves it, and the column one step `toward`. The next step is the
    /// dust it is fed from, four above `bottom`.
    CascadePair { bottom: Anchor, toward: Facing },
    /// A torch pair whose bottom block stands at `block`: blocks at `block`,
    /// `+2` and `+4`, torches at `+1` and `+3`. The next step is the dust on
    /// top, at `+5`.
    TorchPair { block: Anchor },
}

/// A realised riser: what to place, where it ends, what it costs in time.
#[derive(Debug, Clone)]
pub(crate) struct Riser {
    /// Every block the riser places, supports included, `mouth` first.
    pub blocks: Vec<(Anchor, BlockState)>,
    /// The riser's top dust cell.
    pub exit: Anchor,
    pub delay_game_ticks: u32,
}

/// Search charge for one torch pair: its five blocks, and its four game ticks
/// of delay weighed heavily enough that dust wins wherever dust fits.
const TORCH_PAIR_COST: u64 = 24;

fn up(at: Anchor, levels: i32) -> Anchor {
    Anchor {
        y: at.y + levels,
        ..at
    }
}

/// The five cells a torch pair standing on `block` occupies.
fn torch_column(block: Anchor) -> [Anchor; 5] {
    [0, 1, 2, 3, 4].map(|levels| up(block, levels))
}

/// Cells beside a torch pair's torches and powered blocks that no dust of
/// the riser may occupy: a torch lights dust beside it, and a block a torch
/// powers strongly lights dust beside it too.
fn torch_ring(block: Anchor) -> Vec<Anchor> {
    (1..=4)
        .flat_map(|levels| {
            [Facing::North, Facing::South, Facing::East, Facing::West]
                .map(|side| step(up(block, levels), side))
        })
        .collect()
}

/// A cascade pair's blocks above `bottom`, and the cells that must stay
/// empty for it: the head dust four above `bottom` weakly powers its support,
/// a wall torch on that support lights the dust beneath it in the other
/// column, and so again back into `bottom`.
fn cascade(bottom: Anchor, toward: Facing) -> (Vec<(Anchor, BlockState)>, Vec<Anchor>) {
    let other = step(bottom, toward);
    let blocks = vec![
        (up(bottom, 1), wall_torch(toward.opposite())),
        (up(other, 1), stone()),
        (up(other, 2), dust()),
        (up(other, 3), wall_torch(toward)),
        (up(bottom, 3), stone()),
    ];
    let empty = vec![up(bottom, 2), up(other, 4)];
    (blocks, empty)
}

/// Every cell a cascade pair holds, and the ring beside it no other dust may
/// enter.
fn cascade_held(bottom: Anchor, toward: Facing) -> Vec<Anchor> {
    let (blocks, empty) = cascade(bottom, toward);
    let own = blocks
        .iter()
        .map(|(cell, _)| *cell)
        .chain(empty)
        .collect::<BTreeSet<_>>();
    let ring = own.iter().flat_map(|cell| {
        [Facing::North, Facing::South, Facing::East, Facing::West].map(|side| step(*cell, side))
    });
    let exclude = [bottom, up(bottom, 4)];
    own.iter()
        .copied()
        .chain(ring)
        .filter(|cell| !exclude.contains(cell))
        .collect()
}

fn wall_torch(facing: Facing) -> BlockState {
    BlockState {
        kind: BlockKind::WallTorch,
        facing: Some(facing),
        lit: true,
        name: "minecraft:redstone_wall_torch".into(),
        ..BlockState::air()
    }
}

fn glass() -> BlockState {
    BlockState {
        kind: BlockKind::Glass,
        name: "minecraft:glass".into(),
        ..BlockState::air()
    }
}

fn torch() -> BlockState {
    BlockState {
        kind: BlockKind::Torch,
        lit: true,
        name: "minecraft:redstone_torch".into(),
        ..BlockState::air()
    }
}

/// Plan a riser for `request` among cells `open` lets it lay dust in and
/// `clear` lets it hold as supports, clearance or a torch pair's surround.
/// Returns its parts from `mouth` up, `mouth` itself excluded, or `None` when
/// the column holds no riser within `max_nodes`.
pub(crate) fn plan_riser(
    request: &RiserRequest,
    open: impl Fn(Anchor) -> bool,
    clear: impl Fn(Anchor) -> bool,
) -> Option<Vec<RiserStep>> {
    let RiserRequest {
        mouth,
        entered,
        top,
        flow,
        reach,
        max_nodes,
        torches,
        ladders,
    } = *request;
    if top <= mouth.y {
        return Some(Vec::new());
    }
    let torch_pairs = torches && flow == RiserFlow::Up;
    let cascades = torches && flow == RiserFlow::Down;
    let ladders = ladders && flow == RiserFlow::Up;
    let in_reach = |at: Anchor| {
        at.x.abs_diff(mouth.x) <= reach.unsigned_abs() && at.z.abs_diff(mouth.z) <= reach.unsigned_abs()
    };
    // Search state: the head dust cell, the run of cells behind it that
    // cannot hold a repeater, and the cell it was entered from. At one cell
    // from one predecessor, an arrival dominates only when no costlier and no
    // further from a refresh (`arrival_dominates`'s rule), so no arrival
    // shadows one that can still refresh.
    type Node = (Anchor, usize, Anchor);
    let back = |at: Anchor, cells: i32| (0..cells).fold(at, |at, _| step(at, entered.opposite()));
    // The runway the route arrives along is flat and straight; count the
    // mouth's run from one cell behind it, conservatively.
    let runway = [back(mouth, 1), back(mouth, 2), back(mouth, 3)];
    let start: Node = (mouth, 1, runway[0]);
    let mut parent = BTreeMap::<Node, (Node, Vec<RiserStep>)>::new();
    let mut cost = BTreeMap::<Node, u64>::from([(start, 0)]);
    let mut best = BTreeMap::<(Anchor, Anchor), Vec<(u64, usize)>>::new();
    let mut frontier = BTreeSet::from([(u64::try_from(top - mouth.y).ok()?, 0u64, start)]);
    // Everything placed behind `node`: its dust, newest first, then the
    // runway; and the cells torch pairs hold, which no later part may enter.
    let placed = |parent: &BTreeMap<Node, (Node, Vec<RiserStep>)>, node: Node| {
        let mut chain = vec![node.0];
        let mut held = BTreeSet::new();
        let mut walk = node;
        while let Some((before, steps)) = parent.get(&walk) {
            for part in steps.iter().rev() {
                match *part {
                    RiserStep::TorchPair { block } => {
                        held.extend(torch_column(block));
                        held.extend(torch_ring(block));
                    }
                    RiserStep::CascadePair { bottom, toward } => {
                        held.extend(cascade_held(bottom, toward));
                    }
                    RiserStep::Rung(cell) => {
                        held.insert(up(cell, -1));
                    }
                    RiserStep::Dust(_) => {}
                }
            }
            chain.push(before.0);
            walk = *before;
        }
        chain.extend(runway);
        (chain, held)
    };
    let mut expanded = 0usize;
    while let Some(entry) = frontier.iter().next().copied() {
        frontier.remove(&entry);
        let (_, travelled, node) = entry;
        if cost.get(&node) != Some(&travelled) {
            continue;
        }
        let (at, run, before) = node;
        if at.y == top {
            let mut steps = Vec::new();
            let mut walk = node;
            while let Some((earlier, parts)) = parent.get(&walk) {
                steps.extend(parts.iter().rev().copied());
                walk = *earlier;
            }
            steps.reverse();
            return Some(steps);
        }
        expanded += 1;
        if expanded > max_nodes {
            return None;
        }
        let (chain, held) = placed(&parent, node);
        let entered_at = horizontal_direction(before, at);
        let mut offer = |next: Node, parts: Vec<RiserStep>, charge: u64| {
            let (cell, behind, _) = next;
            let spread = cell.x.abs_diff(mouth.x).max(cell.z.abs_diff(mouth.z));
            let next_cost = travelled + charge + u64::from(spread);
            let seen = best.entry((cell, next.2)).or_default();
            if seen
                .iter()
                .any(|&(known_cost, known_behind)| known_cost <= next_cost && known_behind <= behind)
            {
                return;
            }
            seen.retain(|&(known_cost, known_behind)| {
                !(next_cost <= known_cost && behind <= known_behind)
            });
            seen.push((next_cost, behind));
            cost.insert(next, next_cost);
            parent.insert(next, (node, parts));
            let remaining = u64::try_from(top - cell.y).unwrap_or(0);
            frontier.insert((next_cost + remaining, next_cost, next));
        };

        // Dust steps.
        let candidates = neighbours(at);
        let mut obstructed = vec![false; candidates.len()];
        self_obstructs_batch(chain.iter().copied(), at, &candidates, true, &mut obstructed);
        for (next, obstructs) in candidates.into_iter().zip(obstructed) {
            let support = up(next, -1);
            if obstructs
                || next.y < mouth.y
                || next.y > top
                || !in_reach(next)
                || chain.contains(&next)
                || held.contains(&next)
                || held.contains(&support)
                || !open(next)
                || !clear(support)
                || !staircase_clearance_typed(at, next)
                    .into_iter()
                    .all(|cell| clear(cell) && !held.contains(&cell))
            {
                continue;
            }
            let behind = if entered_at.is_some() && horizontal_direction(at, next) == entered_at {
                0
            } else {
                run + 1
            };
            if behind + usize::from(next.y != at.y) > MAX_DUST_RUN as usize {
                continue;
            }
            offer((next, behind, at), vec![RiserStep::Dust(next)], 1);
        }

        // A torch pair, fed straight from `at`'s dust.
        if let (true, Some(direction)) = (torch_pairs, entered_at) {
            let block = step(at, direction);
            let exit = up(block, 5);
            let column = torch_column(block);
            let ring = torch_ring(block);
            let fits = exit.y <= top
                && run < MAX_DUST_RUN as usize
                && column.iter().chain([&exit]).all(|cell| {
                    in_reach(*cell) && open(*cell) && !chain.contains(cell) && !held.contains(cell)
                })
                && ring
                    .iter()
                    .all(|cell| clear(*cell) && !chain.contains(cell) && !held.contains(cell));
            if fits {
                offer(
                    (exit, 0, column[4]),
                    vec![RiserStep::TorchPair { block }, RiserStep::Dust(exit)],
                    TORCH_PAIR_COST,
                );
            }
        }

        // A cascade pair feeding `at` from four above it. The torch stands
        // over `at`, so `at` must not have been reached by stepping down onto
        // it: that connection needs the cell over `at` open.
        if cascades && at.y >= before.y && run < MAX_DUST_RUN as usize {
            for toward in [Facing::North, Facing::South, Facing::East, Facing::West] {
                let head = up(at, 4);
                let (blocks, empty) = cascade(at, toward);
                let own = blocks
                    .iter()
                    .map(|(cell, _)| *cell)
                    .chain(empty.iter().copied())
                    .chain([head])
                    .collect::<Vec<_>>();
                let ring = cascade_held(at, toward)
                    .into_iter()
                    .filter(|cell| !own.contains(cell))
                    .collect::<Vec<_>>();
                let fits = head.y <= top
                    && own.iter().all(|cell| {
                        in_reach(*cell) && open(*cell) && !chain.contains(cell) && !held.contains(cell)
                    })
                    && ring
                        .iter()
                        .all(|cell| clear(*cell) && !chain.contains(cell) && !held.contains(cell));
                if fits {
                    offer(
                        (head, 0, up(at, 3)),
                        vec![RiserStep::CascadePair { bottom: at, toward }, RiserStep::Dust(head)],
                        TORCH_PAIR_COST,
                    );
                }
            }
        }

        // Rungs: a step up onto glass. Glass leaves the cell over `at`
        // transparent, so a later rung may stand its own glass there.
        if ladders {
            for side in [Facing::North, Facing::South, Facing::East, Facing::West] {
                let support = step(at, side);
                let next = up(support, 1);
                let headroom = up(at, 1);
                let behind = run + 1;
                let fits = next.y <= top
                    && behind < MAX_DUST_RUN as usize
                    && in_reach(next)
                    && open(next)
                    && clear(support)
                    && clear(headroom)
                    && [next, support, headroom]
                        .iter()
                        .all(|cell| !chain.contains(cell))
                    && !held.contains(&next)
                    && !held.contains(&support);
                if fits {
                    offer((next, behind, at), vec![RiserStep::Rung(next)], 2);
                }
            }
        }
    }
    None
}

/// Why a planned riser could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RiserError {
    #[error("a torch pair cannot carry a downward riser")]
    TorchPairGoingDown,
    #[error("a cascade pair cannot carry an upward riser")]
    CascadeGoingUp,
    #[error("a rung cannot carry a downward riser")]
    RungGoingDown,
    #[error("the stretch of dust ending at {end:?} does not carry")]
    DoesNotCarry { end: Anchor },
    #[error("the realised riser does not switch its exit both ways in the simulator")]
    FailsInIsolation,
}

/// Lay `steps` for `request`, arriving with `incoming` strength: dust on
/// stone, a repeater wherever each stretch of dust needs one, torch pairs as
/// planned. The result has been driven both ways by [`carries_in_isolation`].
pub(crate) fn realise_riser(
    request: &RiserRequest,
    steps: &[RiserStep],
    incoming: u8,
) -> Result<Riser, RiserError> {
    let mut blocks = Vec::new();
    let mut delay_game_ticks = 0u32;
    let rungs = steps
        .iter()
        .filter_map(|part| match part {
            RiserStep::Rung(cell) => Some(*cell),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    // Lays one stretch of dust into `blocks` and returns its repeaters'
    // delay. `hosts_refresh` false keeps its last cell dust: a stretch that
    // feeds a cascade must power the block under its last cell.
    let lay_stretch = |previous: Anchor,
                       incoming: u8,
                       cells: &[Anchor],
                       hosts_refresh: bool,
                       blocks: &mut Vec<(Anchor, BlockState)>| {
            let laid = realise_branch_cells(
                previous,
                incoming,
                cells,
                true,
                ReservePolicy::LatestLegalCell,
                hosts_refresh,
            );
            if !laid.carries {
                return Err(RiserError::DoesNotCarry {
                    end: *cells.last().expect("a stretch has a cell"),
                });
            }
            let mut delay = 0;
            for ((cell, block), floor) in cells.iter().zip(laid.blocks).zip(laid.floors) {
                if block.kind == BlockKind::Repeater {
                    delay += 2 * u32::from(block.delay.max(1));
                }
                let floor = if rungs.contains(cell) { glass() } else { floor };
                blocks.push((up(*cell, -1), floor));
                blocks.push((*cell, block));
            }
            Ok(delay)
        };
    let dust_cells = std::iter::once(request.mouth)
        .chain(steps.iter().filter_map(|part| match part {
            RiserStep::Dust(cell) | RiserStep::Rung(cell) => Some(*cell),
            RiserStep::TorchPair { .. } | RiserStep::CascadePair { .. } => None,
        }))
        .collect::<Vec<_>>();
    let exit = *dust_cells.last().expect("the mouth is a dust cell");
    match request.flow {
        RiserFlow::Up => {
            let mut previous = step(request.mouth, request.entered.opposite());
            let mut strength = incoming;
            let mut stretch = vec![request.mouth];
            for part in steps {
                match *part {
                    RiserStep::Dust(cell) | RiserStep::Rung(cell) => stretch.push(cell),
                    RiserStep::CascadePair { .. } => return Err(RiserError::CascadeGoingUp),
                    RiserStep::TorchPair { block } => {
                        delay_game_ticks +=
                            lay_stretch(previous, strength, &stretch, true, &mut blocks)?;
                        for (levels, cell) in torch_column(block).into_iter().enumerate() {
                            blocks.push((cell, if levels % 2 == 0 { stone() } else { torch() }));
                        }
                        delay_game_ticks += 4;
                        previous = up(block, 4);
                        strength = MAX_SIGNAL_STRENGTH;
                        stretch.clear();
                    }
                }
            }
            delay_game_ticks += lay_stretch(previous, strength, &stretch, true, &mut blocks)?;
        }
        RiserFlow::Down => {
            // In flow order, top down: stretches of dust, broken by cascades.
            let mut stretches: Vec<Vec<Anchor>> = vec![vec![request.mouth]];
            let mut cascades = Vec::new();
            for part in steps {
                match *part {
                    RiserStep::Dust(cell) => stretches.last_mut().expect("one stretch").push(cell),
                    RiserStep::Rung(_) => return Err(RiserError::RungGoingDown),
                    RiserStep::TorchPair { .. } => return Err(RiserError::TorchPairGoingDown),
                    RiserStep::CascadePair { bottom, toward } => {
                        cascades.push((bottom, toward));
                        stretches.push(Vec::new());
                    }
                }
            }
            let mut previous = None;
            let mut strength = incoming;
            for (index, stretch) in stretches.iter().enumerate().rev() {
                let flow = stretch.iter().rev().copied().collect::<Vec<_>>();
                let feeds_cascade = index > 0;
                delay_game_ticks += lay_stretch(
                    previous.unwrap_or(flow[0]),
                    strength,
                    &flow,
                    !feeds_cascade,
                    &mut blocks,
                )?;
                if feeds_cascade {
                    let (bottom, toward) = cascades[index - 1];
                    blocks.extend(cascade(bottom, toward).0);
                    delay_game_ticks += 4;
                    previous = Some(up(bottom, 1));
                    strength = MAX_SIGNAL_STRENGTH;
                }
            }
        }
    }
    let riser = Riser {
        blocks,
        exit,
        delay_game_ticks,
    };
    if !carries_in_isolation(request, &riser) {
        return Err(RiserError::FailsInIsolation);
    }
    Ok(riser)
}

/// Build `riser` alone in a world, drive its input on and off with a redstone
/// block, and report whether its output follows both ways: the exit for an
/// upward riser, `mouth` for a downward one.
pub(crate) fn carries_in_isolation(request: &RiserRequest, riser: &Riser) -> bool {
    let placed = riser
        .blocks
        .iter()
        .map(|(cell, _)| *cell)
        .collect::<BTreeSet<_>>();
    let (feed, output) = match request.flow {
        RiserFlow::Up => (step(request.mouth, request.entered.opposite()), request.mouth),
        RiserFlow::Down => {
            let Some(feed) = [Facing::North, Facing::South, Facing::East, Facing::West]
                .into_iter()
                .map(|side| step(riser.exit, side))
                .find(|cell| !placed.contains(cell) && !placed.contains(&up(*cell, -1)))
            else {
                return false;
            };
            (feed, request.mouth)
        }
    };
    let read = match request.flow {
        RiserFlow::Up => riser.exit,
        RiserFlow::Down => output,
    };
    let all = placed.iter().copied().chain([feed, up(feed, -1)]);
    let (mut lo, mut hi) = (
        Anchor {
            x: i32::MAX,
            y: i32::MAX,
            z: i32::MAX,
        },
        Anchor {
            x: i32::MIN,
            y: i32::MIN,
            z: i32::MIN,
        },
    );
    for cell in all {
        lo = Anchor {
            x: lo.x.min(cell.x),
            y: lo.y.min(cell.y),
            z: lo.z.min(cell.z),
        };
        hi = Anchor {
            x: hi.x.max(cell.x),
            y: hi.y.max(cell.y),
            z: hi.z.max(cell.z),
        };
    }
    let shift = |cell: Anchor| (cell.x - lo.x + 1, cell.y - lo.y + 1, cell.z - lo.z + 1);
    let mut world = World::new(hi.x - lo.x + 3, hi.y - lo.y + 3, hi.z - lo.z + 3);
    for (cell, block) in &riser.blocks {
        let (x, y, z) = shift(*cell);
        world.set(x, y, z, block.clone());
    }
    // An upward riser is fed through the runway cell behind its mouth, laid
    // as dust on stone; a downward one straight into its exit.
    let (fx, fy, fz) = shift(feed);
    if request.flow == RiserFlow::Up {
        world.set(fx, fy - 1, fz, stone());
        world.set(fx, fy, fz, dust());
    }
    let source = match request.flow {
        RiserFlow::Up => shift(step(feed, request.entered.opposite())),
        RiserFlow::Down => (fx, fy, fz),
    };
    let (rx, ry, rz) = shift(read);
    let mut simulator = Simulator::new(world);
    let powered = |simulator: &Simulator| simulator.world().get(rx, ry, rz).power > 0;
    if simulator.run_until_stable(2_000).is_err() || powered(&simulator) {
        return false;
    }
    simulator
        .world_mut()
        .set(source.0, source.1, source.2, redstone_block());
    if simulator.run_until_stable(2_000).is_err() || !powered(&simulator) {
        return false;
    }
    simulator
        .world_mut()
        .set(source.0, source.1, source.2, BlockState::air());
    simulator.run_until_stable(2_000).is_ok() && !powered(&simulator)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32, z: i32) -> Anchor {
        Anchor { x, y, z }
    }

    fn request(top: i32, flow: RiserFlow, torches: bool) -> RiserRequest {
        RiserRequest {
            mouth: at(20, 1, 20),
            entered: Facing::East,
            top,
            flow,
            reach: 3,
            max_nodes: 50_000,
            torches,
            ladders: false,
        }
    }

    fn footprint_within(request: &RiserRequest, riser: &Riser) {
        for (cell, _) in &riser.blocks {
            let reach = request.reach.unsigned_abs();
            assert!(
                cell.x.abs_diff(request.mouth.x) <= reach && cell.z.abs_diff(request.mouth.z) <= reach,
                "{cell:?} outside the column"
            );
        }
    }

    #[test]
    fn an_open_column_coils_dust_thirty_levels_up_and_back_down() {
        for flow in [RiserFlow::Up, RiserFlow::Down] {
            let request = request(31, flow, false);
            let steps = plan_riser(&request, |_| true, |_| true).expect("an open column holds a riser");
            let riser = realise_riser(&request, &steps, MAX_SIGNAL_STRENGTH)
                .unwrap_or_else(|error| panic!("{flow:?}: {error}: {steps:?}"));
            assert_eq!(riser.exit.y, 31);
            assert!(steps.iter().all(|part| matches!(part, RiserStep::Dust(_))));
            footprint_within(&request, &riser);
        }
    }

    #[test]
    fn a_riser_bends_around_an_obstacle_in_its_column() {
        let request = request(20, RiserFlow::Up, false);
        let blocked = |cell: Anchor| (8..=12).contains(&cell.y) && cell.x < 23;
        let steps = plan_riser(&request, |cell| !blocked(cell), |cell| !blocked(cell))
            .expect("the column still has a way past the slab");
        let riser = realise_riser(&request, &steps, MAX_SIGNAL_STRENGTH).unwrap();
        assert!(riser.blocks.iter().all(|(cell, _)| !blocked(*cell)));
        assert!(steps
            .iter()
            .any(|part| matches!(part, RiserStep::Dust(cell) if (8..=12).contains(&cell.y))));
        assert!(plan_riser(&request, |cell| cell.y != 10, |_| true).is_none());
    }

    /// Two cells wide and nowhere to stair: a short rise that must also
    /// refresh is one torch pair, and without torch pairs there is no riser.
    #[test]
    fn a_boxed_short_rise_stacks_a_torch_pair() {
        let request = request(6, RiserFlow::Up, true);
        let open = |cell: Anchor| cell.z == 20 && (20..=21).contains(&cell.x);
        let steps = plan_riser(&request, open, |_| true).expect("a torch pair fits the strip");
        assert!(steps
            .iter()
            .any(|part| matches!(part, RiserStep::TorchPair { .. })));
        let riser = realise_riser(&request, &steps, 3).expect("the pair refreshes a weak signal");
        assert_eq!(riser.exit.y, 6);
        assert_eq!(riser.delay_game_ticks, 4);
        assert!(plan_riser(&RiserRequest { torches: false, ..request }, open, |_| true).is_none());
        // Going down, the same strip is a cascade pair's, not a torch pair's.
        let down = RiserRequest {
            flow: RiserFlow::Down,
            ..request
        };
        let steps = plan_riser(&down, open, |_| true).expect("a cascade fits the strip");
        assert!(steps
            .iter()
            .all(|part| !matches!(part, RiserStep::TorchPair { .. })));
        realise_riser(&down, &steps, 3).expect("the descent carries");
    }

    /// With room to stair, dust wins over a torch pair's delay.
    #[test]
    fn an_open_column_prefers_dust_to_torches() {
        let request = request(16, RiserFlow::Up, true);
        let steps = plan_riser(&request, |_| true, |_| true).unwrap();
        assert!(steps.iter().all(|part| matches!(part, RiserStep::Dust(_))));
    }

    /// Two cells wide: stone stairs smother their own dust, rungs on glass
    /// zig-zag straight up with no delay -- and only upward.
    #[test]
    fn a_one_by_two_column_climbs_on_rungs() {
        let request = RiserRequest {
            ladders: true,
            ..request(13, RiserFlow::Up, false)
        };
        let open = |cell: Anchor| cell.z == 20 && (20..=21).contains(&cell.x);
        let steps = plan_riser(&request, open, open).expect("rungs fit the strip");
        assert!(steps.iter().any(|part| matches!(part, RiserStep::Rung(_))));
        let riser = realise_riser(&request, &steps, MAX_SIGNAL_STRENGTH)
            .unwrap_or_else(|error| panic!("{error}: {steps:?}"));
        assert_eq!(riser.exit.y, 13);
        assert!(riser
            .blocks
            .iter()
            .any(|(_, block)| block.kind == BlockKind::Glass));
        assert!(plan_riser(&RiserRequest { ladders: false, ..request }, open, open).is_none());
        assert!(plan_riser(
            &RiserRequest {
                flow: RiserFlow::Down,
                ..request
            },
            open,
            open
        )
        .is_none());
    }

    /// Two cells wide going down, arriving weak: a cascade pair drops four
    /// levels and hands the rest of the way full strength.
    #[test]
    fn a_boxed_descent_refreshes_through_a_cascade_pair() {
        let request = request(9, RiserFlow::Down, true);
        let open = |cell: Anchor| cell.z == 20 && (20..=21).contains(&cell.x);
        let steps = plan_riser(&request, open, |_| true).expect("a cascade fits the strip");
        assert!(steps
            .iter()
            .any(|part| matches!(part, RiserStep::CascadePair { .. })));
        let riser = realise_riser(&request, &steps, 2)
            .unwrap_or_else(|error| panic!("{error}: {steps:?}"));
        assert_eq!(riser.exit.y, 9);
        assert!(riser.delay_game_ticks >= 4);
        assert!(plan_riser(&RiserRequest { torches: false, ..request }, open, |_| true).is_none());
    }
}
