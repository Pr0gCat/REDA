//! The hard constraints: how far apart two bodies must be, and what has to
//! hold exactly.
//!
//! Separation is projected rather than added as a force, because the number it
//! enforces is derived rather than tuned -- and because a force that competes
//! with the springs settles at whatever the two balance out to, which is a
//! layout that is nearly legal.

use crate::compile::geometry;
use crate::compile::relax::build::{cells, BodyGraph, BodyKind, Weld};
use crate::redstone::simulator::position::Position;

/// Two conductors of different signals need two cells of clearance.
///
/// Derived in `2026-08-09-channel-safety-condition.md` from `dust_reach`,
/// whose every case is a horizontal cardinal step with a vertical difference
/// of 0 or 1: "a gap of 2 in the shared horizontal axis is both necessary and
/// sufficient to rule out every case at once". Not a tuning parameter.
pub const CONDUCTOR_CLEARANCE: f64 = 2.0;

/// What rounding a position can cost.
///
/// Rounding moves a body by at most half a cell, so two bodies approach by at
/// most one, and a continuous solution separated by the requirement plus one
/// is still separated after.
pub const SNAP_MARGIN: f64 = 1.0;

/// The pitch two parallel foreign routes need -- the same 2, for the same
/// reason: a route is one cell of dust, and two foreign dust runs need a gap
/// of 2.
pub const ROUTE_PITCH: f64 = 2.0;

/// How many separate-then-weld rounds a projection gets before it is called a
/// deadlock.
///
/// A budget rather than a proof: three bodies that must each touch a fourth
/// and each stay clear of the others may have no arrangement at all.
pub const PROJECTION_ROUNDS: usize = 4096;

/// How close to satisfied counts as satisfied.
///
/// Not zero, and the difference is not pedantry. This design's own premise is
/// that the relaxed solution sits *at* the minimum separation everywhere, and
/// a pair sitting exactly there has a shortfall of float residue rather than
/// 0.0. Testing `> 0.0` made the projection move bodies by 5e-17, call it
/// progress, and spend its whole budget at its own designed equilibrium --
/// measured on 2026-08-13, in a run that reported its remaining violation as
/// `0.000`.
///
/// A millionth of a cell: below anything rounding can express, above the
/// residue of summing a few hundred coordinates of order a thousand.
pub const SETTLED: f64 = 1e-6;

/// Room a body reserves beyond its own clearance for the routes that must
/// reach it.
///
/// Routes arrive from every side, so `d` lanes at [`ROUTE_PITCH`] sit on a
/// ring rather than in a line: a ring at radius `r` around a cell has about
/// `8r` lattice cells on it, and `8r >= ROUTE_PITCH * d` gives `r >= d / 4`.
///
/// The spec states this term as `routed_degree * route_width` outright -- a
/// length. That is the *total* width the routes need, not the radius that
/// supplies it, and spending it as a radius would hold two degree-4 bodies
/// eight cells apart before clearance was even added. The perimeter step
/// converts the one into the other, and the spec's term 3 is amended to match.
///
/// **This is the design's one guessed number.** The spec says how it fails: a
/// halo is not a channel, and a high-degree gate gets a large ring whether or
/// not its neighbours needed one. If placements come out routable but
/// wasteful, or compact but unroutable, this is what was wrong.
pub fn reservation(routed_degree: usize) -> f64 {
    ROUTE_PITCH * routed_degree as f64 / 8.0
}

/// Which axes relaxation may move a body along.
///
/// It governs the linear solve as well as the projection, which is what makes
/// "bodies stay at the Y their starting layout gave them" true rather than
/// merely intended: restricting only the projection leaves the solve free to
/// pull every body onto one plane.
///
/// Stage 1 is in-plane. Stage 2 adds Y, and that one-word difference is the
/// whole of "let separation choose the axis".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axes(&'static [usize]);

impl Axes {
    pub const IN_PLANE: Axes = Axes(&[0, 2]);
    pub const ALL: Axes = Axes(&[0, 1, 2]);

    pub fn iter(self) -> impl Iterator<Item = usize> {
        self.0.iter().copied()
    }
}

