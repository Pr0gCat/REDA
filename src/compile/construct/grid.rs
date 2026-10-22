//! The occupancy grid every construction step asks, and the one local
//! legality rule it enforces.
//!
//! The rule is deliberately coarse: two blocks with different owners never
//! stand within one cell of each other (Chebyshev distance, all 26
//! neighbours). With that gap no dust of one owner can connect to, power, or
//! be powered by anything of another, so electrical isolation between nets
//! never has to be reasoned about cell by cell. It costs space, which the
//! generator is allowed to spend.
//!
//! A reserved air cell is owned but inert: nothing may be placed in it, and
//! it does not count toward anyone's gap.
//!
//! A *clear* cell is the opposite: nothing occupies it, but it keeps the gap
//! as if its owner's block stood there. A tile marks the cell its output
//! leaves through and the cell each input stub is entered from, so nothing
//! placed later can wall a connection in before it is routed.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::compile::geometry::Anchor;
use crate::redstone::world::block::BlockState;

/// A multiply-rotate hasher for small integer keys: the grid and the router
/// hash millions of anchors, and the default SipHash dominates their cost.
#[derive(Default)]
pub struct CellHasher(u64);

impl Hasher for CellHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_i32(&mut self, value: i32) {
        self.write_u64(value as u32 as u64);
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub type CellMap<K, V> = HashMap<K, V, BuildHasherDefault<CellHasher>>;
pub type CellSet<K> = std::collections::HashSet<K, BuildHasherDefault<CellHasher>>;

/// Who a cell belongs to. A gate owns its body; a net owns its source port,
/// every route cell, every floor under them and each sink stub it ends in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Owner {
    Net(usize),
    Gate(usize),
}

#[derive(Debug, Clone)]
struct Slot {
    owner: Owner,
    /// `None` is reserved air.
    block: Option<BlockState>,
    /// The route branch that placed this cell, if a route did.
    branch: Option<usize>,
}

#[derive(Default, Clone)]
pub struct Grid {
    slots: CellMap<Anchor, Slot>,
    /// What every cell the searches ask about holds, in arrays: who has
    /// taken it and who stands near it.
    dense: Dense,
    /// Every clear cell, with how many times each owner keeps it.
    clear: CellMap<Anchor, Vec<(Owner, u32)>>,
    journal: Vec<Entry>,
    /// Levels routes may climb above the usual ceiling, to reach ports
    /// placed high or to pass over blocks.
    pub raise: i32,
    /// Levels the preferred travel levels sit above the usual ones: a
    /// parent's routes travel over its blocks, whose own routes keep below.
    pub lift: i32,
}

/// An owner as a nonzero number, so zero can mean nobody.
fn code(owner: Owner) -> u32 {
    match owner {
        Owner::Net(net) => 2 * net as u32 + 2,
        Owner::Gate(gate) => 2 * gate as u32 + 3,
    }
}

/// Marks a taken cell that is reserved air.
const AIR: u32 = 1 << 31;

/// Who stands within one cell of a cell: one owner and how many of its
/// blocks, and how many blocks everyone else has. `mine` is zero only when
/// `others` is too, so an owner is always named when anyone is near.
#[derive(Clone, Copy, Default)]
struct Near {
    owner: u32,
    mine: u32,
    others: u32,
}

impl Near {
    fn add(&mut self, owner: u32) {
        if self.mine == 0 {
            self.owner = owner;
            self.mine = 1;
        } else if self.owner == owner {
            self.mine += 1;
        } else {
            self.others += 1;
        }
    }

    /// Take one of `owner`'s out; `true` when the named owner has none
    /// left but others do, and the cell has to be counted again.
    fn remove(&mut self, owner: u32) -> bool {
        if self.owner == owner {
            self.mine -= 1;
            self.mine == 0 && self.others > 0
        } else {
            self.others -= 1;
            false
        }
    }

    fn foreign(&self, owner: u32) -> bool {
        self.others > 0 || (self.mine > 0 && self.owner != owner)
    }
}

#[derive(Clone, Copy, Default)]
struct Cell {
    /// Zero when free; else the owner's code, with [`AIR`] for reserved air.
    taken: u32,
    near: Near,
    near_clear: Near,
}

const SHIFT: i32 = 3;
const SIDE: i32 = 1 << SHIFT;
const CHUNK: usize = (SIDE * SIDE * SIDE) as usize;

