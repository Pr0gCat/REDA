//! Fabric increment 1: physics kernels.
//!
//! Hand-laid rigs for the geometric facts the fabric plan leans on, each
//! run in the real `Simulator`. Every rig is a set of independently driven
//! nets (a lever plus the cells it should light, in flow order). The one
//! assertion, [`assert_independent`], toggles every lever from every
//! combination of the others, in both polarities, and after each settle
//! demands that **every** net reads exactly what its *own* lever alone
//! predicts -- dust strength cell by cell, repeater and lamp lit state. A
//! foreign net moving by even one strength step is a coupling and fails.
//!
//! [`Net::expected`] is computed from the cell sequence alone (dust -1 per
//! cell, a lit repeater restarts at 15), so the same check also proves each
//! rig is built right: a missing floor, a backwards repeater or a broken
//! step shows up as the net's own reading being wrong.

use reda::redstone::simulator::position::Position;
use reda::redstone::simulator::Simulator;
use reda::redstone::world::block::{BlockKind, BlockState, Facing};
use reda::redstone::world::storage::World;

const MAX_TICKS: u64 = 400;
/// K4: a flat landing with a repeater every this many levels of a ramp.
const LANDING_EVERY: i32 = 12;
const TRACK_LEN: i32 = 20;

fn block(kind: BlockKind, name: &str) -> BlockState {
    let mut b = BlockState::air();
    b.kind = kind;
    b.name = name.to_string();
    b
}

fn stone() -> BlockState {
    block(BlockKind::Solid, "minecraft:stone")
}

fn dust() -> BlockState {
    block(BlockKind::RedstoneWire, "minecraft:redstone_wire")
}

fn lamp() -> BlockState {
    block(BlockKind::Lamp, "minecraft:redstone_lamp")
}

/// Floating lever, same as `tests/simulator_circuits.rs`: nothing under it,
/// so the block it strongly powers is air and the only thing it drives is
/// the first cell of its own net.
fn lever(on: bool) -> BlockState {
    let mut b = block(BlockKind::Lever, "minecraft:lever");
    b.lit = on;
    b
}

/// A repeater carrying signal in `flow`. Minecraft's `facing` points from
/// output to input, so it is `flow.opposite()`.
fn repeater(flow: Facing) -> BlockState {
    let mut b = block(BlockKind::Repeater, "minecraft:repeater");
    b.facing = Some(flow.opposite());
    b.delay = 1;
    b
}