/// A pair that is too close, and by how much.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Violation {
    pub left: usize,
    pub right: usize,
    pub shortfall: f64,
}

/// How far each body must stay from a foreign conductor.
///
/// Three of the spec's four terms; the fourth -- "the cells each body
/// occupies" -- is not a distance but the thing distances are measured
/// between, and it is why this is per-body rather than one number.
pub fn required_separations(graph: &BodyGraph) -> Vec<f64> {
    // Pulls are the edges a router has to lay dust for, and they are already
    // exactly that: `signal_pulls` never emits one between a welded pair,
    // because a welded pair is adjacent by construction and no wire runs
    // between them. An earlier draft subtracted the welds again here, which
    // took a route away from every junction that had not been charged for one.
    let mut degree = vec![0usize; graph.bodies.len()];
    for pull in &graph.pulls {
        degree[pull.from.0] += 1;
        degree[pull.to.0] += 1;
    }
    degree
        .into_iter()
        .map(|routed| CONDUCTOR_CLEARANCE + reservation(routed) + SNAP_MARGIN)
        .collect()
}

/// What one walk of a pair's cells found: how far short they are, and what
/// clearing them along each axis would cost.
///
/// One struct rather than two functions because the two answers come from the
/// same walk. Deciding whether a pair is violating and deciding which way to
/// push it both range over every cell of one body against every cell of the
/// other, and the test at the centre of that walk -- [`unseparated`] -- is a
/// string comparison per net per cell pair. Asking separately meant walking
/// once for the shortfall and once more per axis, three times over the same
/// cells for an answer that had already been computed.
#[derive(Debug, Clone, Copy)]
struct Offence {
    /// How far short of the requirement the closest offending pair of cells
    /// is, by horizontal Chebyshev. Zero when no pair offends.
    shortfall: f64,
    /// Per axis, how far the worst offending pair falls short of that axis's
    /// own target. Computed for all three whether or not [`Axes`] asks for
    /// them: it is three subtractions inside a loop that is already comparing
    /// strings, and it lets one walk answer either stage.
    deficit: [f64; 3],
}

/// Measure one pair of bodies, cell against cell.
///
/// **Cells, not centres.** A body is not a point: a torch is its support and
/// its torch block, and two torches three apart facing each other have their
/// torch blocks one apart. Measuring between centres would separate the wrong
/// thing and produce exactly the failure this design exists to avoid.
///
/// Each pair is measured by horizontal Chebyshev against the requirement,
/// **or** two cells of height. The horizontal requirement carries the routing
/// reservation and the vertical one does not, which is why crowding buys
/// height rather than width: a body with nowhere to go sideways has somewhere
/// to go up, and it is cheaper.
///
/// Two cells of height, not the one the safety condition alone would allow.
/// That condition is derived from `dust_reach`, whose every unsafe case takes a
/// horizontal cardinal step, so it has no pure-vertical case at all -- but
/// `dust_reach` is the *join* mechanism, and power reaching a block from the
/// dust above or below it is a different one nobody here has derived. Two is
/// [`CONDUCTOR_CLEARANCE`] applied to an axis rather than a new claim, and it is
/// already cheap enough to produce the stacking. Tightening it to one is worth
/// a measurement and needs that derivation first; the spec's test 8 says the
/// same.
///
/// Conservative in two ways the derivation would allow relaxing -- it forbids
/// the horizontal diagonal, which `dust_reach` has no case for, and it ignores
/// that a repeater is a firewall on its non-facing sides. Both are a
/// measurement away, and both are the first thing to try if layouts come out
/// sparse.
fn offence(left: &[PlacedCell], right: &[PlacedCell], required: f64) -> Offence {
    let mut found = Offence { shortfall: 0.0, deficit: [0.0; 3] };
    for here in left {
        for there in right {
            if !unseparated(here, there, required) {
                continue;
            }
            let apart = [
                (here.at[0] - there.at[0]).abs(),
                (here.at[1] - there.at[1]).abs(),
                (here.at[2] - there.at[2]).abs(),
            ];
            found.shortfall = found.shortfall.max(required - apart[0].max(apart[2]));
            // The worst offending pair decides what a move along each axis
            // costs, because moving on one axis shifts every pair by the same
            // amount. Y is charged [`CONDUCTOR_CLEARANCE`] and the horizontal
            // axes the pair's own requirement, which is the whole of "crowding
            // buys height".
            found.deficit[0] = found.deficit[0].max(required - apart[0]);
            found.deficit[1] = found.deficit[1].max(CONDUCTOR_CLEARANCE - apart[1]);
            found.deficit[2] = found.deficit[2].max(required - apart[2]);
        }
    }
    found.shortfall = found.shortfall.max(0.0);
    found
}