/// Cells in chunks of `SIDE` cubed, over a box of chunks that grows to
/// take whatever is written; a cell never written reads as default.
#[derive(Clone, Default)]
struct Dense {
    low: [i32; 3],
    size: [i32; 3],
    chunks: Vec<Option<Box<[Cell; CHUNK]>>>,
}

impl Dense {
    fn split(at: Anchor) -> ([i32; 3], usize) {
        let chunk = [at.x >> SHIFT, at.y >> SHIFT, at.z >> SHIFT];
        let mask = SIDE - 1;
        let inner = ((at.y & mask) * SIDE + (at.z & mask)) * SIDE + (at.x & mask);
        (chunk, inner as usize)
    }

    fn index(&self, chunk: [i32; 3]) -> Option<usize> {
        let mut index = 0;
        for ((chunk, low), size) in chunk.into_iter().zip(self.low).zip(self.size) {
            let offset = chunk - low;
            if !(0..size).contains(&offset) {
                return None;
            }
            index = index * size as usize + offset as usize;
        }
        Some(index)
    }

    fn get(&self, at: Anchor) -> Cell {
        let (chunk, inner) = Dense::split(at);
        self.index(chunk)
            .and_then(|index| self.chunks[index].as_ref())
            .map_or_else(Cell::default, |cells| cells[inner])
    }

    fn get_mut(&mut self, at: Anchor) -> &mut Cell {
        let (chunk, inner) = Dense::split(at);
        let index = match self.index(chunk) {
            Some(index) => index,
            None => {
                self.grow(chunk);
                self.index(chunk).unwrap()
            }
        };
        &mut self.chunks[index].get_or_insert_with(|| Box::new([Cell::default(); CHUNK]))[inner]
    }

    /// Widen the box to take `chunk`, with room to spare.
    fn grow(&mut self, chunk: [i32; 3]) {
        let (mut low, mut high) = ([0; 3], [0; 3]);
        for axis in 0..3 {
            if self.size[axis] == 0 {
                low[axis] = chunk[axis] - 2;
                high[axis] = chunk[axis] + 3;
            } else {
                let (old_low, old_high) = (self.low[axis], self.low[axis] + self.size[axis]);
                let spare = self.size[axis] / 2 + 1;
                low[axis] = if chunk[axis] < old_low { chunk[axis] - spare } else { old_low };
                high[axis] = if chunk[axis] >= old_high { chunk[axis] + 1 + spare } else { old_high };
            }
        }
        let size = [high[0] - low[0], high[1] - low[1], high[2] - low[2]];
        let mut chunks = Vec::new();
        chunks.resize_with((size[0] * size[1] * size[2]) as usize, || None);
        let old = std::mem::take(&mut self.chunks);
        let (old_low, old_size) = (self.low, self.size);
        self.low = low;
        self.size = size;
        for (index, cells) in old.into_iter().enumerate() {
            if cells.is_some() {
                let [_, height, depth] = old_size.map(|side| side as usize);
                let at = [index / depth / height, index / depth % height, index % depth];
                let at = [0, 1, 2].map(|axis| old_low[axis] + at[axis] as i32);
                chunks[self.index(at).unwrap()] = cells;
            }
        }
        self.chunks = chunks;
    }
}

fn add(counts: &mut Vec<(Owner, u32)>, owner: Owner) {
    match counts.iter_mut().find(|(other, _)| *other == owner) {
        Some((_, count)) => *count += 1,
        None => counts.push((owner, 1)),
    }
}

/// Take one of `owner`'s counts out; `true` when the list is left empty.
fn remove(counts: &mut Vec<(Owner, u32)>, owner: Owner) -> bool {
    let index = counts
        .iter()
        .position(|(other, _)| *other == owner)
        .unwrap();
    counts[index].1 -= 1;
    if counts[index].1 == 0 {
        counts.swap_remove(index);
    }
    counts.is_empty()
}

#[derive(Clone)]
enum Entry {
    Slot(Anchor),
    Removed(Anchor, Slot),
    Clear(Anchor, Owner),
    Released(Anchor, Owner),
}

pub fn around(at: Anchor) -> impl Iterator<Item = Anchor> {
    (-1..=1).flat_map(move |dx| {
        (-1..=1).flat_map(move |dy| {
            (-1..=1).map(move |dz| Anchor {
                x: at.x + dx,
                y: at.y + dy,
                z: at.z + dz,
            })
        })
    })
}

/// Who stands within one cell of a cell, as seen by one owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nearby {
    Nobody,
    /// Only the owner's own blocks.
    Own,
    /// Somebody else's.
    Foreign,
}

