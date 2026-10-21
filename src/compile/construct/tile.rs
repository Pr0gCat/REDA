//! The fixed block patterns one netlist element becomes.
//!
//! Every tile is written facing north (signals leave toward -z) and is
//! never turned; only a block's ports come in one per edge. Inputs arrive from the west, east and south. Each input is
//! a *stub* the tile already contains -- a repeater against the body and one
//! dust cell behind it, both owned by the input's net -- so a route only has
//! to reach that dust cell with any strength above zero. The repeater gives
//! every input the same terminal whatever shape the route arrives in.

use super::placement::Side;
use crate::compile::geometry::Anchor;
use crate::compile::{dust, repeater, stone};
use crate::redstone::world::block::{BlockKind, BlockState, Facing};

/// Whose cell this is, relative to the tile's own netlist element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The element's body: a NOR's support, a DFF's storage.
    Body,
    /// The `n`-th net the element drives (gates drive one).
    Output(usize),
    /// The net feeding the element's `n`-th input.
    Input(usize),
}

#[derive(Clone)]
pub struct TileCell {
    pub offset: (i32, i32, i32),
    pub role: Role,
    /// `None` keeps the cell as air.
    pub block: Option<BlockState>,
}

/// Where a route may join one of a tile's output nets, and with what
/// strength.
#[derive(Clone)]
pub struct SourceCell {
    pub output: usize,
    pub offset: (i32, i32, i32),
    pub strength: u8,
    /// Whether a route may branch from this cell (never from a repeater or a
    /// junction whose every side is spoken for).
    pub tappable: bool,
}

/// The dust cell a route into input `input` must reach, and every cell of
/// its stub (which a route arriving there may stand beside).
#[derive(Clone)]
pub struct Stub {
    pub input: usize,
    pub approach: (i32, i32, i32),
    /// The cells beyond the approach -- its entry and a ramp up from it --
    /// kept clear until routed.
    pub entry: Vec<(i32, i32, i32)>,
    pub cells: Vec<(i32, i32, i32)>,
}

#[derive(Clone)]
pub struct Tile {
    pub cells: Vec<TileCell>,
    /// For each output, the cells its net keeps clear until every sink it
    /// drives is connected: the way out, and a ramp up from it.
    pub exits: Vec<Vec<(i32, i32, i32)>>,
    pub source: Vec<SourceCell>,
    pub stubs: Vec<Stub>,
    /// The cell a test reads this element's own value from.
    pub probe: Option<(i32, i32, i32)>,
    /// A block's: where each port's value can be read, inputs then
    /// outputs.
    pub ports: Vec<(i32, i32, i32)>,
}

/// How many cells north of its source a tile's output runs flat, and how
/// many levels the ramp after that climbs: enough to clear every tile, so an
/// unconnected output can always reach the open layers above.
const EXIT: i32 = 1;
const RAMP: i32 = 4;

/// The clear cells for an output leaving north from `(x, y, z)`.
fn exit(from: (i32, i32, i32)) -> Vec<(i32, i32, i32)> {
    exit_toward(from, (0, 0, -1), 1)
}

/// The clear cells for an output leaving `from` one `step` at a time,
/// then ramping up (`slope` 1) or down (-1).
fn exit_toward((x, y, z): (i32, i32, i32), (dx, _, dz): (i32, i32, i32), slope: i32) -> Vec<(i32, i32, i32)> {
    let flat = (1..=EXIT).map(move |ahead| (x + dx * ahead, y, z + dz * ahead));
    let ramp = (1..=RAMP).map(move |rise| (x + dx * (EXIT + rise), y + slope * rise, z + dz * (EXIT + rise)));
    corridor(flat.chain(ramp))
}

/// A stub's entry and the ramp away from it, up (`slope` 1) or down (-1).
fn entry_ramp(entry: (i32, i32, i32), back: (i32, i32, i32), slope: i32) -> Vec<(i32, i32, i32)> {
    corridor(std::iter::once(entry).chain((1..=RAMP).map(|rise| {
        (
            entry.0 + back.0 * rise,
            entry.1 + slope * rise,
            entry.2 + back.2 * rise,
        )
    })))
}

