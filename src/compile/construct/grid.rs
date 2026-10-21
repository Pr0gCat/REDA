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
    /// For every cell, how many blocks of each owner stand within one cell
    /// of it (itself included).
    near: CellMap<Anchor, Vec<(Owner, u32)>>,
    /// The same for clear cells.
    near_clear: CellMap<Anchor, Vec<(Owner, u32)>>,
    journal: Vec<Entry>,
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

/// Count `owner` in (or out of) every cell within one of `at`.
fn tally(map: &mut CellMap<Anchor, Vec<(Owner, u32)>>, at: Anchor, owner: Owner, adding: bool) {
    for cell in around(at) {
        if adding {
            add(map.entry(cell).or_default(), owner);
        } else if remove(map.get_mut(&cell).unwrap(), owner) {
            map.remove(&cell);
        }
    }
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

impl Grid {
    pub fn is_free(&self, at: Anchor) -> bool {
        !self.slots.contains_key(&at)
    }

    /// Whether a block of `owner` at `at` would stand within one cell of a
    /// block somebody else owns.
    pub fn foreign_near(&self, at: Anchor, owner: Owner) -> bool {
        self.near
            .get(&at)
            .is_some_and(|counts| counts.iter().any(|&(other, _)| other != owner))
    }

    /// Whether `owner` already has a block (or a clear cell) within one
    /// cell of `at`; a cheap filter before [`Grid::own_blocks_near`].
    pub fn own_near(&self, at: Anchor, owner: Owner) -> bool {
        self.near
            .get(&at)
            .is_some_and(|counts| counts.iter().any(|&(other, _)| other == owner))
    }

    /// `owner`'s blocks within one cell of `at`.
    pub fn own_blocks_near(&self, at: Anchor, owner: Owner) -> impl Iterator<Item = Anchor> + '_ {
        around(at).filter(move |cell| {
            self.slots
                .get(cell)
                .is_some_and(|slot| slot.owner == owner && slot.block.is_some())
        })
    }

    /// Whether anything not `owner`'s stands within `radius` cells of `at`
    /// horizontally and one cell vertically.
    pub fn foreign_within(&self, at: Anchor, owner: Owner, radius: i32) -> bool {
        (-radius..=radius).any(|dx| {
            (-1..=1).any(|dy| {
                (-radius..=radius).any(|dz| {
                    self.slots
                        .get(&Anchor {
                            x: at.x + dx,
                            y: at.y + dy,
                            z: at.z + dz,
                        })
                        .is_some_and(|slot| slot.owner != owner && slot.block.is_some())
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
        if block.is_some() {
            self.count(at, owner);
        }
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
        if slot.block.is_some() {
            self.uncount(at, slot.owner);
        }
        self.journal.push(Entry::Removed(at, slot));
    }

    /// The route branch that placed the block at `at`.
    pub fn branch_at(&self, at: Anchor) -> Option<usize> {
        self.slots.get(&at).and_then(|slot| slot.branch)
    }

    /// Keep everyone but `owner` a cell away from `at`: tiles strictly,
    /// routes at a price (see [`Grid::clear_near`]).
    pub fn keep_clear(&mut self, at: Anchor, owner: Owner) {
        tally(&mut self.near_clear, at, owner, true);
        self.journal.push(Entry::Clear(at, owner));
    }

    /// Drop a clear cell once the connection it protected is made.
    pub fn release(&mut self, at: Anchor, owner: Owner) {
        tally(&mut self.near_clear, at, owner, false);
        self.journal.push(Entry::Released(at, owner));
    }

    /// Whether a clear cell of somebody else's lies within one cell of `at`.
    pub fn clear_near(&self, at: Anchor, owner: Owner) -> bool {
        self.near_clear
            .get(&at)
            .is_some_and(|counts| counts.iter().any(|&(other, _)| other != owner))
    }

    /// Whether `owner` holds a clear cell within one cell of `at`.
    pub fn own_clear_near(&self, at: Anchor, owner: Owner) -> bool {
        self.near_clear
            .get(&at)
            .is_some_and(|counts| counts.iter().any(|&(other, _)| other == owner))
    }

    fn count(&mut self, at: Anchor, owner: Owner) {
        tally(&mut self.near, at, owner, true);
    }

    fn uncount(&mut self, at: Anchor, owner: Owner) {
        tally(&mut self.near, at, owner, false);
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
                    if slot.block.is_some() {
                        self.uncount(at, slot.owner);
                    }
                }
                Entry::Removed(at, slot) => {
                    if slot.block.is_some() {
                        self.count(at, slot.owner);
                    }
                    self.slots.insert(at, slot);
                }
                Entry::Clear(at, owner) => tally(&mut self.near_clear, at, owner, false),
                Entry::Released(at, owner) => tally(&mut self.near_clear, at, owner, true),
            }
        }
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