/// Everything a route search asks about one cell, read in one go.
#[derive(Clone, Copy)]
pub struct Look(Cell);

impl Look {
    pub fn is_free(&self) -> bool {
        self.0.taken == 0
    }

    /// Whose blocks stand within one cell, as [`Grid::foreign_near`] and
    /// [`Grid::own_near`] would say.
    pub fn near(&self, owner: Owner) -> Nearby {
        if self.0.near.foreign(code(owner)) {
            Nearby::Foreign
        } else if self.0.near.mine > 0 {
            Nearby::Own
        } else {
            Nearby::Nobody
        }
    }

    /// [`Grid::clear_near`].
    pub fn clear_near(&self, owner: Owner) -> bool {
        self.0.near_clear.foreign(code(owner))
    }
}

impl Grid {
    pub fn look(&self, at: Anchor) -> Look {
        Look(self.dense.get(at))
    }

    pub fn is_free(&self, at: Anchor) -> bool {
        self.dense.get(at).taken == 0
    }

    /// Whether a block of `owner` at `at` would stand within one cell of a
    /// block somebody else owns.
    pub fn foreign_near(&self, at: Anchor, owner: Owner) -> bool {
        self.dense.get(at).near.foreign(code(owner))
    }

    /// Whether `owner` already has a block within one cell of `at`; a
    /// cheap filter before [`Grid::own_blocks_near`].
    pub fn own_near(&self, at: Anchor, owner: Owner) -> bool {
        let near = self.dense.get(at).near;
        (near.mine > 0 && near.owner == code(owner))
            || (near.others > 0 && self.own_blocks_near(at, owner).next().is_some())
    }