/// The cells a route along `path` needs kept clear: each cell, the floor it
/// will stand on, and the air above it a stair needs.
fn corridor(path: impl Iterator<Item = (i32, i32, i32)>) -> Vec<(i32, i32, i32)> {
    let mut cells: Vec<(i32, i32, i32)> = path
        .flat_map(|(x, y, z)| [(x, y - 1, z), (x, y, z), (x, y + 1, z)])
        .collect();
    cells.sort_unstable();
    cells.dedup();
    cells
}

/// The horizontal extent of everything a tile holds -- its blocks and the
/// cells it keeps clear -- as `(min x, max x, min z, max z)` from its origin.
pub fn extent(tile: &Tile) -> (i32, i32, i32, i32) {
    let offsets = tile
        .cells
        .iter()
        .map(|cell| cell.offset)
        .chain(tile.exits.iter().flatten().copied())
        .chain(
            tile.stubs
                .iter()
                .flat_map(|stub| stub.entry.iter().copied()),
        );
    offsets.fold(
        (i32::MAX, i32::MIN, i32::MAX, i32::MIN),
        |(a, b, c, d), (x, _, z)| (a.min(x), b.max(x), c.min(z), d.max(z)),
    )
}

pub fn at(origin: Anchor, offset: (i32, i32, i32)) -> Anchor {
    Anchor {
        x: origin.x + offset.0,
        y: origin.y + offset.1,
        z: origin.z + offset.2,
    }
}

fn wall_torch(facing: Facing) -> BlockState {
    let mut state = BlockState::air();
    state.kind = BlockKind::WallTorch;
    state.name = "minecraft:redstone_wall_torch".to_string();
    state.facing = Some(facing);
    state.lit = true;
    state
}

fn cell(offset: (i32, i32, i32), role: Role, block: Option<BlockState>) -> TileCell {
    TileCell {
        offset,
        role,
        block,
    }
}

/// A block standing on a stone floor.
fn standing(cells: &mut Vec<TileCell>, offset: (i32, i32, i32), role: Role, block: BlockState) {
    cells.push(cell(
        (offset.0, offset.1 - 1, offset.2),
        role,
        Some(stone()),
    ));
    cells.push(cell(offset, role, Some(block)));
}

/// The three sides inputs arrive on, as (socket, direction the signal
/// travels into the body). The approach dust is one further out.
const SIDES: [((i32, i32, i32), Facing); 3] = [
    ((-1, 0, 0), Facing::East),
    ((1, 0, 0), Facing::West),
    ((0, 0, 1), Facing::North),
];

/// A repeater at `socket` aimed `toward` the body, fed from dust behind it.
fn stub(cells: &mut Vec<TileCell>, input: usize, socket: (i32, i32, i32), toward: Facing) -> Stub {
    sloped_stub(cells, input, socket, toward, 1)
}

/// [`stub`], its way in ramping up (`slope` 1) or down (-1).
fn sloped_stub(cells: &mut Vec<TileCell>, input: usize, socket: (i32, i32, i32), toward: Facing, slope: i32) -> Stub {
    let back = match toward {
        Facing::East => (-1, 0, 0),
        Facing::West => (1, 0, 0),
        Facing::North => (0, 0, 1),
        Facing::South => (0, 0, -1),
        _ => unreachable!("inputs arrive horizontally"),
    };
    let approach = (socket.0 + back.0, socket.1, socket.2 + back.2);
    let entry = (approach.0 + back.0, approach.1, approach.2 + back.2);
    standing(cells, socket, Role::Input(input), repeater(toward));
    standing(cells, approach, Role::Input(input), dust());
    let below = |(x, y, z): (i32, i32, i32)| (x, y - 1, z);
    Stub {
        input,
        approach,
        entry: entry_ramp(entry, back, slope),
        cells: vec![socket, below(socket), approach, below(approach)],
    }
}