/// Whether this one pair of cells is a violation.
///
/// Exempt when they *share* a signal -- the route between them is what makes
/// them one thing -- and when either is inert.
///
/// Inert means a floor. `cells` emits an empty `carries` in exactly four
/// places: a junction's floor, a lever's home floor and its pin's floor, and a
/// primitive variant's `Solid` block -- and the one body `build` ever gives no
/// output to is an isolated branch's repeater, whose only `Solid` is the `DOWN`
/// its variant stands on. A floor conducts no net, so nothing has to be held
/// away from it; the one thing separation would otherwise buy is the cell
/// itself, and in Stage 1 that is worth nothing.
///
/// That inventory is of what `build` produces. This module's own test fixture
/// deliberately sits outside it -- it hands `Body` an `output: None` with a
/// `Primitive::Torch`, which no `build` path does, and whose inert `ORIGIN`
/// cell is a *support* rather than a floor. Nothing in those tests measures
/// against that cell, but a reader who finds it two hundred lines below should
/// not have to wonder whether this paragraph is wrong. `ensure_floor` writes
/// `stone()` through a bare `world.set`, so two floors landing in one cell is
/// the same stone written twice.
///
/// Not because a spacing check covers it. `planner::verify_spacing` walks
/// `candidate.routes[..].anchors` and proves every *routed* cell has one owner;
/// a floor produces no anchor and never appears in it.
///
/// Share, not equal. A NOR's support is on every one of its input nets, so a
/// socket carrying the second input shares a net with it and must not be
/// pushed away from it. Requiring equality would have separation fight the
/// springs on every gate of arity two or more.
fn unseparated(here: &PlacedCell, there: &PlacedCell, required: f64) -> bool {
    if here.carries.is_empty() || there.carries.is_empty() {
        return false;
    }
    if here.carries.iter().any(|mine| there.carries.contains(mine)) {
        return false;
    }
    let dx = (here.at[0] - there.at[0]).abs();
    let dy = (here.at[1] - there.at[1]).abs();
    let dz = (here.at[2] - there.at[2]).abs();
    dy < CONDUCTOR_CLEARANCE && dx.max(dz) < required
}

/// The axis that clears this pair for the least movement, and how much.
///
/// One deficit per axis, computed from the cells rather than shared between
/// them. An earlier draft handed every horizontal axis the same number and
/// then picked the first *strictly* smaller one, so Z was unreachable -- and Z
/// is the axis with the most room, because `starting_layout` lays gates in
/// rows along it by depth.
///
/// Y is charged [`CONDUCTOR_CLEARANCE`] flat rather than the pair's own
/// requirement, because height does not carry the routing reservation. That is
/// the whole reason crowding buys height: a body with nowhere to go sideways
/// has somewhere to go up, and it is cheaper. [`offence`] applies that charge;
/// this only chooses between what it measured.
fn cheapest_axis(found: &Offence, axes: Axes) -> (usize, f64) {
    let mut best = (usize::MAX, f64::INFINITY);
    for axis in axes.iter() {
        if found.deficit[axis] < best.1 {
            best = (axis, found.deficit[axis]);
        }
    }
    best
}

/// One of a body's cells, in world coordinates.
///
/// Recomputed each round rather than cached: a body that turned has moved
/// every cell it owns, and a cached one would be the layout before the turn.
#[derive(Debug, Clone)]
pub struct PlacedCell {
    pub at: [f64; 3],
    pub carries: Vec<String>,
}