    /// `owner`'s blocks within one cell of `at`.
    pub fn own_blocks_near(&self, at: Anchor, owner: Owner) -> impl Iterator<Item = Anchor> + '_ {
        let code = code(owner);
        around(at).filter(move |&cell| self.dense.get(cell).taken == code)
    }

    /// Whether anything not `owner`'s stands within `radius` cells of `at`
    /// horizontally and one cell vertically.
    pub fn foreign_within(&self, at: Anchor, owner: Owner, radius: i32) -> bool {
        let code = code(owner);
        (-radius..=radius).any(|dx| {
            (-1..=1).any(|dy| {
                (-radius..=radius).any(|dz| {
                    let taken = self
                        .dense
                        .get(Anchor {
                            x: at.x + dx,
                            y: at.y + dy,
                            z: at.z + dz,
                        })
                        .taken;
                    taken != 0 && taken & AIR == 0 && taken != code
                })
            })
        })
    }

    /// Place without checking; callers check first.
    pub fn place(&mut self, at: Anchor, owner: Owner, block: Option<BlockState>) {
        self.place_for(at, owner, block, None);
    }

    /// [`Grid::place`], for route branch `branch`.
    pub fn place_for(
        &mut self,
        at: Anchor,
        owner: Owner,
        block: Option<BlockState>,
        branch: Option<usize>,
    ) {
        debug_assert!(self.is_free(at), "{at:?} is already taken");
        self.put(at, owner, block.is_some());
        self.slots.insert(
            at,
            Slot {
                owner,
                block,
                branch,
            },
        );
        self.journal.push(Entry::Slot(at));
    }

    /// Take a placed cell out again (ripping up a route).
    pub fn remove(&mut self, at: Anchor) {
        let slot = self.slots.remove(&at).expect("removing a placed cell");
        self.take(at, slot.owner, slot.block.is_some());
        self.journal.push(Entry::Removed(at, slot));
    }

    /// The route branch that placed the block at `at`.
    pub fn branch_at(&self, at: Anchor) -> Option<usize> {
        self.slots.get(&at).and_then(|slot| slot.branch)
    }

    /// Keep everyone but `owner` a cell away from `at`: tiles strictly,
    /// routes at a price (see [`Grid::clear_near`]).
    pub fn keep_clear(&mut self, at: Anchor, owner: Owner) {
        self.clear_cell(at, owner);
        self.journal.push(Entry::Clear(at, owner));
    }

    /// Drop a clear cell once the connection it protected is made.
    pub fn release(&mut self, at: Anchor, owner: Owner) {
        self.release_cell(at, owner);
        self.journal.push(Entry::Released(at, owner));
    }

    /// Whether a clear cell of somebody else's lies within one cell of `at`.
    pub fn clear_near(&self, at: Anchor, owner: Owner) -> bool {
        self.dense.get(at).near_clear.foreign(code(owner))
    }

    /// Whether `owner` holds a clear cell within one cell of `at`.
    pub fn own_clear_near(&self, at: Anchor, owner: Owner) -> bool {
        let near = self.dense.get(at).near_clear;
        (near.mine > 0 && near.owner == code(owner))
            || (near.others > 0
                && around(at).any(|cell| {
                    self.clear
                        .get(&cell)
                        .is_some_and(|counts| counts.iter().any(|&(other, _)| other == owner))
                }))
    }

    /// Mark `at` taken, and count a block there in every cell near it.
    fn put(&mut self, at: Anchor, owner: Owner, block: bool) {
        let code = code(owner);
        self.dense.get_mut(at).taken = if block { code } else { code | AIR };
        if block {
            for cell in around(at) {
                self.dense.get_mut(cell).near.add(code);
            }
        }
    }

    /// Undo [`Grid::put`].
    fn take(&mut self, at: Anchor, owner: Owner, block: bool) {
        let code = code(owner);
        self.dense.get_mut(at).taken = 0;
        if block {
            for cell in around(at) {
                if self.dense.get_mut(cell).near.remove(code) {
                    let near = self.recount(cell, |grid, at| {
                        let taken = grid.dense.get(at).taken;
                        (taken != 0 && taken & AIR == 0).then_some((taken, 1)).into_iter().collect()
                    });
                    self.dense.get_mut(cell).near = near;
                }
            }
        }
    }

    fn clear_cell(&mut self, at: Anchor, owner: Owner) {
        add(self.clear.entry(at).or_default(), owner);
        let code = code(owner);
        for cell in around(at) {
            self.dense.get_mut(cell).near_clear.add(code);
        }
    }

    fn release_cell(&mut self, at: Anchor, owner: Owner) {
        if remove(self.clear.get_mut(&at).unwrap(), owner) {
            self.clear.remove(&at);
        }
        let mine = code(owner);
        for cell in around(at) {
            if self.dense.get_mut(cell).near_clear.remove(mine) {
                let near = self.recount(cell, |grid, at| {
                    grid.clear
                        .get(&at)
                        .map(|counts| counts.iter().map(|&(owner, count)| (code(owner), count)).collect())
                        .unwrap_or_default()
                });
                self.dense.get_mut(cell).near_clear = near;
            }
        }
    }

    /// Count again who stands within one cell of `at`, from what each cell
    /// near it holds.
    fn recount(&self, at: Anchor, holds: impl Fn(&Grid, Anchor) -> Vec<(u32, u32)>) -> Near {
        let mut near = Near::default();
        for cell in around(at) {
            for (owner, count) in holds(self, cell) {
                for _ in 0..count {
                    near.add(owner);
                }
            }
        }
        near
    }

    pub fn mark(&self) -> usize {
        self.journal.len()
    }

    /// Take back everything placed since `mark`.
    pub fn undo(&mut self, mark: usize) {
        while self.journal.len() > mark {
            match self.journal.pop().unwrap() {
                Entry::Slot(at) => {
                    let slot = self.slots.remove(&at).unwrap();
                    self.take(at, slot.owner, slot.block.is_some());
                }
                Entry::Removed(at, slot) => {
                    self.put(at, slot.owner, slot.block.is_some());
                    self.slots.insert(at, slot);
                }
                Entry::Clear(at, owner) => self.release_cell(at, owner),
                Entry::Released(at, owner) => self.clear_cell(at, owner),
            }
        }
    }

    /// Every occupied cell, reserved air included (`None`).
    pub fn cells(&self) -> impl Iterator<Item = (Anchor, Owner, Option<&BlockState>)> {
        self.slots.iter().map(|(at, slot)| (*at, slot.owner, slot.block.as_ref()))
    }

    pub fn blocks(&self) -> impl Iterator<Item = (Anchor, Owner, &BlockState)> {
        self.slots
            .iter()
            .filter_map(|(at, slot)| slot.block.as_ref().map(|block| (*at, slot.owner, block)))
    }

    /// The horizontal extent of everything placed, as `(min, max)`.
    pub fn extent(&self) -> Option<(Anchor, Anchor)> {
        let mut cells = self.slots.keys();
        let first = *cells.next()?;
        Some(cells.fold((first, first), |(low, high), at| {
            (
                Anchor {
                    x: low.x.min(at.x),
                    y: low.y.min(at.y),
                    z: low.z.min(at.z),
                },
                Anchor {
                    x: high.x.max(at.x),
                    y: high.y.max(at.y),
                    z: high.z.max(at.z),
                },
            )
        }))
    }
}