fn put(world: &mut World, p: Position, state: BlockState) {
    world.set(p.x, p.y, p.z, state);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Part {
    Dust,
    Repeater,
    Lamp,
}

/// One independently driven signal path.
struct Net {
    name: String,
    lever: Position,
    /// Every cell the lever lights, in flow order.
    cells: Vec<(Position, Part)>,
}

impl Net {
    fn new(world: &mut World, name: &str, lever_at: Position) -> Net {
        put(world, lever_at, lever(false));
        Net {
            name: name.to_string(),
            lever: lever_at,
            cells: Vec::new(),
        }
    }

    /// Lay `part` at `p` -- dust and repeaters on their own stone floor --
    /// and append it to the net.
    fn lay(&mut self, world: &mut World, p: Position, part: Part, flow: Facing) {
        let state = match part {
            Part::Dust => dust(),
            Part::Repeater => repeater(flow),
            Part::Lamp => lamp(),
        };
        if part != Part::Lamp {
            put(world, p.down(), stone());
        }
        put(world, p, state);
        self.cells.push((p, part));
    }

    /// What each cell must read with the lever `on`: dust strength, or 1/0
    /// for a lit/unlit repeater or lamp. Derived from the sequence alone.
    fn expected(&self, on: bool) -> Vec<u8> {
        let mut signal: u8 = if on { 15 } else { 0 };
        self.cells
            .iter()
            .map(|&(_, part)| match part {
                Part::Dust => {
                    let here = signal;
                    signal = here.saturating_sub(1);
                    here
                }
                Part::Repeater | Part::Lamp => {
                    let lit = signal > 0;
                    signal = if lit { 15 } else { 0 };
                    lit as u8
                }
            })
            .collect()
    }

    fn read(&self, sim: &Simulator) -> Vec<u8> {
        self.cells
            .iter()
            .map(|&(p, part)| {
                let s = sim.world().get(p.x, p.y, p.z);
                match part {
                    Part::Dust => s.power,
                    Part::Repeater | Part::Lamp => s.lit as u8,
                }
            })
            .collect()
    }
}

/// A straight line of `len` cells leaving `lever_at` in `dir`, dust except
/// at the 1-based indices in `repeaters_at`.
fn line(
    world: &mut World,
    name: &str,
    lever_at: Position,
    dir: Facing,
    len: i32,
    repeaters_at: &[i32],
) -> Net {
    let mut net = Net::new(world, name, lever_at);
    let mut p = lever_at;
    for k in 1..=len {
        p = p.offset(dir);
        let part = if repeaters_at.contains(&k) {
            Part::Repeater
        } else {
            Part::Dust
        };
        net.lay(world, p, part, dir);
    }
    net
}

fn set_levers(sim: &mut Simulator, nets: &[Net], state: u32) {
    for (i, net) in nets.iter().enumerate() {
        put(sim.world_mut(), net.lever, lever(state >> i & 1 == 1));
    }
    sim.run_until_stable(MAX_TICKS)
        .expect("rig must settle after a lever change");
}

fn check(rig: &str, sim: &Simulator, nets: &[Net], state: u32, when: &str) {
    let width = nets.len();
    for (i, net) in nets.iter().enumerate() {
        let own = state >> i & 1 == 1;
        let want = net.expected(own);
        let got = net.read(sim);
        if let Some(k) = (0..want.len()).find(|&k| want[k] != got[k]) {
            let (p, part) = net.cells[k];
            panic!(
                "{rig}: {when} (levers {state:0width$b}, lsb = {}): net `{}` cell #{k} {part:?} at \
                 {p:?} reads {} but its own lever is {} so it must read {}",
                nets[0].name,
                net.name,
                got[k],
                if own { "on" } else { "off" },
                want[k],
            );
        }
    }
}

/// Toggle every lever from every combination of the others, both ways, and
/// require every net to read exactly its own lever's prediction throughout.
/// A plain 20-cell dust track's dead tail (cells 16..) is included: it must
/// stay at 0, which makes it the most sensitive leak detector in the rig.
fn assert_independent(rig: &str, world: World, nets: &[Net]) {
    let mut sim = Simulator::new(world);
    for from in 0..1u32 << nets.len() {
        for (flip, flipped) in nets.iter().enumerate() {
            set_levers(&mut sim, nets, from);
            check(rig, &sim, nets, from, "baseline");
            let to = from ^ (1 << flip);
            let verb = if to >> flip & 1 == 1 { "on" } else { "off" };
            set_levers(&mut sim, nets, to);
            check(
                rig,
                &sim,
                nets,
                to,
                &format!("after turning `{}` {verb}", flipped.name),
            );
        }
    }
}

// ---------------------------------------------------------------------
// K1 / K6: two parallel 20-cell tracks along +x
// ---------------------------------------------------------------------

/// Repeater x positions shared by both tracks, so repeaters sit exactly
/// abreast of each other (the worst case for a lateral leak).
const REPEATER_XS: [i32; 3] = [6, 11, 16];

/// Track A along +x at (y=4, z=1); track B the same length offset laterally
/// by (dy, dz), flowing east (`b_east`, "along") or west ("against").
/// `a_reps`/`b_reps` swap three dust cells for repeaters at [`REPEATER_XS`].
fn parallel(dy: i32, dz: i32, a_reps: bool, b_reps: bool, b_east: bool) -> (World, Vec<Net>) {
    const Y: i32 = 4;
    const Z: i32 = 1;
    let mut world = World::new(TRACK_LEN + 2, 9, 6);
    let east_reps: &[i32] = &REPEATER_XS;
    // Flowing west from x = 21, index k sits at x = 21 - k.
    let west_reps: Vec<i32> = REPEATER_XS.iter().map(|x| TRACK_LEN + 1 - x).collect();

    let a = line(
        &mut world,
        "A",
        Position::new(0, Y, Z),
        Facing::East,
        TRACK_LEN,
        if a_reps { east_reps } else { &[] },
    );
    let b = if b_east {
        line(
            &mut world,
            "B",
            Position::new(0, Y + dy, Z + dz),
            Facing::East,
            TRACK_LEN,
            if b_reps { east_reps } else { &[] },
        )
    } else {
        line(
            &mut world,
            "B",
            Position::new(TRACK_LEN + 1, Y + dy, Z + dz),
            Facing::West,
            TRACK_LEN,
            if b_reps { &west_reps } else { &[] },
        )
    };
    (world, vec![a, b])
}

/// Every dust/repeater mix and both flow senses at one lateral offset.
fn all_parallel_variants(kernel: &str, dy: i32, dz: i32) {
    for a_reps in [false, true] {
        for b_reps in [false, true] {
            for b_east in [true, false] {
                let rig = format!(
                    "{kernel} dy={dy} dz={dz} A={} B={} {}",
                    if a_reps { "repeaters" } else { "dust" },
                    if b_reps { "repeaters" } else { "dust" },
                    if b_east { "along" } else { "against" },
                );
                let (world, nets) = parallel(dy, dz, a_reps, b_reps, b_east);
                assert_independent(&rig, world, &nets);
            }
        }
    }
}

/// K1: every lateral offset with L1 = 3 -- flat pitch 3 (dz=3), the
/// diagonals (dy=±1,dz=2 and dy=±2,dz=1) and straight above (dy=3, B's
/// floor at y+2 leaving air at y+1) -- for dust/dust, dust/repeater,
/// repeater/dust and repeater/repeater, with the tracks flowing along and
/// against each other. Nothing couples.
#[test]
fn k1_parallel_tracks_at_l1_three_do_not_couple() {
    for (dy, dz) in [(0, 3), (1, 2), (-1, 2), (2, 1), (-2, 1), (3, 0)] {
        all_parallel_variants("K1", dy, dz);
    }
}

// ---------------------------------------------------------------------
// K3 / K6: perpendicular crossing
// ---------------------------------------------------------------------

/// Lower track along +x at y=2, z=10; upper track along +z at y=2+dy on its
/// own stone floor (y=1+dy), x=10. They cross at (10, *, 10); the crossing
/// cell of either may be a repeater.
fn crossing(dy: i32, lower_rep: bool, upper_rep: bool) -> (World, Vec<Net>) {
    const C: i32 = 10;
    const Y: i32 = 2;
    let mut world = World::new(TRACK_LEN + 2, Y + dy + 2, TRACK_LEN + 2);
    let lower = line(
        &mut world,
        "lower",
        Position::new(0, Y, C),
        Facing::East,
        TRACK_LEN,
        if lower_rep { &[C] } else { &[] },
    );
    let upper = line(
        &mut world,
        "upper",
        Position::new(C, Y + dy, 0),
        Facing::South,
        TRACK_LEN,
        if upper_rep { &[C] } else { &[] },
    );
    (world, vec![lower, upper])
}

fn all_crossing_variants(kernel: &str, dy: i32) {
    for lower_rep in [false, true] {
        for upper_rep in [false, true] {
            let rig =
                format!("{kernel} crossing dy={dy} lower_rep={lower_rep} upper_rep={upper_rep}");
            let (world, nets) = crossing(dy, lower_rep, upper_rep);
            assert_independent(&rig, world, &nets);
        }
    }
}

/// K3: dust at y, a perpendicular track at y+3 on its floor at y+2, air at
/// y+1 over the crossing. Inert with dust or a repeater on either crossing
/// cell.
#[test]
fn k3_dy_three_crossing_is_inert() {
    all_crossing_variants("K3", 3);
}

// ---------------------------------------------------------------------
// K4: dust staircases
// ---------------------------------------------------------------------

/// A single-row straight dust staircase along +x at depth `z`, `levels`
/// levels tall, one cell sideways per level, every dust on its own stone.
///
/// Up: lever -> repeater -> flat dust at y=1, then climb; the output is the
/// top dust at y=1+levels. Down: the same from y=1+levels descending to
/// y=1, then repeater -> lamp as the output. Every [`LANDING_EVERY`] levels
/// (when more follow) a 3-cell flat landing: the arriving dust, a repeater
/// pointing along the stairs, and a departing dust.
fn ramp(world: &mut World, name: &str, z: i32, levels: i32, up: bool) -> Net {
    let east = Facing::East;
    let (start_y, dy) = if up { (1, 1) } else { (levels + 1, -1) };
    let mut p = Position::new(0, start_y, z);
    let mut net = Net::new(world, name, p);
    p = p.offset(east);
    net.lay(world, p, Part::Repeater, east);
    p = p.offset(east);
    net.lay(world, p, Part::Dust, east);
    for level in 1..=levels {
        p = Position::new(p.x + 1, p.y + dy, p.z);
        net.lay(world, p, Part::Dust, east);
        if level % LANDING_EVERY == 0 && level < levels {
            p = p.offset(east);
            net.lay(world, p, Part::Repeater, east);
            p = p.offset(east);
            net.lay(world, p, Part::Dust, east);
        }
    }
    if !up {
        p = p.offset(east);
        net.lay(world, p, Part::Repeater, east);
        p = p.offset(east);
        net.lay(world, p, Part::Lamp, east);
    }
    net
}

/// Wide enough for the longest ramp: lever, feed, flat, levels, two cells
/// per landing, and the down ramp's repeater + lamp.
fn ramp_world(max_levels: i32, depth: i32) -> World {
    let landings = (max_levels - 1) / LANDING_EVERY;
    World::new(2 + max_levels + 2 * landings + 3, max_levels + 3, depth)
}

/// K4: up h levels the top dust is powered with the lever on and dark with
/// it off; down h levels the bottom repeater lights a lamp. `expected`
/// pins every step's strength, so h=12 (top at 3, no landing), h=13 (one
/// landing, top back at 14) and h=40 (three landings, top at 11) are
/// exact, not merely "> 0".
#[test]
fn k4_dust_ramp_with_landings_carries_up_and_down() {
    for levels in [1, 5, 12, 13, 24, 40] {
        for up in [true, false] {
            let mut world = ramp_world(levels, 3);
            let net = ramp(&mut world, if up { "up" } else { "down" }, 1, levels, up);
            let top = *net.expected(true).last().unwrap();
            assert!(top > 0, "h={levels}: design must keep the signal alive");
            let rig = format!("K4 h={levels} {}", if up { "up" } else { "down" });
            assert_independent(&rig, world, &[net]);
        }
    }
}

/// K4b: two staircases 3 apart in z (pitch 3), different heights -- and one
/// pair going opposite ways -- toggled independently. Nothing couples.
#[test]
fn k4b_adjacent_ramps_at_pitch_three_do_not_couple() {
    for (a, a_up, b, b_up) in [
        (5, true, 12, true),
        (13, true, 24, true),
        (12, true, 40, true),
        (13, true, 24, false),
        (24, false, 40, false),
    ] {
        let mut world = ramp_world(a.max(b), 6);
        let na = ramp(&mut world, "A", 1, a, a_up);
        let nb = ramp(&mut world, "B", 4, b, b_up);
        let rig = format!(
            "K4b A=h{a}{} B=h{b}{}",
            if a_up { "up" } else { "down" },
            if b_up { "up" } else { "down" }
        );
        assert_independent(&rig, world, &[na, nb]);
    }
}

// ---------------------------------------------------------------------
// K6: dy=2 stack
// ---------------------------------------------------------------------

/// K6: upper dust at y+2 on a stone floor at y+1 that sits **directly on
/// top of** the lower dust at y -- both as a parallel stack (every variant
/// of K1) and as a perpendicular crossing.
///
/// Result: **it does not couple.** The upper dust only weakly powers its
/// floor, weak power never re-drives dust, and dust never powers the block
/// above it, so the shared floor carries nothing either way.
#[test]
fn k6_dy_two_stack_behaviour() {
    all_parallel_variants("K6", 2, 0);
    all_crossing_variants("K6", 2);
}

/// Control: the detector is not blind. Two dust tracks side by side (L1 = 1)
/// merge into one net, so turning A on must show up as B reading non-zero.
#[test]
#[should_panic(expected = "so it must read")]
fn control_adjacent_tracks_are_reported_as_coupled() {
    let (world, nets) = parallel(0, 1, false, false, true);
    assert_independent("control dz=1", world, &nets);
}