/// Where every body's cells are right now.
pub fn placed_cells(graph: &BodyGraph) -> Vec<Vec<PlacedCell>> {
    graph
        .bodies
        .iter()
        .map(|body| {
            cells(body)
                .into_iter()
                .map(|cell| PlacedCell {
                    at: [
                        body.position[0] + cell.offset.0 as f64,
                        body.position[1] + cell.offset.1 as f64,
                        body.position[2] + cell.offset.2 as f64,
                    ],
                    carries: cell.carries,
                })
                .collect()
        })
        .collect()
}

/// Which bodies each body is welded to.
///
/// Built once per pass rather than rediscovered per pair. The pair loop is
/// quadratic in the bodies and a scan of `welds` is linear in the welds, which
/// made the exemption test the product of the two -- once per pair, per round,
/// for [`PROJECTION_ROUNDS`] rounds.
///
/// Each weld is recorded from both ends, so the lookup does not care which of
/// the pair the caller happens to be holding.
fn welded_partners(graph: &BodyGraph) -> Vec<Vec<usize>> {
    let mut partners = vec![Vec::new(); graph.bodies.len()];
    for weld in &graph.welds {
        let (one, other) = match *weld {
            Weld::AtSocket { repeater, junction, .. } => (repeater, junction),
            Weld::BesideAt { lock, data, .. } => (lock, data),
        };
        partners[one].push(other);
        partners[other].push(one);
    }
    partners
}

/// Whether two bodies are allowed to be as close as they are.
///
/// Exempt when a weld relates them -- a welded pair is *required* to touch, so
/// a projection that pushed them apart would fight the thing that holds them
/// together, and the two would take turns undoing each other.
///
/// Not exempt for belonging to the same gate. A gate is exactly the place one
/// net ends and another begins: a torch's support carries the signal driving
/// it and its torch carries the signal it drives, and those are different nets
/// by definition.
fn exempt(welded: &[Vec<usize>], left: usize, right: usize) -> bool {
    welded[left].contains(&right)
}

/// The worst pair still too close, for an error that names something.
pub fn worst_violation(graph: &BodyGraph, required: &[f64]) -> Option<Violation> {
    debug_assert_eq!(
        required.len(),
        graph.bodies.len(),
        "the requirement table is indexed by body"
    );
    let cells = placed_cells(graph);
    let welded = welded_partners(graph);
    let mut worst: Option<Violation> = None;
    for left in 0..graph.bodies.len() {
        for right in (left + 1)..graph.bodies.len() {
            if exempt(&welded, left, right) {
                continue;
            }
            let need = required[left].max(required[right]);
            let short = offence(&cells[left], &cells[right], need).shortfall;
            if short > SETTLED && worst.is_none_or(|current| short > current.shortfall) {
                worst = Some(Violation { left, right, shortfall: short });
            }
        }
    }
    worst
}