/// NOR of `arity` inputs: a torch hung on the north face of a stone
/// support, each input a repeater into the support.
pub fn nor(arity: usize) -> Tile {
    assert!(
        arity <= 3,
        "a NOR has at most three sides to take inputs on"
    );
    let mut cells = vec![
        cell((0, 0, 0), Role::Body, Some(stone())),
        cell((0, 1, 0), Role::Body, None),
        cell((0, -1, 0), Role::Body, None),
        cell((0, 0, -1), Role::Output(0), Some(wall_torch(Facing::North))),
        cell((0, 1, -1), Role::Output(0), None),
    ];
    standing(&mut cells, (0, 0, -2), Role::Output(0), dust());
    let stubs: Vec<Stub> = (0..arity)
        .map(|input| stub(&mut cells, input, SIDES[input].0, SIDES[input].1))
        .collect();
    Tile {
        cells,
        exits: vec![exit((0, 0, -2))],
        source: vec![SourceCell {
            output: 0,
            offset: (0, 0, -2),
            strength: 15,
            tappable: true,
        }],
        stubs,
        probe: Some((0, 0, -1)),
        ports: Vec::new(),
    }
}

/// OR of `arity` inputs: each input a repeater into one junction dust.
pub fn or(arity: usize) -> Tile {
    assert!((2..=3).contains(&arity), "an OR merges two or three inputs");
    let mut cells = Vec::new();
    standing(&mut cells, (0, 0, 0), Role::Output(0), dust());
    standing(&mut cells, (0, 0, -1), Role::Output(0), dust());
    let stubs: Vec<Stub> = (0..arity)
        .map(|input| stub(&mut cells, input, SIDES[input].0, SIDES[input].1))
        .collect();
    Tile {
        cells,
        exits: vec![exit((0, 0, -1))],
        source: vec![
            SourceCell {
                output: 0,
                offset: (0, 0, 0),
                strength: 15,
                tappable: false,
            },
            SourceCell {
                output: 0,
                offset: (0, 0, -1),
                strength: 14,
                tappable: true,
            },
        ],
        stubs,
        probe: Some((0, 0, -1)),
        ports: Vec::new(),
    }
}

/// The DFF's internal components, relative to its probe (the slave data
/// repeater, lit exactly when Q is high), by role: master and slave data
/// repeaters, their locks, and the inverted clock torch.
pub const DFF_SITES: [(&str, (i32, i32, i32)); 5] = [
    ("MData", (0, 0, 1)),
    ("SData", (0, 0, 0)),
    ("MLock", (1, 0, 1)),
    ("SLock", (1, 0, 0)),
    ("InvC", (2, 0, 0)),
];

/// Where the DFF tile's components sit, relative to its origin.
pub const DFF_M_DATA: (i32, i32, i32) = (0, 0, 0);
pub const DFF_S_DATA: (i32, i32, i32) = (0, 0, -1);
pub const DFF_M_LOCK: (i32, i32, i32) = (1, 0, 0);
pub const DFF_S_LOCK: (i32, i32, i32) = (1, 0, -1);
pub const DFF_INVERTED_CLOCK: (i32, i32, i32) = (2, 0, -1);