/// Separate every violating pair, then re-satisfy every weld, and repeat until
/// neither moves anything.
///
/// Welds last, deliberately: if only one can hold at the end of a round it
/// must be the one whose failure the invariants would not catch as a wrong
/// answer.
pub fn project(graph: &mut BodyGraph, required: &[f64], axes: Axes) -> Result<(), Violation> {
    // Indexed by body below, with nothing in the type tying the two together.
    // A short table would panic on the index; this says which of the two
    // arguments was wrong.
    debug_assert_eq!(
        required.len(),
        graph.bodies.len(),
        "the requirement table is indexed by body"
    );
    // Welds do not change during a projection, so the exemption table is built
    // once for the whole call rather than rescanned per pair.
    let welded = welded_partners(graph);
    for _ in 0..PROJECTION_ROUNDS {
        let mut moved = false;
        // Recomputed once per round, not once per pair. The snapshot is what
        // every *decision* in this round is taken against -- which pairs are
        // violating, by how much, and along which axis -- so no pair is judged
        // against a position an earlier pair has already moved. The moves
        // themselves land live: `separate` reads its direction from
        // `graph.bodies`, so a pair pushed past its neighbour by an earlier
        // move is pushed the other way. The order of the pair loop therefore
        // does change the path this takes. It is the same order every time,
        // which is where determinism comes from -- not from independence.
        let cells = placed_cells(graph);
        for left in 0..graph.bodies.len() {
            for right in (left + 1)..graph.bodies.len() {
                if exempt(&welded, left, right) {
                    continue;
                }
                let need = required[left].max(required[right]);
                let found = offence(&cells[left], &cells[right], need);
                if found.shortfall <= SETTLED {
                    continue;
                }
                let (axis, amount) = cheapest_axis(&found, axes);
                if amount <= SETTLED {
                    continue;
                }
                separate(graph, left, right, axis, amount);
                moved = true;
            }
        }
        // Taken and put back rather than cloned: `satisfy` reads `bodies` and
        // never `welds`, and a clone per round is 4096 allocations of a list
        // that never changes.
        let welds = std::mem::take(&mut graph.welds);
        for weld in &welds {
            moved |= satisfy(graph, weld);
        }
        graph.welds = welds;
        if !moved {
            return Ok(());
        }
    }
    match worst_violation(graph, required) {
        Some(violation) => Err(violation),
        None => Ok(()),
    }
}

/// Move one pair `cost` apart along `axis`.
///
/// It chooses neither. [`cheapest_axis`] picked the axis and measured what
/// clearing this pair along it costs, and that is the only place either
/// decision is made -- including the one that makes stacking cheap, since Y is
/// charged [`CONDUCTOR_CLEARANCE`] flat there and the horizontal axes are
/// charged the pair's full requirement. What is left here is who moves --
/// both by half, or the free one by the whole -- and which way.
fn separate(graph: &mut BodyGraph, left: usize, right: usize, axis: usize, cost: f64) {
    // Which way each goes. Equal positions are a tie, broken by index so the
    // same input always produces the same layout.
    let delta = graph.bodies[left].position[axis] - graph.bodies[right].position[axis];
    let left_goes_negative = if delta == 0.0 { true } else { delta < 0.0 };
    let sign = if left_goes_negative { -1.0 } else { 1.0 };

    match (graph.bodies[left].pinned, graph.bodies[right].pinned) {
        (true, true) => {}
        (true, false) => graph.bodies[right].position[axis] -= sign * cost,
        (false, true) => graph.bodies[left].position[axis] += sign * cost,
        (false, false) => {
            graph.bodies[left].position[axis] += sign * cost / 2.0;
            graph.bodies[right].position[axis] -= sign * cost / 2.0;
        }
    }
}

/// Put a welded body back where its weld says it goes.
///
/// A weld's offset is a function of facing: which cell is "the socket" turns
/// with the junction, which is why this runs after facings are chosen and not
/// before. A body that turned has moved the cell its weld points at, and the
/// weld has to be restored at the facing that will actually be built.
fn satisfy(graph: &mut BodyGraph, weld: &Weld) -> bool {
    let (held, anchor, offset) = match *weld {
        Weld::AtSocket { repeater, junction, input_index } => {
            let facing = graph.bodies[junction].facing;
            let direction = geometry::input_directions(facing)[input_index];
            let step = Position::new(0, 0, 0).offset(direction);
            (repeater, junction, [step.x as f64, step.y as f64, step.z as f64])
        }
        Weld::BesideAt { lock, data, side } => {
            let facing = graph.bodies[data].facing;
            let BodyKind::Primitive { kind, .. } = graph.bodies[data].what else {
                unreachable!("only a primitive has a side")
            };
            let port = crate::compile::physical::variants(kind)[usize::from(facing.index())]
                .ports_of(crate::compile::physical::PortKind::RepeaterSide)
                .find(|port| port.side == Some(side))
                .expect("a repeater variant has both sides");
            (
                lock,
                data,
                [
                    port.position.x as f64,
                    port.position.y as f64,
                    port.position.z as f64,
                ],
            )
        }
    };

    let want = [
        graph.bodies[anchor].position[0] + offset[0],
        graph.bodies[anchor].position[1] + offset[1],
        graph.bodies[anchor].position[2] + offset[2],
    ];
    if graph.bodies[held].position == want {
        return false;
    }
    graph.bodies[held].position = want;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::geometry::CellFacing;
    use crate::compile::relax::build::{Body, BodyGraph, BodyKind, Weld};
    use crate::compile::topology::Primitive;

    /// A one-cell body on its own net, which is the simplest thing the
    /// projection can be asked about.
    fn body(x: f64, y: f64, z: f64) -> Body {
        Body {
            what: BodyKind::Primitive { node: 0, kind: Primitive::Torch },
            position: [x, y, z],
            inputs: vec![format!("net{x}{y}{z}")],
            output: None,
            facing: CellFacing::NORTH,
            pinned: false,
        }
    }

    fn graph_of(bodies: Vec<Body>, welds: Vec<Weld>) -> BodyGraph {
        let count = bodies.len();
        BodyGraph {
            bodies,
            pulls: Vec::new(),
            welds,
            nodes: (0..count).map(|index| vec![index]).collect(),
            anchor_body: (0..count).collect(),
        }
    }

    /// A ring around a body grows as the square root of nothing: `d` lanes at
    /// `ROUTE_PITCH` sit on a perimeter of about `8r`, so `r >= d / 4`.
    #[test]
    fn a_reservation_is_a_quarter_of_the_routes_that_must_reach_it() {
        assert_eq!(reservation(0), 0.0);
        assert_eq!(reservation(4), 1.0);
        assert_eq!(reservation(10), 2.5);
    }

    /// Two bodies on top of each other end up two cells apart, in the plane,
    /// and not one cell further.
    #[test]
    fn two_crowded_bodies_end_up_exactly_far_enough_apart() {
        let mut graph = graph_of(vec![body(0.0, 1.0, 0.0), body(0.2, 1.0, 0.0)], Vec::new());
        let required = vec![3.0, 3.0];
        project(&mut graph, &required, Axes::IN_PLANE).expect("two bodies always fit");

        let gap = (graph.bodies[0].position[0] - graph.bodies[1].position[0]).abs();
        assert!(gap >= 3.0 - 1e-9, "they are still {gap} apart");
        assert!(gap <= 3.0 + 1e-9, "they were pushed to {gap}, further than asked");
    }

    /// Z is reachable. A pair already most of the way apart along Z is
    /// finished off along Z, because that is the cheaper axis -- not shoved
    /// the full requirement along X.
    ///
    /// The axis is chosen from a real per-axis deficit, computed from the
    /// cells. An earlier draft handed every horizontal axis the same number --
    /// the Chebyshev shortfall -- and then took the first *strictly* smaller
    /// one, so X always won and Z was unreachable. That is the axis with the
    /// most room, because `starting_layout` lays gates in rows along it by
    /// depth.
    #[test]
    fn separation_takes_the_axis_that_is_already_nearly_clear() {
        let mut graph = graph_of(vec![body(0.0, 1.0, 0.0), body(0.5, 1.0, 2.5)], Vec::new());
        let required = vec![3.0, 3.0];
        project(&mut graph, &required, Axes::IN_PLANE).expect("two bodies always fit");

        let dz = (graph.bodies[0].position[2] - graph.bodies[1].position[2]).abs();
        assert!(
            (dz - 3.0).abs() < 1e-9,
            "Z was 0.5 short and is the cheap axis; they ended up {dz} apart"
        );
        assert_eq!(
            (graph.bodies[0].position[0], graph.bodies[1].position[0]),
            (0.0, 0.5),
            "X was 2.5 short and nothing should have paid that"
        );
    }

    /// Stage 1 may not spend height. Bodies stay at the Y their starting
    /// layout gave them, so a projection that reaches for the third dimension
    /// here has changed what the stage promised.
    ///
    /// It also has to separate them. "Nobody moved in Y" is true of a
    /// projection that does nothing at all, so the horizontal check is what
    /// makes this test about *in-plane* rather than about *inert*.
    #[test]
    fn in_plane_projection_never_moves_a_body_in_y() {
        let mut graph = graph_of(
            vec![body(0.0, 1.0, 0.0), body(0.1, 1.0, 0.1), body(0.2, 1.0, 0.2)],
            Vec::new(),
        );
        let required = vec![3.0; 3];
        project(&mut graph, &required, Axes::IN_PLANE).expect("three bodies in a plane fit");
        for (index, body) in graph.bodies.iter().enumerate() {
            assert_eq!(body.position[1], 1.0, "body {index} left its storey");
        }
        for left in 0..3 {
            for right in (left + 1)..3 {
                let dx = (graph.bodies[left].position[0] - graph.bodies[right].position[0]).abs();
                let dz = (graph.bodies[left].position[2] - graph.bodies[right].position[2]).abs();
                assert!(
                    dx.max(dz) >= 3.0 - SETTLED,
                    "bodies {left} and {right} started 0.1 apart and are still {} apart",
                    dx.max(dz)
                );
            }
        }
    }

    /// Welds win. A body forced away from something it is welded to ends the
    /// projection welded, and the separation is what is left violated.
    ///
    /// The order matters because a weld violated is a circuit that does not
    /// work, while a separation violated is a circuit that works and is
    /// illegal -- and only the second is something an invariant will catch.
    #[test]
    fn a_weld_survives_a_separation_that_fights_it() {
        // Two welded bodies and a third crowding them, with the separation set
        // so wide that satisfying it would have to break the weld.
        let mut graph = graph_of(
            vec![body(0.0, 1.0, 0.0), body(-1.0, 1.0, 0.0), body(0.4, 1.0, 0.0)],
            vec![Weld::AtSocket { repeater: 1, junction: 0, input_index: 0 }],
        );
        let required = vec![8.0; 3];
        let _ = project(&mut graph, &required, Axes::IN_PLANE);

        let junction = graph.bodies[0].position;
        let repeater = graph.bodies[1].position;
        let offset = [
            repeater[0] - junction[0],
            repeater[1] - junction[1],
            repeater[2] - junction[2],
        ];
        assert_eq!(offset, [-1.0, 0.0, 0.0], "input 0's socket is one cell west");

        // The weld holding is only interesting if something was pulling on it.
        // A projection that moved nothing at all would pass the assertion
        // above, because the fixture starts the repeater at exactly the offset
        // it is asked to end at.
        let crowder = graph.bodies[2].position;
        assert!(
            (crowder[0] - 0.4).abs() > 1.0,
            "the body separation was fighting never moved: it is still at {crowder:?}"
        );
    }

    /// A pinned body takes no force, so everything moves around it.
    #[test]
    fn a_pinned_body_does_not_move() {
        let mut bodies = vec![body(0.0, 1.0, 0.0), body(0.2, 1.0, 0.0)];
        bodies[0].pinned = true;
        let mut graph = graph_of(bodies, Vec::new());
        let required = vec![3.0, 3.0];
        project(&mut graph, &required, Axes::IN_PLANE).expect("one may move");

        assert_eq!(graph.bodies[0].position, [0.0, 1.0, 0.0]);
        assert!((graph.bodies[1].position[0] - 3.0).abs() < 1e-9);
    }

    /// Three bodies that must each touch a fourth and each stay clear of the
    /// others may have no arrangement at all. That is a real outcome, and it
    /// has to be reported rather than looped on for ever.
    #[test]
    fn constraints_that_contradict_are_reported_rather_than_spun_on() {
        let mut graph = graph_of(
            vec![body(0.0, 1.0, 0.0), body(-1.0, 1.0, 0.0), body(1.0, 1.0, 0.0)],
            vec![
                Weld::AtSocket { repeater: 1, junction: 0, input_index: 0 },
                Weld::AtSocket { repeater: 2, junction: 0, input_index: 1 },
            ],
        );
        // Wider than the two welded sockets can ever be from each other.
        let required = vec![9.0; 3];
        let deadlock = project(&mut graph, &required, Axes::IN_PLANE)
            .expect_err("two welds one cell either side cannot also be nine apart");
        assert!(deadlock.shortfall > 0.0);
    }
}