/// The Design H positive-edge DFF: master and slave data repeaters locked
/// by the clock and its complement. D is input 0, C input 1.
pub fn dff() -> Tile {
    let mut cells = Vec::new();
    standing(&mut cells, DFF_M_DATA, Role::Body, repeater(Facing::North));
    standing(&mut cells, DFF_S_DATA, Role::Body, repeater(Facing::North));
    standing(&mut cells, DFF_M_LOCK, Role::Body, repeater(Facing::West));
    standing(&mut cells, DFF_S_LOCK, Role::Body, repeater(Facing::West));
    cells.push(cell((2, 0, 0), Role::Body, Some(stone())));
    cells.push(cell((2, 1, 0), Role::Body, None));
    cells.push(cell((2, -1, 0), Role::Body, None));
    cells.push(cell(
        DFF_INVERTED_CLOCK,
        Role::Body,
        Some(wall_torch(Facing::North)),
    ));
    cells.push(cell((2, 1, -1), Role::Body, None));
    standing(&mut cells, (0, 0, -2), Role::Output(0), dust());
    let stubs = vec![
        stub(&mut cells, 0, (0, 0, 1), Facing::North),
        stub(&mut cells, 1, (3, 0, 0), Facing::West),
    ];
    Tile {
        cells,
        exits: vec![exit((0, 0, -2))],
        source: vec![SourceCell {
            output: 0,
            offset: (0, 0, -2),
            strength: 15,
            tappable: true,
        }],
        stubs,
        probe: Some((0, 0, -1)),
        ports: Vec::new(),
    }
}

/// One step outward across `edge`, and the way a signal travels coming in
/// across it.
pub fn across(edge: Side) -> ((i32, i32, i32), Facing) {
    match edge {
        Side::West => ((-1, 0, 0), Facing::East),
        Side::East => ((1, 0, 0), Facing::West),
        Side::North => ((0, 0, -1), Facing::South),
        Side::South => ((0, 0, 1), Facing::North),
    }
}

/// The way a signal travels going out across `edge`.
fn outward(edge: Side) -> Facing {
    match edge {
        Side::West => Facing::West,
        Side::East => Facing::East,
        Side::North => Facing::North,
        Side::South => Facing::South,
    }
}

/// An input port facing `edge`: a stub from outside through a repeater
/// into the dust that starts the circuit's own net, which leaves inward. The
/// repeater keeps the two nets apart; the stub's way in stays clear for
/// whatever drives it. A block's ports stand `on_top`, above its own routes:
/// the way in climbs to its parent's, the way on descends to its own.
pub fn port_in(edge: Side, on_top: bool) -> Tile {
    let (out, inward) = across(edge);
    let next = (-out.0, 0, -out.2);
    let mut cells = Vec::new();
    let stubs = vec![stub(&mut cells, 0, out, inward)];
    standing(&mut cells, (0, 0, 0), Role::Output(0), dust());
    standing(&mut cells, next, Role::Output(0), dust());
    Tile {
        cells,
        exits: vec![exit_toward(next, next, if on_top { -1 } else { 1 })],
        source: vec![
            SourceCell {
                output: 0,
                offset: (0, 0, 0),
                strength: 15,
                tappable: false,
            },
            SourceCell {
                output: 0,
                offset: next,
                strength: 14,
                tappable: true,
            },
        ],
        stubs,
        probe: Some((0, 0, 0)),
        ports: Vec::new(),
    }
}

/// An output port facing `edge`: the circuit's net reaches a stub from
/// inside, whose repeater drives the dust the outside taps, with the way out
/// kept clear. `on_top` as for [`port_in`].
pub fn port_out(edge: Side, on_top: bool) -> Tile {
    let (out, _) = across(edge);
    let mut cells = Vec::new();
    let slope = if on_top { -1 } else { 1 };
    let stubs = vec![sloped_stub(&mut cells, 0, (-out.0, 0, -out.2), outward(edge), slope)];
    standing(&mut cells, (0, 0, 0), Role::Output(0), dust());
    standing(&mut cells, out, Role::Output(0), dust());
    Tile {
        cells,
        exits: vec![exit_toward(out, out, 1)],
        source: vec![
            SourceCell {
                output: 0,
                offset: (0, 0, 0),
                strength: 15,
                tappable: false,
            },
            SourceCell {
                output: 0,
                offset: out,
                strength: 14,
                tappable: true,
            },
        ],
        stubs,
        probe: Some((0, 0, 0)),
        ports: Vec::new(),
    }
}
